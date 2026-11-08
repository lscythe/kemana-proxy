use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use kemana_proxy::adapter::GtfsScheduleAdapter;
use kemana_proxy::bundle::{BuildOutcome, BundleStore, build_bundle};
use kemana_proxy::http::{SHUTDOWN_DRAIN_LIMIT, router, serve as serve_http};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Normalizes transit data and serves it to client apps"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Build an offline SQLite bundle from a GTFS Schedule zip
    BuildBundle {
        /// Prefix for every id in the bundle, such as `demo`
        #[arg(long, value_parser = operator_id)]
        operator: String,
        /// Path to the GTFS Schedule zip
        #[arg(long)]
        gtfs: PathBuf,
        /// Directory that holds the bundles and the manifest
        #[arg(long)]
        out: PathBuf,
    },
    /// Serve the bundles in a directory over HTTP
    Serve {
        /// Directory written by `build-bundle`
        #[arg(long)]
        bundles: PathBuf,
        #[arg(long, default_value = "127.0.0.1:8080")]
        addr: SocketAddr,
    },
}

/// Operator ids prefix every entity id as `<operator_id>:<source_id>`, so
/// they must not contain the separator.
fn operator_id(value: &str) -> Result<String, String> {
    let valid = !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
        });
    if valid {
        Ok(value.to_owned())
    } else {
        Err("use lowercase letters, digits, `-` or `_`".to_owned())
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Command::BuildBundle {
            operator,
            gtfs,
            out,
        } => build(operator, gtfs, out),
        Command::Serve { bundles, addr } => serve(bundles, addr).await,
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn build(operator: String, gtfs: PathBuf, out: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let report = build_bundle(&GtfsScheduleAdapter::new(operator), &gtfs, &out)?;
    let status = match report.outcome {
        BuildOutcome::Built => "built",
        BuildOutcome::Unchanged => "unchanged",
    };
    println!("{status} {}", report.manifest.version);
    Ok(())
}

async fn serve(bundles: PathBuf, addr: SocketAddr) -> Result<(), Box<dyn std::error::Error>> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let shutdown = shutdown_signal()?;
    println!(
        "serving {} on http://{}",
        bundles.display(),
        listener.local_addr()?
    );
    serve_http(
        listener,
        router(BundleStore::new(bundles)),
        shutdown,
        SHUTDOWN_DRAIN_LIMIT,
    )
    .await?;
    Ok(())
}

/// Resolves on ctrl-c, or on SIGTERM where the platform has it. The SIGTERM
/// handler is installed before this returns, so a SIGTERM sent right after the
/// startup banner is never lost.
fn shutdown_signal() -> std::io::Result<impl Future<Output = ()> + Send + 'static> {
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = Box::pin(tokio::signal::ctrl_c());
    Ok(async move {
        #[cfg(unix)]
        let terminated = async {
            terminate.recv().await;
        };
        #[cfg(not(unix))]
        let terminated = std::future::pending::<()>();
        tokio::select! {
            _ = &mut interrupt => {}
            () = terminated => {}
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_line_is_well_formed() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    #[test]
    fn build_bundle_takes_an_operator_a_feed_and_an_output_directory() {
        let cli = Cli::try_parse_from([
            "kemana-proxy",
            "build-bundle",
            "--operator",
            "demo",
            "--gtfs",
            "feed.zip",
            "--out",
            "bundles",
        ])
        .unwrap();

        let Command::BuildBundle {
            operator,
            gtfs,
            out,
        } = cli.command
        else {
            panic!("expected build-bundle");
        };
        assert_eq!(operator, "demo");
        assert_eq!(gtfs, PathBuf::from("feed.zip"));
        assert_eq!(out, PathBuf::from("bundles"));
    }

    #[test]
    fn serve_listens_on_localhost_by_default() {
        let cli = Cli::try_parse_from(["kemana-proxy", "serve", "--bundles", "bundles"]).unwrap();

        let Command::Serve { addr, .. } = cli.command else {
            panic!("expected serve");
        };
        assert_eq!(addr, "127.0.0.1:8080".parse().unwrap());
    }

    #[test]
    fn operator_ids_cannot_carry_the_namespace_separator() {
        assert_eq!(operator_id("demo-2_b"), Ok("demo-2_b".to_owned()));
        for value in ["", "demo:x", "Demo", "de mo"] {
            assert!(operator_id(value).is_err(), "{value:?}");
        }
    }
}

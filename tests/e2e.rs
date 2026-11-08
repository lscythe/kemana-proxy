//! Builds a bundle from the demo GTFS feed with the real binary, serves it,
//! and downloads it over HTTP the way a client app would. Run it with
//! `mise run e2e`; it is ignored by the default test run.

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use kemana_proxy::bundle::{BundleManifest, SCHEMA_VERSION};
use prost::Message;
use reqwest::StatusCode;
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;

const BINARY: &str = env!("CARGO_BIN_EXE_kemana-proxy");
const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/demo-gtfs");

struct RunningServer {
    child: Child,
    base_url: String,
}

impl RunningServer {
    fn start(bundles: &Path) -> Self {
        let mut child = Command::new(BINARY)
            .args(["serve", "--addr", "127.0.0.1:0", "--bundles"])
            .arg(bundles)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut banner = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut banner)
            .unwrap();
        let base_url = banner
            .split_whitespace()
            .last()
            .unwrap_or_else(|| panic!("no address in {banner:?}"))
            .to_owned();
        Self { child, base_url }
    }

    async fn get(&self, path: &str) -> reqwest::Response {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("{}{path}", self.base_url))
            .send()
            .await
            .unwrap()
    }
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn zip_fixture(destination: &Path) -> PathBuf {
    let path = destination.join("demo-gtfs.zip");
    let mut writer = zip::ZipWriter::new(File::create(&path).unwrap());
    let mut entries: Vec<_> = fs::read_dir(FIXTURE)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    entries.sort();
    for entry in entries {
        let name = entry.file_name().unwrap().to_string_lossy().into_owned();
        writer
            .start_file(name, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(&fs::read(&entry).unwrap()).unwrap();
    }
    writer.finish().unwrap();
    path
}

fn build_bundle(gtfs: &Path, out: &Path) -> String {
    let output = Command::new(BINARY)
        .args(["build-bundle", "--operator", "demo", "--gtfs"])
        .arg(gtfs)
        .arg("--out")
        .arg(out)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn fixture_rows(file: &str) -> i64 {
    let rows = fs::read_to_string(Path::new(FIXTURE).join(file))
        .unwrap()
        .lines()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .count();
    i64::try_from(rows).unwrap()
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn single<T: rusqlite::types::FromSql>(connection: &Connection, sql: &str) -> T {
    connection.query_row(sql, [], |row| row.get(0)).unwrap()
}

#[tokio::test]
#[ignore = "end-to-end: builds the binary's bundle and serves it on localhost; run with `mise run e2e`"]
async fn a_client_downloads_the_bundle_built_from_a_gtfs_feed() {
    let workspace = tempfile::tempdir().unwrap();
    let gtfs = zip_fixture(workspace.path());
    let bundles = workspace.path().join("bundles");

    let first = build_bundle(&gtfs, &bundles);
    let second = build_bundle(&gtfs, &bundles);
    assert!(first.starts_with("built "), "{first}");
    assert_eq!(second, first.replacen("built", "unchanged", 1));

    let server = RunningServer::start(&bundles);
    let response = server.get("/v1/bundle/manifest").await;
    assert_eq!(response.status(), StatusCode::OK);
    let manifest = BundleManifest::decode(response.bytes().await.unwrap()).unwrap();
    assert_eq!(first.trim(), format!("built {}", manifest.version));
    assert_eq!(manifest.schema_version, SCHEMA_VERSION);
    assert!(
        manifest.version.starts_with(&format!("{SCHEMA_VERSION}-")),
        "{}",
        manifest.version
    );
    assert_eq!(
        manifest.download_path,
        format!("/v1/bundle/{}", manifest.version)
    );

    let response = server.get(&manifest.download_path).await;
    assert_eq!(response.status(), StatusCode::OK);
    let compressed = response.bytes().await.unwrap();
    assert_eq!(compressed.len() as u64, manifest.size_bytes);
    assert_eq!(sha256_hex(&compressed), manifest.sha256);

    let unknown = server
        .get(&format!("/v1/bundle/{SCHEMA_VERSION}-ffffffffffffffff"))
        .await;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

    let mut database = Vec::new();
    zstd::Decoder::new(Cursor::new(compressed))
        .unwrap()
        .read_to_end(&mut database)
        .unwrap();
    let database_path = workspace.path().join("downloaded.sqlite");
    fs::write(&database_path, database).unwrap();
    let connection = Connection::open(&database_path).unwrap();

    for (table, file) in [
        ("stops", "stops.txt"),
        ("routes", "routes.txt"),
        ("trips", "trips.txt"),
        ("stop_times", "stop_times.txt"),
        ("shapes", "shapes.txt"),
        ("frequencies", "frequencies.txt"),
        ("fares", "fare_attributes.txt"),
        ("transfers", "transfers.txt"),
    ] {
        let rows: i64 = single(&connection, &format!("SELECT count(*) FROM {table}"));
        assert_eq!(rows, fixture_rows(file), "{table}");
    }
    let foreign_key_violations: i64 =
        single(&connection, "SELECT count(*) FROM pragma_foreign_key_check");
    assert_eq!(foreign_key_violations, 0);
    let source_sha256: String = single(&connection, "SELECT source_sha256 FROM bundle_meta");
    assert_eq!(source_sha256, sha256_hex(&fs::read(&gtfs).unwrap()));
    let operator: String = single(&connection, "SELECT name FROM operators WHERE id = 'demo'");
    assert_eq!(operator, "Demo Transit");
    let parent: String = single(
        &connection,
        "SELECT parent_station FROM stops WHERE id = 'demo:CEN-P1'",
    );
    assert_eq!(parent, "demo:CEN");

    let departures: Vec<(String, u32)> = connection
        .prepare(
            "SELECT trip_id, departure_time FROM stop_times \
             WHERE stop_id = 'demo:MKT' AND departure_time >= ?1 ORDER BY departure_time",
        )
        .unwrap()
        .query_map([12 * 3600], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        departures,
        [
            ("demo:R1-PM".to_owned(), 62_250),
            ("demo:R2-NIGHT".to_owned(), 86_730)
        ]
    );
}

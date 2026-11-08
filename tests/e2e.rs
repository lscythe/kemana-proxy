//! Builds a bundle from the demo GTFS feed with the real binary, serves it,
//! and downloads it over HTTP the way a client app would. Run it with
//! `mise run e2e`; it is ignored by the default test run.

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use kemana_proxy::bundle::{BundleManifest, SCHEMA_VERSION};
use prost::Message;
use reqwest::StatusCode;
use reqwest::header::{CACHE_CONTROL, CONTENT_RANGE, ETAG, IF_NONE_MATCH};
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

    async fn get_if_none_match(&self, path: &str, etag: &str) -> reqwest::Response {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("{}{path}", self.base_url))
            .header(IF_NONE_MATCH, etag)
            .send()
            .await
            .unwrap()
    }

    async fn get_with_headers(&self, path: &str, headers: &[(&str, &str)]) -> reqwest::Response {
        let mut request = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("{}{path}", self.base_url));
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        request.send().await.unwrap()
    }

    /// Uses the `kill` utility so the test needs no signal crate.
    #[cfg(unix)]
    fn terminate(&mut self) -> std::process::ExitStatus {
        let signalled = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status()
            .unwrap();
        assert!(signalled.success());
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "the server ignored SIGTERM");
            std::thread::sleep(Duration::from_millis(20));
        }
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
    let etag = response.headers()[ETAG].to_str().unwrap().to_owned();
    assert_eq!(etag, format!("\"{}\"", manifest.version));
    assert_eq!(
        response.headers()[CACHE_CONTROL],
        "public, max-age=31536000, immutable"
    );
    let compressed = response.bytes().await.unwrap();
    assert_eq!(compressed.len() as u64, manifest.size_bytes);
    assert_eq!(sha256_hex(&compressed), manifest.sha256);

    let revalidated = server
        .get_if_none_match(&manifest.download_path, &etag)
        .await;
    assert_eq!(revalidated.status(), StatusCode::NOT_MODIFIED);
    assert!(revalidated.bytes().await.unwrap().is_empty());

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
        ("service_exceptions", "calendar_dates.txt"),
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

    assert_service_calendars_reached_the_bundle(&connection);
    assert_blank_stop_times_were_filled_in(&connection);

    #[cfg(unix)]
    {
        let mut server = server;
        let status = server.terminate();
        assert!(status.success(), "the server exited with {status}");
    }
}

#[tokio::test]
#[ignore = "end-to-end: builds the binary's bundle and serves it on localhost; run with `mise run e2e`"]
async fn a_client_resumes_an_interrupted_download_with_a_range_request() {
    let workspace = tempfile::tempdir().unwrap();
    let gtfs = zip_fixture(workspace.path());
    let bundles = workspace.path().join("bundles");
    build_bundle(&gtfs, &bundles);
    let server = RunningServer::start(&bundles);
    let manifest = BundleManifest::decode(
        server
            .get("/v1/bundle/manifest")
            .await
            .bytes()
            .await
            .unwrap(),
    )
    .unwrap();
    let total = usize::try_from(manifest.size_bytes).unwrap();
    let received = total / 3;
    assert!(received > 0, "the demo bundle is too small to split");

    let first_part = server
        .get_with_headers(
            &manifest.download_path,
            &[("range", &format!("bytes=0-{}", received - 1))],
        )
        .await;
    assert_eq!(first_part.status(), StatusCode::PARTIAL_CONTENT);
    let etag = first_part.headers()[ETAG].to_str().unwrap().to_owned();
    assert_eq!(etag, format!("\"{}\"", manifest.version));
    assert_eq!(
        first_part.headers()[CONTENT_RANGE],
        format!("bytes 0-{}/{total}", received - 1).as_str()
    );
    let mut downloaded = first_part.bytes().await.unwrap().to_vec();
    assert_eq!(downloaded.len(), received);

    let remainder = server
        .get_with_headers(
            &manifest.download_path,
            &[
                ("range", &format!("bytes={}-", downloaded.len())),
                ("if-range", &etag),
            ],
        )
        .await;
    assert_eq!(remainder.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        remainder.headers()[CONTENT_RANGE],
        format!("bytes {received}-{}/{total}", total - 1).as_str()
    );
    downloaded.extend_from_slice(&remainder.bytes().await.unwrap());

    assert_eq!(downloaded.len() as u64, manifest.size_bytes);
    assert_eq!(sha256_hex(&downloaded), manifest.sha256);

    let past_the_end = server
        .get_with_headers(
            &manifest.download_path,
            &[("range", &format!("bytes={total}-"))],
        )
        .await;
    assert_eq!(past_the_end.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        past_the_end.headers()[CONTENT_RANGE],
        format!("bytes */{total}").as_str()
    );
}

fn assert_service_calendars_reached_the_bundle(connection: &Connection) {
    let services: i64 = single(connection, "SELECT count(*) FROM services");
    assert_eq!(services, fixture_rows("calendar.txt") + 1);

    let weekly: (u8, u8, u32, u32) = connection
        .query_row(
            "SELECT monday, sunday, start_date, end_date FROM services WHERE id = 'demo:WEEKDAY'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(weekly, (1, 0, 20_260_101, 20_261_231));

    let exceptions_only: (u8, u8, Option<u32>, Option<u32>) = connection
        .query_row(
            "SELECT monday, sunday, start_date, end_date FROM services WHERE id = 'demo:EVENT'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(exceptions_only, (0, 0, None, None));

    let exceptions: Vec<(String, u32, u8)> = connection
        .prepare("SELECT service_id, date, exception_type FROM service_exceptions ORDER BY date")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        exceptions,
        [
            ("demo:WEEKDAY".to_owned(), 20_260_703, 2),
            ("demo:WEEKDAY".to_owned(), 20_260_705, 1),
            ("demo:EVENT".to_owned(), 20_260_815, 1),
        ]
    );

    let event_trip_service: String = single(
        connection,
        "SELECT service_id FROM trips WHERE id = 'demo:R1-EVENT'",
    );
    assert_eq!(event_trip_service, "demo:EVENT");
}

fn assert_blank_stop_times_were_filled_in(connection: &Connection) {
    let times: Vec<(u32, u32)> = connection
        .prepare(
            "SELECT arrival_time, departure_time FROM stop_times \
             WHERE trip_id = 'demo:R1-EVENT' ORDER BY stop_sequence",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    assert_eq!(
        times,
        [
            (32_400, 32_400),
            (32_996, 32_996),
            (33_598, 33_598),
            (34_200, 34_200),
        ]
    );
}

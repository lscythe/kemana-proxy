use std::future::{self, IntoFuture};
use std::io::{self, ErrorKind, SeekFrom};
use std::path::Path;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_util::io::ReaderStream;

use crate::bundle::BundleStore;
use range::RangeRequest;

mod range;

/// How long `serve` waits for in-flight requests after a shutdown signal.
pub const SHUTDOWN_DRAIN_LIMIT: Duration = Duration::from_secs(30);

const PROTOBUF: &str = "application/x-protobuf";
const ZSTD: &str = "application/zstd";
// A bundle's bytes never change for a given version, so clients may keep it forever.
const BUNDLE_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";
const MANIFEST_CACHE_CONTROL: &str = "no-cache";
const BYTE_RANGES: &str = "bytes";
const STREAM_CHUNK_BYTES: usize = 64 * 1024;

pub fn router(store: BundleStore) -> Router {
    Router::new()
        .route("/v1/bundle/manifest", get(manifest))
        .route("/v1/bundle/{version}", get(bundle))
        .with_state(store)
}

/// Serves `app` until `shutdown` resolves, then lets in-flight requests finish
/// for at most `drain_limit`, so a slow client cannot hold the process open.
pub async fn serve(
    listener: TcpListener,
    app: Router,
    shutdown: impl Future<Output = ()> + Send + 'static,
    drain_limit: Duration,
) -> io::Result<()> {
    let (signalled, shutdown_signalled) = oneshot::channel();
    let draining = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown.await;
            let _ = signalled.send(());
        })
        .into_future();
    let drain_deadline = async move {
        if shutdown_signalled.await.is_ok() {
            tokio::time::sleep(drain_limit).await;
        } else {
            future::pending::<()>().await;
        }
    };
    tokio::select! {
        result = draining => result,
        () = drain_deadline => Ok(()),
    }
}

async fn manifest(State(store): State<BundleStore>, request_headers: HeaderMap) -> Response {
    let path = store.manifest_path();
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(error) => return read_failure(&path, &error),
    };
    let etag = format!("\"{}\"", sha256_hex(&bytes));
    if matches_if_none_match(&request_headers, &etag) {
        return not_modified(&etag, MANIFEST_CACHE_CONTROL);
    }
    ok(
        PROTOBUF,
        &etag,
        MANIFEST_CACHE_CONTROL,
        bytes.len() as u64,
        Body::from(bytes),
    )
}

/// Checks run in the order RFC 9110 gives: existence, `If-None-Match`, then `Range`.
async fn bundle(
    State(store): State<BundleStore>,
    UrlPath(version): UrlPath<String>,
    method: Method,
    request_headers: HeaderMap,
) -> Response {
    let Some(path) = store.bundle_path(&version) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut file = match tokio::fs::File::open(&path).await {
        Ok(file) => file,
        Err(error) => return read_failure(&path, &error),
    };
    let length = match file.metadata().await {
        Ok(metadata) => metadata.len(),
        Err(error) => return read_failure(&path, &error),
    };
    let etag = format!("\"{version}\"");
    if matches_if_none_match(&request_headers, &etag) {
        return not_modified(&etag, BUNDLE_CACHE_CONTROL);
    }
    let requested = if method == Method::GET {
        requested_range(&request_headers, &etag, length)
    } else {
        RangeRequest::Ignore
    };
    match requested {
        RangeRequest::Ignore => {
            let body = Body::from_stream(ReaderStream::with_capacity(file, STREAM_CHUNK_BYTES));
            bundle_response(StatusCode::OK, &etag, length, None, body)
        }
        RangeRequest::Unsatisfiable => range_not_satisfiable(length),
        RangeRequest::Satisfiable(span) => {
            if let Err(error) = file.seek(SeekFrom::Start(span.first)).await {
                return read_failure(&path, &error);
            }
            let body = Body::from_stream(ReaderStream::with_capacity(
                file.take(span.len()),
                STREAM_CHUNK_BYTES,
            ));
            let content_range = format!("bytes {}-{}/{length}", span.first, span.last);
            bundle_response(
                StatusCode::PARTIAL_CONTENT,
                &etag,
                span.len(),
                Some(&content_range),
                body,
            )
        }
    }
}

fn requested_range(request_headers: &HeaderMap, etag: &str, length: u64) -> RangeRequest {
    let Some(header) = request_headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
    else {
        return RangeRequest::Ignore;
    };
    if !if_range_allows_range(request_headers, etag) {
        return RangeRequest::Ignore;
    }
    range::parse(header, length)
}

/// `If-Range` needs a strong comparison, so a weak tag or an HTTP date, which
/// this server never issues, cannot match and the client restarts from byte 0.
fn if_range_allows_range(request_headers: &HeaderMap, etag: &str) -> bool {
    request_headers
        .get_all(header::IF_RANGE)
        .iter()
        .all(|value| value.to_str().is_ok_and(|candidate| candidate == etag))
}

fn bundle_response(
    status: StatusCode,
    etag: &str,
    content_length: u64,
    content_range: Option<&str>,
    body: Body,
) -> Response {
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, ZSTD)
        .header(header::CONTENT_LENGTH, content_length)
        .header(header::ETAG, etag)
        .header(header::CACHE_CONTROL, BUNDLE_CACHE_CONTROL)
        .header(header::ACCEPT_RANGES, BYTE_RANGES);
    if let Some(content_range) = content_range {
        builder = builder.header(header::CONTENT_RANGE, content_range);
    }
    builder
        .body(body)
        .unwrap_or_else(|error| internal_error(&error))
}

/// No `Cache-Control`: a client must not keep the error as if it were the bundle.
fn range_not_satisfiable(length: u64) -> Response {
    Response::builder()
        .status(StatusCode::RANGE_NOT_SATISFIABLE)
        .header(header::CONTENT_RANGE, format!("bytes */{length}"))
        .header(header::ACCEPT_RANGES, BYTE_RANGES)
        .body(Body::empty())
        .unwrap_or_else(|error| internal_error(&error))
}

fn ok(
    content_type: &str,
    etag: &str,
    cache_control: &str,
    content_length: u64,
    body: Body,
) -> Response {
    Response::builder()
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, content_length)
        .header(header::ETAG, etag)
        .header(header::CACHE_CONTROL, cache_control)
        .body(body)
        .unwrap_or_else(|error| internal_error(&error))
}

fn not_modified(etag: &str, cache_control: &str) -> Response {
    Response::builder()
        .status(StatusCode::NOT_MODIFIED)
        .header(header::ETAG, etag)
        .header(header::CACHE_CONTROL, cache_control)
        .body(Body::empty())
        .unwrap_or_else(|error| internal_error(&error))
}

/// Weak comparison, as RFC 9110 requires for `If-None-Match`.
fn matches_if_none_match(request_headers: &HeaderMap, etag: &str) -> bool {
    request_headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .any(|candidate| {
            candidate == "*" || candidate.strip_prefix("W/").unwrap_or(candidate) == etag
        })
}

fn read_failure(path: &Path, error: &io::Error) -> Response {
    if error.kind() == ErrorKind::NotFound {
        return StatusCode::NOT_FOUND.into_response();
    }
    eprintln!("cannot read {}: {error}", path.display());
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

fn internal_error(error: &dyn std::fmt::Display) -> Response {
    eprintln!("cannot build response: {error}");
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use prost::Message;
    use reqwest::StatusCode;
    use reqwest::header::{
        ACCEPT_RANGES, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG,
        IF_NONE_MATCH,
    };
    use sha2::{Digest, Sha256};
    use tokio::sync::Notify;

    use super::*;
    use crate::bundle::BundleManifest;

    const VERSION: &str = "1-0123456789abcdef";
    const BUNDLE_BYTES: &[u8] = b"compressed bundle bytes";
    const BUNDLE_ETAG: &str = "\"1-0123456789abcdef\"";
    const IMMUTABLE: &str = "public, max-age=31536000, immutable";

    struct Server {
        _dir: tempfile::TempDir,
        base_url: String,
    }

    impl Server {
        async fn start(with_bundle: bool) -> Self {
            if with_bundle {
                Self::start_with_bundle(BUNDLE_BYTES).await
            } else {
                Self::start_with_files(None).await
            }
        }

        async fn start_with_bundle(bundle: &[u8]) -> Self {
            Self::start_with_files(Some(bundle)).await
        }

        async fn start_with_files(bundle: Option<&[u8]>) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let store = BundleStore::new(dir.path());
            if let Some(bundle) = bundle {
                std::fs::write(store.manifest_path(), manifest().encode_to_vec()).unwrap();
                std::fs::write(store.bundle_path(VERSION).unwrap(), bundle).unwrap();
            }
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base_url = format!("http://{}", listener.local_addr().unwrap());
            tokio::spawn(async move { axum::serve(listener, router(store)).await.unwrap() });
            Self {
                _dir: dir,
                base_url,
            }
        }

        fn client() -> reqwest::Client {
            reqwest::Client::builder().no_proxy().build().unwrap()
        }

        async fn get(&self, path: &str) -> reqwest::Response {
            Self::client()
                .get(format!("{}{path}", self.base_url))
                .send()
                .await
                .unwrap()
        }

        async fn get_if_none_match(&self, path: &str, tags: &str) -> reqwest::Response {
            Self::client()
                .get(format!("{}{path}", self.base_url))
                .header(IF_NONE_MATCH, tags)
                .send()
                .await
                .unwrap()
        }

        async fn head(&self, path: &str) -> reqwest::Response {
            Self::client()
                .head(format!("{}{path}", self.base_url))
                .send()
                .await
                .unwrap()
        }

        async fn get_with_headers(
            &self,
            path: &str,
            headers: &[(&str, &str)],
        ) -> reqwest::Response {
            let mut request = Self::client().get(format!("{}{path}", self.base_url));
            for (name, value) in headers {
                request = request.header(*name, *value);
            }
            request.send().await.unwrap()
        }

        async fn get_range(&self, path: &str, range: &str) -> reqwest::Response {
            self.get_with_headers(path, &[("range", range)]).await
        }
    }

    fn manifest_etag() -> String {
        let digest: String = Sha256::digest(manifest().encode_to_vec())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        format!("\"{digest}\"")
    }

    fn bundle_path() -> String {
        format!("/v1/bundle/{VERSION}")
    }

    async fn assert_not_modified(response: reqwest::Response, etag: &str, cache_control: &str) {
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(response.headers()[ETAG], etag);
        assert_eq!(response.headers()[CACHE_CONTROL], cache_control);
        assert!(response.bytes().await.unwrap().is_empty());
    }

    fn manifest() -> BundleManifest {
        BundleManifest {
            version: VERSION.into(),
            size_bytes: BUNDLE_BYTES.len() as u64,
            sha256: "not checked by the server".into(),
            download_path: format!("/v1/bundle/{VERSION}"),
            schema_version: 1,
        }
    }

    #[tokio::test]
    async fn the_manifest_is_served_as_protobuf() {
        let server = Server::start(true).await;

        let response = server.get("/v1/bundle/manifest").await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_TYPE], "application/x-protobuf");
        let body = response.bytes().await.unwrap();
        assert_eq!(BundleManifest::decode(body).unwrap(), manifest());
    }

    #[tokio::test]
    async fn a_known_version_is_served_as_zstd() {
        let server = Server::start(true).await;

        let response = server.get(&format!("/v1/bundle/{VERSION}")).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_TYPE], "application/zstd");
        assert_eq!(response.bytes().await.unwrap(), BUNDLE_BYTES);
    }

    #[tokio::test]
    async fn an_unknown_version_is_not_found() {
        let server = Server::start(true).await;

        let response = server.get("/v1/bundle/1-fedcba9876543210").await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_malformed_version_is_not_found() {
        let server = Server::start(true).await;

        for path in [
            "/v1/bundle/latest",
            "/v1/bundle/..%2Fmanifest.pb",
            "/v1/bundle/1-0123456789ABCDEF",
        ] {
            assert_eq!(
                server.get(path).await.status(),
                StatusCode::NOT_FOUND,
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn without_a_built_bundle_the_manifest_is_not_found() {
        let server = Server::start(false).await;

        let response = server.get("/v1/bundle/manifest").await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_bundle_declares_its_length_and_is_cacheable_forever() {
        let server = Server::start(true).await;

        let response = server.get(&bundle_path()).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[CONTENT_LENGTH],
            BUNDLE_BYTES.len().to_string().as_str()
        );
        assert_eq!(response.headers()[CACHE_CONTROL], IMMUTABLE);
        assert_eq!(response.headers()[ETAG], BUNDLE_ETAG);
        assert_eq!(response.bytes().await.unwrap(), BUNDLE_BYTES);
    }

    #[tokio::test]
    async fn a_bundle_larger_than_one_stream_chunk_arrives_byte_identical() {
        let large: Vec<u8> = (0..300_000_u32).map(|index| (index % 251) as u8).collect();
        let server = Server::start_with_bundle(&large).await;

        let response = server.get(&bundle_path()).await;

        assert_eq!(
            response.headers()[CONTENT_LENGTH],
            large.len().to_string().as_str()
        );
        assert_eq!(response.bytes().await.unwrap(), large);
    }

    #[tokio::test]
    async fn the_manifest_must_be_revalidated_and_carries_a_content_hash() {
        let server = Server::start(true).await;

        let response = server.get("/v1/bundle/manifest").await;

        assert_eq!(response.headers()[CACHE_CONTROL], "no-cache");
        assert_eq!(response.headers()[ETAG], manifest_etag().as_str());
    }

    #[tokio::test]
    async fn a_matching_tag_on_a_bundle_is_not_modified() {
        let server = Server::start(true).await;
        let other = "\"1-fedcba9876543210\"";

        for tags in [
            BUNDLE_ETAG.to_owned(),
            format!("{other}, {BUNDLE_ETAG}"),
            format!("{other},{BUNDLE_ETAG}"),
            "*".to_owned(),
            format!("W/{BUNDLE_ETAG}"),
            format!("{other}, W/{BUNDLE_ETAG}"),
        ] {
            let response = server.get_if_none_match(&bundle_path(), &tags).await;
            assert_not_modified(response, BUNDLE_ETAG, IMMUTABLE).await;
        }
    }

    #[tokio::test]
    async fn a_matching_tag_on_the_manifest_is_not_modified() {
        let server = Server::start(true).await;
        let etag = manifest_etag();
        let other = "\"deadbeef\"";

        for tags in [
            etag.clone(),
            format!("{other}, {etag}"),
            "*".to_owned(),
            format!("W/{etag}"),
        ] {
            let response = server.get_if_none_match("/v1/bundle/manifest", &tags).await;
            assert_not_modified(response, &etag, "no-cache").await;
        }
    }

    #[tokio::test]
    async fn a_different_tag_gets_the_full_response() {
        let server = Server::start(true).await;

        let bundle = server
            .get_if_none_match(&bundle_path(), "\"1-fedcba9876543210\", garbage")
            .await;
        assert_eq!(bundle.status(), StatusCode::OK);
        assert_eq!(bundle.bytes().await.unwrap(), BUNDLE_BYTES);

        let manifest_response = server
            .get_if_none_match("/v1/bundle/manifest", "\"deadbeef\"")
            .await;
        assert_eq!(manifest_response.status(), StatusCode::OK);
        assert_eq!(
            BundleManifest::decode(manifest_response.bytes().await.unwrap()).unwrap(),
            manifest()
        );
    }

    #[tokio::test]
    async fn a_missing_bundle_is_not_found_even_with_a_matching_tag() {
        let server = Server::start(false).await;

        let bundle = server.get_if_none_match(&bundle_path(), "*").await;
        let manifest_response = server.get_if_none_match("/v1/bundle/manifest", "*").await;

        assert_eq!(bundle.status(), StatusCode::NOT_FOUND);
        assert_eq!(manifest_response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn head_on_a_bundle_returns_the_headers_without_a_body() {
        let server = Server::start(true).await;

        let response = server.head(&bundle_path()).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_TYPE], "application/zstd");
        assert_eq!(
            response.headers()[CONTENT_LENGTH],
            BUNDLE_BYTES.len().to_string().as_str()
        );
        assert_eq!(response.headers()[CACHE_CONTROL], IMMUTABLE);
        assert_eq!(response.headers()[ETAG], BUNDLE_ETAG);
        assert!(response.bytes().await.unwrap().is_empty());
    }

    fn large_bundle() -> Vec<u8> {
        (0..300_000_u32).map(|index| (index % 251) as u8).collect()
    }

    async fn assert_partial(response: reqwest::Response, first: usize, last: usize, bundle: &[u8]) {
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        let expected = &bundle[first..=last];
        assert_eq!(
            response.headers()[CONTENT_RANGE],
            format!("bytes {first}-{last}/{}", bundle.len()).as_str()
        );
        assert_eq!(
            response.headers()[CONTENT_LENGTH],
            expected.len().to_string().as_str()
        );
        assert_eq!(response.headers()[CONTENT_TYPE], "application/zstd");
        assert_eq!(response.headers()[ETAG], BUNDLE_ETAG);
        assert_eq!(response.headers()[CACHE_CONTROL], IMMUTABLE);
        assert_eq!(response.headers()[ACCEPT_RANGES], "bytes");
        assert_eq!(response.bytes().await.unwrap(), expected);
    }

    async fn assert_full_bundle(response: reqwest::Response, bundle: &[u8]) {
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get(CONTENT_RANGE).is_none());
        assert_eq!(response.headers()[ACCEPT_RANGES], "bytes");
        assert_eq!(
            response.headers()[CONTENT_LENGTH],
            bundle.len().to_string().as_str()
        );
        assert_eq!(response.bytes().await.unwrap(), bundle);
    }

    async fn assert_unsatisfiable(response: reqwest::Response, length: usize) {
        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(
            response.headers()[CONTENT_RANGE],
            format!("bytes */{length}").as_str()
        );
        assert_eq!(response.headers()[ACCEPT_RANGES], "bytes");
        assert!(response.headers().get(CACHE_CONTROL).is_none());
        assert!(response.bytes().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_full_bundle_response_advertises_byte_ranges() {
        let server = Server::start(true).await;

        assert_full_bundle(server.get(&bundle_path()).await, BUNDLE_BYTES).await;
        let head = server.head(&bundle_path()).await;
        assert_eq!(head.headers()[ACCEPT_RANGES], "bytes");
    }

    #[tokio::test]
    async fn a_middle_range_is_partial_content() {
        let server = Server::start(true).await;

        let response = server.get_range(&bundle_path(), "bytes=3-7").await;

        assert_partial(response, 3, 7, BUNDLE_BYTES).await;
        assert_eq!(&BUNDLE_BYTES[3..=7], b"press");
    }

    #[tokio::test]
    async fn an_open_ended_range_runs_to_the_end_of_the_bundle() {
        let server = Server::start(true).await;

        let response = server.get_range(&bundle_path(), "bytes=11-").await;

        assert_partial(response, 11, BUNDLE_BYTES.len() - 1, BUNDLE_BYTES).await;
    }

    #[tokio::test]
    async fn a_suffix_range_selects_the_last_bytes_of_the_bundle() {
        let server = Server::start(true).await;

        let response = server.get_range(&bundle_path(), "bytes=-5").await;

        assert_partial(
            response,
            BUNDLE_BYTES.len() - 5,
            BUNDLE_BYTES.len() - 1,
            BUNDLE_BYTES,
        )
        .await;
    }

    #[tokio::test]
    async fn the_first_and_last_bytes_can_be_requested_alone() {
        let server = Server::start(true).await;
        let last = BUNDLE_BYTES.len() - 1;

        assert_partial(
            server.get_range(&bundle_path(), "bytes=0-0").await,
            0,
            0,
            BUNDLE_BYTES,
        )
        .await;
        assert_partial(
            server
                .get_range(&bundle_path(), &format!("bytes={last}-{last}"))
                .await,
            last,
            last,
            BUNDLE_BYTES,
        )
        .await;
    }

    #[tokio::test]
    async fn an_end_past_the_bundle_is_clamped_to_its_last_byte() {
        let server = Server::start(true).await;
        let last = BUNDLE_BYTES.len() - 1;

        let response = server.get_range(&bundle_path(), "bytes=20-9999").await;

        assert_partial(response, 20, last, BUNDLE_BYTES).await;
    }

    #[tokio::test]
    async fn a_suffix_longer_than_the_bundle_returns_all_of_it_as_partial_content() {
        let server = Server::start(true).await;
        let last = BUNDLE_BYTES.len() - 1;

        let response = server.get_range(&bundle_path(), "bytes=-9999").await;

        assert_partial(response, 0, last, BUNDLE_BYTES).await;
    }

    #[tokio::test]
    async fn a_range_across_a_stream_chunk_boundary_is_byte_identical() {
        let large = large_bundle();
        let server = Server::start_with_bundle(&large).await;

        for (header, first, last) in [
            ("bytes=65530-131080", 65_530, 131_080),
            ("bytes=65536-131071", 65_536, 131_071),
            ("bytes=200000-", 200_000, large.len() - 1),
            ("bytes=-100000", large.len() - 100_000, large.len() - 1),
            ("bytes=0-299999", 0, large.len() - 1),
        ] {
            let response = server.get_range(&bundle_path(), header).await;
            assert_partial(response, first, last, &large).await;
        }
    }

    #[tokio::test]
    async fn a_range_past_the_end_of_the_bundle_is_not_satisfiable() {
        let server = Server::start(true).await;
        let length = BUNDLE_BYTES.len();

        for header in [
            format!("bytes={length}-"),
            format!("bytes={length}-{length}"),
            "bytes=100-200".to_owned(),
            "bytes=-0".to_owned(),
            "bytes=18446744073709551615-".to_owned(),
        ] {
            let response = server.get_range(&bundle_path(), &header).await;
            assert_unsatisfiable(response, length).await;
        }
    }

    #[tokio::test]
    async fn any_range_on_an_empty_bundle_is_not_satisfiable() {
        let server = Server::start_with_bundle(b"").await;

        for header in ["bytes=0-", "bytes=0-0", "bytes=-1"] {
            let response = server.get_range(&bundle_path(), header).await;
            assert_unsatisfiable(response, 0).await;
        }
    }

    #[tokio::test]
    async fn an_unusable_range_gets_the_full_bundle() {
        let server = Server::start(true).await;

        for header in [
            "bytes=0-1,3-4",
            "bytes=0-1,",
            "items=0-1",
            "bytes=",
            "bytes=-",
            "bytes=a-b",
            "bytes=5-2",
            "bytes=18446744073709551616-",
            "bytes=0-18446744073709551616",
            "garbage",
        ] {
            let response = server.get_range(&bundle_path(), header).await;
            assert_full_bundle(response, BUNDLE_BYTES).await;
        }
    }

    #[tokio::test]
    async fn an_if_range_matching_the_strong_tag_keeps_the_range() {
        let server = Server::start(true).await;

        let response = server
            .get_with_headers(
                &bundle_path(),
                &[("range", "bytes=3-7"), ("if-range", BUNDLE_ETAG)],
            )
            .await;

        assert_partial(response, 3, 7, BUNDLE_BYTES).await;
    }

    #[tokio::test]
    async fn an_if_range_that_is_not_the_strong_tag_gets_the_full_bundle() {
        let server = Server::start(true).await;

        for validator in [
            "\"1-fedcba9876543210\"".to_owned(),
            format!("W/{BUNDLE_ETAG}"),
            "Wed, 21 Oct 2015 07:28:00 GMT".to_owned(),
            "1-0123456789abcdef".to_owned(),
            String::new(),
        ] {
            let response = server
                .get_with_headers(
                    &bundle_path(),
                    &[("range", "bytes=3-7"), ("if-range", &validator)],
                )
                .await;
            assert_full_bundle(response, BUNDLE_BYTES).await;
        }
    }

    #[tokio::test]
    async fn a_failed_if_range_ignores_even_an_unsatisfiable_range() {
        let server = Server::start(true).await;

        let response = server
            .get_with_headers(
                &bundle_path(),
                &[("range", "bytes=9999-"), ("if-range", "\"stale\"")],
            )
            .await;

        assert_full_bundle(response, BUNDLE_BYTES).await;
    }

    #[tokio::test]
    async fn a_matching_if_none_match_wins_over_a_range() {
        let server = Server::start(true).await;

        for range in ["bytes=3-7", "bytes=9999-", "bytes=0-1,3-4"] {
            let response = server
                .get_with_headers(
                    &bundle_path(),
                    &[("range", range), ("if-none-match", BUNDLE_ETAG)],
                )
                .await;
            assert_not_modified(response, BUNDLE_ETAG, IMMUTABLE).await;
        }
        let with_if_range = server
            .get_with_headers(
                &bundle_path(),
                &[
                    ("range", "bytes=3-7"),
                    ("if-range", BUNDLE_ETAG),
                    ("if-none-match", "*"),
                ],
            )
            .await;
        assert_not_modified(with_if_range, BUNDLE_ETAG, IMMUTABLE).await;
    }

    #[tokio::test]
    async fn a_different_if_none_match_still_honours_the_range() {
        let server = Server::start(true).await;

        let response = server
            .get_with_headers(
                &bundle_path(),
                &[
                    ("range", "bytes=3-7"),
                    ("if-none-match", "\"1-fedcba9876543210\""),
                ],
            )
            .await;

        assert_partial(response, 3, 7, BUNDLE_BYTES).await;
    }

    #[tokio::test]
    async fn a_missing_bundle_is_not_found_even_with_a_range() {
        let server = Server::start(false).await;

        let response = server.get_range(&bundle_path(), "bytes=0-1").await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn head_ignores_a_range() {
        let server = Server::start(true).await;

        for range in ["bytes=3-7", "bytes=9999-"] {
            let response = Server::client()
                .head(format!("{}{}", server.base_url, bundle_path()))
                .header("range", range)
                .send()
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);
            assert!(response.headers().get(CONTENT_RANGE).is_none());
            assert_eq!(response.headers()[ACCEPT_RANGES], "bytes");
            assert_eq!(
                response.headers()[CONTENT_LENGTH],
                BUNDLE_BYTES.len().to_string().as_str()
            );
            assert!(response.bytes().await.unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn the_manifest_ignores_a_range() {
        let server = Server::start(true).await;

        let response = server.get_range("/v1/bundle/manifest", "bytes=0-3").await;

        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get(ACCEPT_RANGES).is_none());
        assert!(response.headers().get(CONTENT_RANGE).is_none());
        assert_eq!(
            BundleManifest::decode(response.bytes().await.unwrap()).unwrap(),
            manifest()
        );
    }

    struct SlowRoute {
        started: Arc<Notify>,
        release: Arc<Notify>,
    }

    struct ShutdownHarness {
        base_url: String,
        route: SlowRoute,
        trigger: Arc<Notify>,
        serving: tokio::task::JoinHandle<std::io::Result<()>>,
    }

    impl ShutdownHarness {
        async fn start(drain_limit: Duration) -> Self {
            let route = SlowRoute {
                started: Arc::new(Notify::new()),
                release: Arc::new(Notify::new()),
            };
            let (started, release) = (route.started.clone(), route.release.clone());
            let app = Router::new().route(
                "/slow",
                get(move || {
                    let (started, release) = (started.clone(), release.clone());
                    async move {
                        started.notify_one();
                        release.notified().await;
                        "finished"
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base_url = format!("http://{}", listener.local_addr().unwrap());
            let trigger = Arc::new(Notify::new());
            let signalled = trigger.clone();
            let serving = tokio::spawn(serve(
                listener,
                app,
                async move { signalled.notified().await },
                drain_limit,
            ));
            Self {
                base_url,
                route,
                trigger,
                serving,
            }
        }

        fn request_slow(&self) -> tokio::task::JoinHandle<reqwest::Response> {
            let url = format!("{}/slow", self.base_url);
            tokio::spawn(async move { Server::client().get(url).send().await.unwrap() })
        }
    }

    #[tokio::test]
    async fn shutdown_lets_an_in_flight_request_finish() {
        let harness = ShutdownHarness::start(Duration::from_secs(30)).await;
        let request = harness.request_slow();
        harness.route.started.notified().await;

        harness.trigger.notify_one();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!harness.serving.is_finished());

        harness.route.release.notify_one();
        let response = request.await.unwrap();
        assert_eq!(response.text().await.unwrap(), "finished");
        let outcome = tokio::time::timeout(Duration::from_secs(5), harness.serving).await;
        assert!(outcome.unwrap().unwrap().is_ok());
    }

    #[tokio::test]
    async fn shutdown_gives_up_on_a_request_that_outlasts_the_drain_limit() {
        let harness = ShutdownHarness::start(Duration::from_millis(300)).await;
        let _stuck = harness.request_slow();
        harness.route.started.notified().await;

        harness.trigger.notify_one();

        let outcome = tokio::time::timeout(Duration::from_secs(5), harness.serving).await;
        assert!(outcome.unwrap().unwrap().is_ok());
    }

    #[tokio::test]
    async fn an_idle_server_stops_as_soon_as_shutdown_is_signalled() {
        let harness = ShutdownHarness::start(Duration::from_secs(30)).await;

        harness.trigger.notify_one();

        let outcome = tokio::time::timeout(Duration::from_secs(5), harness.serving).await;
        assert!(outcome.unwrap().unwrap().is_ok());
    }
}

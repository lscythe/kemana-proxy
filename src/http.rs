use std::io::ErrorKind;
use std::path::Path;

use axum::Router;
use axum::extract::{Path as UrlPath, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::bundle::BundleStore;

const PROTOBUF: &str = "application/x-protobuf";
const ZSTD: &str = "application/zstd";

pub fn router(store: BundleStore) -> Router {
    Router::new()
        .route("/v1/bundle/manifest", get(manifest))
        .route("/v1/bundle/{version}", get(bundle))
        .with_state(store)
}

async fn manifest(State(store): State<BundleStore>) -> Response {
    serve_file(&store.manifest_path(), PROTOBUF).await
}

async fn bundle(State(store): State<BundleStore>, UrlPath(version): UrlPath<String>) -> Response {
    match store.bundle_path(&version) {
        Some(path) => serve_file(&path, ZSTD).await,
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn serve_file(path: &Path, content_type: &'static str) -> Response {
    match tokio::fs::read(path).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, content_type)], bytes).into_response(),
        Err(error) if error.kind() == ErrorKind::NotFound => StatusCode::NOT_FOUND.into_response(),
        Err(error) => {
            eprintln!("cannot read {}: {error}", path.display());
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use prost::Message;
    use reqwest::StatusCode;
    use reqwest::header::CONTENT_TYPE;

    use super::*;
    use crate::bundle::BundleManifest;

    const VERSION: &str = "1-0123456789abcdef";
    const BUNDLE_BYTES: &[u8] = b"compressed bundle bytes";

    struct Server {
        _dir: tempfile::TempDir,
        base_url: String,
    }

    impl Server {
        async fn start(with_bundle: bool) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let store = BundleStore::new(dir.path());
            if with_bundle {
                std::fs::write(store.manifest_path(), manifest().encode_to_vec()).unwrap();
                std::fs::write(store.bundle_path(VERSION).unwrap(), BUNDLE_BYTES).unwrap();
            }
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base_url = format!("http://{}", listener.local_addr().unwrap());
            tokio::spawn(async move { axum::serve(listener, router(store)).await.unwrap() });
            Self {
                _dir: dir,
                base_url,
            }
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
}

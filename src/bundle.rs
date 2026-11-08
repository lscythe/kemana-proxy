mod sqlite;
mod store;

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use prost::Message;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

pub use sqlite::{SCHEMA_SQL, write_sqlite};
pub use store::BundleStore;

use crate::adapter::{AdapterError, OperatorAdapter};
pub use crate::proto::BundleManifest;

/// Version of `contracts/bundle/schema.sql` this writer produces. Bump it
/// whenever the pinned contracts change the schema.
pub const SCHEMA_VERSION: u32 = 1;

const VERSION_DIGEST_LENGTH: usize = 16;

/// Bundles are compressed once and downloaded many times, so a slow, small
/// encoding is worth it.
const ZSTD_LEVEL: i32 = 19;

#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    #[error(transparent)]
    Adapter(#[from] AdapterError),
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot write the bundle database: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildOutcome {
    Built,
    Unchanged,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BuildReport {
    pub outcome: BuildOutcome,
    pub manifest: BundleManifest,
}

pub fn bundle_version(schema_version: u32, source_sha256: &str) -> String {
    format!(
        "{schema_version}-{}",
        &source_sha256[..VERSION_DIGEST_LENGTH]
    )
}

/// Builds the bundle for `source` into `out_dir` and points the manifest at
/// it. A source whose bundle already exists is not loaded again. Files only
/// appear under their final names once complete, so a failed build leaves
/// the previous bundle and manifest untouched.
pub fn build_bundle(
    adapter: &dyn OperatorAdapter,
    source: &Path,
    out_dir: &Path,
) -> Result<BuildReport, BundleError> {
    let source_sha256 = sha256_file(source)?;
    let version = bundle_version(SCHEMA_VERSION, &source_sha256);
    let store = BundleStore::new(out_dir);
    let bundle_path = store
        .bundle_path(&version)
        .expect("a version built from a sha256 digest is well formed");

    let outcome = if bundle_path.exists() {
        BuildOutcome::Unchanged
    } else {
        let dataset = adapter.load_static(source)?;
        std::fs::create_dir_all(out_dir).map_err(io_error(out_dir))?;
        let database = NamedTempFile::new_in(out_dir).map_err(io_error(out_dir))?;
        write_sqlite(&dataset, &source_sha256, database.path())?;
        let compressed = compress(database.path(), out_dir)?;
        compressed
            .persist(&bundle_path)
            .map_err(|error| io_error(&bundle_path)(error.error))?;
        BuildOutcome::Built
    };

    let manifest = BundleManifest {
        download_path: format!("/v1/bundle/{version}"),
        version,
        size_bytes: std::fs::metadata(&bundle_path)
            .map_err(io_error(&bundle_path))?
            .len(),
        sha256: sha256_file(&bundle_path)?,
        schema_version: SCHEMA_VERSION,
    };
    write_atomically(&store.manifest_path(), &manifest.encode_to_vec())?;
    Ok(BuildReport { outcome, manifest })
}

fn compress(database: &Path, out_dir: &Path) -> Result<NamedTempFile, BundleError> {
    let compressed = NamedTempFile::new_in(out_dir).map_err(io_error(out_dir))?;
    let input = File::open(database).map_err(io_error(database))?;
    zstd::stream::copy_encode(
        BufReader::new(input),
        BufWriter::new(compressed.as_file()),
        ZSTD_LEVEL,
    )
    .map_err(io_error(compressed.path()))?;
    Ok(compressed)
}

fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), BundleError> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut file = NamedTempFile::new_in(dir).map_err(io_error(dir))?;
    file.write_all(bytes).map_err(io_error(file.path()))?;
    file.persist(path)
        .map_err(|error| io_error(path)(error.error))?;
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, BundleError> {
    let mut file = File::open(path).map_err(io_error(path))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(io_error(path))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn io_error(path: &Path) -> impl FnOnce(std::io::Error) -> BundleError + '_ {
    move |source| BundleError::Io {
        path: path.to_owned(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use prost::Message;
    use rusqlite::Connection;
    use sha2::{Digest, Sha256};

    use super::*;
    use crate::transit_model::Dataset;

    struct FixedAdapter(Dataset);

    impl OperatorAdapter for FixedAdapter {
        fn operator_id(&self) -> &str {
            "demo"
        }

        fn load_static(&self, _source: &Path) -> Result<Dataset, AdapterError> {
            Ok(self.0.clone())
        }
    }

    struct FailingAdapter;

    impl OperatorAdapter for FailingAdapter {
        fn operator_id(&self) -> &str {
            "demo"
        }

        fn load_static(&self, _source: &Path) -> Result<Dataset, AdapterError> {
            Err(AdapterError::MissingFile { file: "stops.txt" })
        }
    }

    struct Workspace {
        dir: tempfile::TempDir,
    }

    impl Workspace {
        fn new() -> Self {
            Self {
                dir: tempfile::tempdir().unwrap(),
            }
        }

        fn source(&self, contents: &[u8]) -> PathBuf {
            let path = self.dir.path().join("feed.zip");
            std::fs::write(&path, contents).unwrap();
            path
        }

        fn out(&self) -> PathBuf {
            self.dir.path().join("bundles")
        }

        fn out_entries(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(self.out())
                .map(|entries| {
                    entries
                        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            names.sort();
            names
        }
    }

    fn sample_adapter() -> FixedAdapter {
        FixedAdapter(sqlite::tests::sample_dataset())
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[test]
    fn the_version_joins_the_schema_version_and_a_source_digest_prefix() {
        let digest = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

        assert_eq!(bundle_version(7, digest), "7-0123456789abcdef");
    }

    #[test]
    fn a_build_writes_the_compressed_bundle_and_a_manifest_describing_it() {
        let workspace = Workspace::new();
        let source = workspace.source(b"feed bytes");

        let report = build_bundle(&sample_adapter(), &source, &workspace.out()).unwrap();

        let version = bundle_version(SCHEMA_VERSION, &sha256_hex(b"feed bytes"));
        let compressed =
            std::fs::read(workspace.out().join(format!("{version}.sqlite.zst"))).unwrap();
        let manifest = report.manifest;
        assert_eq!(report.outcome, BuildOutcome::Built);
        assert_eq!(manifest.version, version);
        assert_eq!(manifest.schema_version, SCHEMA_VERSION);
        assert_eq!(manifest.size_bytes, compressed.len() as u64);
        assert_eq!(manifest.sha256, sha256_hex(&compressed));
        assert_eq!(manifest.download_path, format!("/v1/bundle/{version}"));
        assert_eq!(
            workspace.out_entries(),
            [format!("{version}.sqlite.zst"), "manifest.pb".to_owned()]
        );
        let stored = std::fs::read(workspace.out().join("manifest.pb")).unwrap();
        assert_eq!(BundleManifest::decode(stored.as_slice()).unwrap(), manifest);
    }

    #[test]
    fn the_compressed_bundle_decompresses_to_the_dataset_database() {
        let workspace = Workspace::new();
        let report = build_bundle(
            &sample_adapter(),
            &workspace.source(b"feed"),
            &workspace.out(),
        )
        .unwrap();

        let compressed = std::fs::File::open(
            workspace
                .out()
                .join(format!("{}.sqlite.zst", report.manifest.version)),
        )
        .unwrap();
        let mut database = Vec::new();
        zstd::Decoder::new(compressed)
            .unwrap()
            .read_to_end(&mut database)
            .unwrap();
        let path = workspace.dir.path().join("decompressed.sqlite");
        std::fs::write(&path, database).unwrap();
        let stops: i64 = Connection::open(path)
            .unwrap()
            .query_row("SELECT count(*) FROM stops", [], |row| row.get(0))
            .unwrap();

        assert_eq!(stops, 3);
    }

    #[test]
    fn an_unchanged_source_is_not_rebuilt() {
        let workspace = Workspace::new();
        let source = workspace.source(b"feed");
        let first = build_bundle(&sample_adapter(), &source, &workspace.out()).unwrap();

        let second = build_bundle(&FailingAdapter, &source, &workspace.out()).unwrap();

        assert_eq!(second.outcome, BuildOutcome::Unchanged);
        assert_eq!(second.manifest, first.manifest);
    }

    #[test]
    fn a_changed_source_becomes_the_latest_bundle() {
        let workspace = Workspace::new();
        let first = build_bundle(
            &sample_adapter(),
            &workspace.source(b"old feed"),
            &workspace.out(),
        )
        .unwrap();

        let second = build_bundle(
            &sample_adapter(),
            &workspace.source(b"new feed"),
            &workspace.out(),
        )
        .unwrap();

        assert_eq!(second.outcome, BuildOutcome::Built);
        assert_ne!(second.manifest.version, first.manifest.version);
        let stored = std::fs::read(workspace.out().join("manifest.pb")).unwrap();
        assert_eq!(
            BundleManifest::decode(stored.as_slice()).unwrap(),
            second.manifest
        );
    }

    #[test]
    fn a_failed_load_leaves_no_bundle_behind() {
        let workspace = Workspace::new();

        let error = build_bundle(
            &FailingAdapter,
            &workspace.source(b"feed"),
            &workspace.out(),
        )
        .unwrap_err();

        assert!(matches!(error, BundleError::Adapter(_)), "{error}");
        assert!(
            workspace.out_entries().is_empty(),
            "{:?}",
            workspace.out_entries()
        );
    }

    #[test]
    fn a_failed_database_write_leaves_no_bundle_behind() {
        let workspace = Workspace::new();
        let mut dataset = sqlite::tests::sample_dataset();
        dataset.trips.clear();

        let error = build_bundle(
            &FixedAdapter(dataset),
            &workspace.source(b"feed"),
            &workspace.out(),
        )
        .unwrap_err();

        assert!(matches!(error, BundleError::Sqlite(_)), "{error}");
        assert!(
            workspace.out_entries().is_empty(),
            "{:?}",
            workspace.out_entries()
        );
    }

    #[test]
    fn a_missing_source_is_an_io_error() {
        let workspace = Workspace::new();

        let error = build_bundle(
            &sample_adapter(),
            &workspace.dir.path().join("absent.zip"),
            &workspace.out(),
        )
        .unwrap_err();

        assert!(matches!(error, BundleError::Io { .. }), "{error}");
    }
}

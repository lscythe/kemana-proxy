mod gtfs;

use std::path::{Path, PathBuf};

pub use gtfs::GtfsScheduleAdapter;

use crate::transit_model::Dataset;

pub trait OperatorAdapter {
    fn operator_id(&self) -> &str;

    fn load_static(&self, source: &Path) -> Result<Dataset, AdapterError>;
}

/// Every row-level variant names the feed file and the 1-based line, so a bad
/// feed can be fixed without guessing. Any error fails the whole load.
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot read the GTFS archive: {0}")]
    Archive(#[from] zip::result::ZipError),
    #[error("{file} is missing from the feed")]
    MissingFile { file: &'static str },
    #[error("{file} has no data rows")]
    EmptyFile { file: &'static str },
    #[error("{file}:{line}: {source}")]
    Csv {
        file: &'static str,
        line: u64,
        #[source]
        source: csv::Error,
    },
    #[error("{file}:{line}: required field {field} is empty or absent")]
    MissingField {
        file: &'static str,
        line: u64,
        field: &'static str,
    },
    #[error("{file}:{line}: field {field} has invalid value {value:?}")]
    InvalidValue {
        file: &'static str,
        line: u64,
        field: &'static str,
        value: String,
    },
    #[error("{file}:{line}: {field} {value:?} does not match any row in {target}")]
    UnknownReference {
        file: &'static str,
        line: u64,
        field: &'static str,
        value: String,
        target: &'static str,
    },
    #[error("{file}:{line}: duplicate key ({}) = {value:?}", key_fields.join(", "))]
    DuplicateKey {
        file: &'static str,
        line: u64,
        key_fields: &'static [&'static str],
        value: String,
    },
    #[error("{file}:{line}: stop {stop_id:?} is its own ancestor through parent_station")]
    ParentCycle {
        file: &'static str,
        line: u64,
        stop_id: String,
    },
}

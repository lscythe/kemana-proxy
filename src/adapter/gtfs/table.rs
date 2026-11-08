use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::str::FromStr;

use csv::{ReaderBuilder, StringRecord};

use crate::adapter::AdapterError;
use crate::transit_model::EntityId;

const BYTE_ORDER_MARK: char = '\u{feff}';

/// One data row of a GTFS file, addressed by column name so that optional
/// and reordered columns work, and every error can name its file and line.
pub(super) struct Row<'a> {
    file: &'static str,
    line: u64,
    columns: &'a HashMap<String, usize>,
    record: &'a StringRecord,
}

impl Row<'_> {
    pub(super) fn line(&self) -> u64 {
        self.line
    }

    pub(super) fn optional(&self, field: &'static str) -> Option<&str> {
        let index = *self.columns.get(field)?;
        self.record
            .get(index)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }

    pub(super) fn required(&self, field: &'static str) -> Result<&str, AdapterError> {
        self.optional(field).ok_or(AdapterError::MissingField {
            file: self.file,
            line: self.line,
            field,
        })
    }

    pub(super) fn parse_optional<T: FromStr>(
        &self,
        field: &'static str,
    ) -> Result<Option<T>, AdapterError> {
        self.optional(field)
            .map(|value| value.parse().map_err(|_| self.invalid(field, value)))
            .transpose()
    }

    pub(super) fn parse_required<T: FromStr>(
        &self,
        field: &'static str,
    ) -> Result<T, AdapterError> {
        let value = self.required(field)?;
        value.parse().map_err(|_| self.invalid(field, value))
    }

    pub(super) fn parse_or<T: FromStr>(
        &self,
        field: &'static str,
        default: T,
    ) -> Result<T, AdapterError> {
        Ok(self.parse_optional(field)?.unwrap_or(default))
    }

    pub(super) fn invalid(&self, field: &'static str, value: &str) -> AdapterError {
        AdapterError::InvalidValue {
            file: self.file,
            line: self.line,
            field,
            value: value.to_owned(),
        }
    }

    pub(super) fn duplicate(&self, key_fields: &'static [&'static str]) -> AdapterError {
        let parts: Vec<&str> = key_fields
            .iter()
            .map(|field| self.optional(field).unwrap_or_default())
            .collect();
        AdapterError::DuplicateKey {
            file: self.file,
            line: self.line,
            key_fields,
            value: parts.join(", "),
        }
    }

    pub(super) fn reference(
        &self,
        field: &'static str,
        id: &EntityId,
        source_id: &str,
        known: &HashSet<EntityId>,
        target: &'static str,
    ) -> Result<(), AdapterError> {
        if known.contains(id) {
            Ok(())
        } else {
            Err(AdapterError::UnknownReference {
                file: self.file,
                line: self.line,
                field,
                value: source_id.to_owned(),
                target,
            })
        }
    }
}

pub(super) fn parse_rows<R: Read, T>(
    file: &'static str,
    reader: R,
    mut parse: impl FnMut(&Row) -> Result<T, AdapterError>,
) -> Result<Vec<T>, AdapterError> {
    let mut reader = ReaderBuilder::new().has_headers(true).from_reader(reader);
    let csv_error = |source: csv::Error| AdapterError::Csv {
        file,
        line: source.position().map_or(1, csv::Position::line),
        source,
    };
    let columns: HashMap<String, usize> = reader
        .headers()
        .map_err(csv_error)?
        .iter()
        .enumerate()
        .map(|(index, name)| {
            (
                name.trim_start_matches(BYTE_ORDER_MARK).trim().to_owned(),
                index,
            )
        })
        .collect();

    let mut rows = Vec::new();
    let mut record = StringRecord::new();
    while reader.read_record(&mut record).map_err(csv_error)? {
        let line = record.position().map_or(0, csv::Position::line);
        rows.push(parse(&Row {
            file,
            line,
            columns: &columns,
            record: &record,
        })?);
    }
    Ok(rows)
}

// SPDX-License-Identifier: MIT

//! The dataset store: one Parquet file per dataset, `<dir>/<name>.parquet`. Every function here is
//! blocking (disk and Polars work); the server runs them on the blocking pool.

use std::fs;
use std::io::Cursor;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use polars::prelude::*;
use polars::sql::SQLContext;
use serde::Serialize;

/// The suffix of a dataset file.
pub const EXTENSION: &str = "parquet";
/// Upload temp files start with this; they are never a dataset and are swept at startup.
const TEMP_PREFIX: &str = ".upload-";

/// A dataset name is `^[a-z][a-z0-9_]{0,62}$`: a lowercase SQL identifier, which also means it can
/// never name another directory or a hidden file.
pub fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 63
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
}

/// An upload format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Csv,
    JsonLines,
    Parquet,
}

impl Format {
    /// The `?format=` values.
    pub fn from_query(value: &str) -> Option<Format> {
        match value.to_ascii_lowercase().as_str() {
            "csv" => Some(Format::Csv),
            "jsonl" | "ndjson" => Some(Format::JsonLines),
            "parquet" => Some(Format::Parquet),
            _ => None,
        }
    }

    /// The `Content-Type` values (parameters such as `charset` are ignored).
    pub fn from_content_type(value: &str) -> Option<Format> {
        let media = value
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        match media.as_str() {
            "text/csv" | "application/csv" => Some(Format::Csv),
            "application/x-ndjson"
            | "application/ndjson"
            | "application/jsonl"
            | "application/x-jsonlines"
            | "application/jsonlines" => Some(Format::JsonLines),
            "application/vnd.apache.parquet" | "application/x-parquet" | "application/parquet" => {
                Some(Format::Parquet)
            }
            _ => None,
        }
    }
}

/// A column: its name and Polars type (`i64`, `f64`, `str`, `date`, …).
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ColumnInfo {
    pub name: String,
    #[serde(rename = "type")]
    pub dtype: String,
}

/// What `GET /v1/datasets` and an upload answer for one dataset.
#[derive(Debug, Serialize)]
pub struct DatasetInfo {
    pub name: String,
    pub rows: u64,
    pub columns: Vec<ColumnInfo>,
    /// The Parquet file's size on disk.
    pub bytes: u64,
}

/// Why a store operation failed.
#[derive(Debug)]
pub enum StoreError {
    /// The upload does not parse as the format it claims; the message is Polars'.
    BadData(String),
    /// The upload would not fit in memory once decoded.
    TooLarge(String),
    /// No dataset of that name.
    NotFound,
    /// The disk is full.
    DiskFull,
    /// Any other I/O or Polars failure.
    Internal(String),
}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        if e.raw_os_error() == Some(28) {
            // ENOSPC
            StoreError::DiskFull
        } else {
            StoreError::Internal(e.to_string())
        }
    }
}

pub fn path_of(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.{EXTENSION}"))
}

/// Refuses a Parquet upload whose footer says it decodes to more than `cap` bytes: a few MB of
/// run-length or dictionary encoded Parquet can expand to many GB. The footer is the writer's own
/// account, so this stops an honest bomb; the memory limit remains the hard stop for a lying one.
fn check_decoded_size(body: &[u8], cap: u64) -> Result<(), StoreError> {
    let mut reader = ParquetReader::new(Cursor::new(body));
    let metadata = reader
        .get_metadata()
        .map_err(|e| StoreError::BadData(e.to_string()))?;
    let mut decoded: u64 = 0;
    for group in &metadata.row_groups {
        let columns = group.n_columns().max(1) as u64;
        // At least eight bytes a value: what a decoded numeric column takes.
        let floor = (group.num_rows() as u64)
            .saturating_mul(columns)
            .saturating_mul(8);
        decoded = decoded.saturating_add((group.total_byte_size() as u64).max(floor));
    }
    if decoded > cap {
        return Err(StoreError::TooLarge(format!(
            "the Parquet file decodes to about {decoded} bytes; at most {cap} fit in memory"
        )));
    }
    Ok(())
}

fn parse(format: Format, body: &[u8]) -> Result<DataFrame, StoreError> {
    let cursor = Cursor::new(body);
    let result = match format {
        Format::Csv => CsvReadOptions::default()
            .with_has_header(true)
            .with_infer_schema_length(Some(10_000))
            .map_parse_options(|o| o.with_try_parse_dates(true))
            .into_reader_with_file_handle(cursor)
            .finish(),
        Format::JsonLines => JsonReader::new(cursor)
            .with_json_format(JsonFormat::JsonLines)
            .infer_schema_len(NonZeroUsize::new(10_000))
            .finish(),
        Format::Parquet => ParquetReader::new(cursor).finish(),
    };
    result.map_err(|e| StoreError::BadData(e.to_string()))
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_path(dir: &Path, name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    dir.join(format!("{TEMP_PREFIX}{name}-{nanos}-{n}.tmp"))
}

/// Parses `body` as `format` and stores it as `<name>.parquet`, replacing any dataset of that
/// name. Under a memory limit (`decoded_cap`), a Parquet upload that decodes to more is refused.
/// The file is written to a temp file, synced and renamed into place, so a reader sees the
/// old dataset or the new one, never half of one. Answers the stored dataset and whether it
/// replaced one.
pub fn store(
    dir: &Path,
    name: &str,
    format: Format,
    body: &[u8],
    decoded_cap: Option<u64>,
) -> Result<(DatasetInfo, bool), StoreError> {
    if let (Format::Parquet, Some(cap)) = (format, decoded_cap) {
        check_decoded_size(body, cap)?;
    }
    let mut frame = parse(format, body)?;
    if frame.width() == 0 {
        return Err(StoreError::BadData("the upload has no columns".into()));
    }
    let temp = temp_path(dir, name);
    let written = (|| -> Result<(), StoreError> {
        let file = fs::File::create(&temp)?;
        ParquetWriter::new(&file)
            .finish(&mut frame)
            .map_err(|e| match e {
                PolarsError::IO { error, .. } if error.raw_os_error() == Some(28) => {
                    StoreError::DiskFull
                }
                other => StoreError::Internal(other.to_string()),
            })?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&temp);
        return Err(e);
    }
    let target = path_of(dir, name);
    let replaced = target.exists();
    if let Err(e) = fs::rename(&temp, &target) {
        let _ = fs::remove_file(&temp);
        return Err(e.into());
    }
    let info = describe(dir, name)?;
    Ok((info, replaced))
}

/// Reads one dataset's name, row count, schema and size from its Parquet footer only.
pub fn describe(dir: &Path, name: &str) -> Result<DatasetInfo, StoreError> {
    let path = path_of(dir, name);
    let file = match fs::File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(StoreError::NotFound),
        Err(e) => return Err(e.into()),
    };
    let bytes = file.metadata()?.len();
    let mut reader = ParquetReader::new(file);
    let rows = reader
        .num_rows()
        .map_err(|e| StoreError::Internal(e.to_string()))? as u64;
    let schema = reader
        .schema()
        .map_err(|e| StoreError::Internal(e.to_string()))?;
    let columns = schema
        .iter_values()
        .map(|field| ColumnInfo {
            name: field.name.to_string(),
            dtype: DataType::from_arrow_field(field).to_string(),
        })
        .collect();
    Ok(DatasetInfo {
        name: name.to_string(),
        rows,
        columns,
        bytes,
    })
}

/// The names of every dataset in `dir`, sorted. Files that are not `<valid name>.parquet` are
/// ignored.
pub fn names(dir: &Path) -> Result<Vec<String>, StoreError> {
    let mut names = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        let Some(name) = file_name.strip_suffix(".parquet") else {
            continue;
        };
        if valid_name(name) && entry.file_type()?.is_file() {
            names.push(name.to_string());
        }
    }
    names.sort();
    Ok(names)
}

/// Every dataset, described.
pub fn list(dir: &Path) -> Result<Vec<DatasetInfo>, StoreError> {
    let mut out = Vec::new();
    for name in names(dir)? {
        match describe(dir, &name) {
            Ok(info) => out.push(info),
            // Deleted between the listing and the read.
            Err(StoreError::NotFound) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

/// Deletes one dataset.
pub fn delete(dir: &Path, name: &str) -> Result<(), StoreError> {
    match fs::remove_file(path_of(dir, name)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(StoreError::NotFound),
        Err(e) => Err(e.into()),
    }
}

/// Removes upload temp files and health probe files a crash left behind.
pub fn sweep_temp_files(dir: &Path) -> std::io::Result<usize> {
    let mut removed = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if file_name.starts_with(TEMP_PREFIX) || file_name.starts_with(".healthz-probe") {
            fs::remove_file(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// A SQL context with every dataset registered as a lazy Parquet scan under its name. Nothing is
/// read until the query runs, and only the columns and row groups the plan needs are.
pub fn context(dir: &Path) -> Result<SQLContext, StoreError> {
    let ctx = SQLContext::new();
    for name in names(dir)? {
        let path = path_of(dir, &name);
        let path = path
            .to_str()
            .ok_or_else(|| StoreError::Internal("the data directory is not UTF-8".into()))?;
        let frame = LazyFrame::scan_parquet(PlRefPath::new(path), ScanArgsParquet::default())
            .map_err(|e| StoreError::Internal(e.to_string()))?;
        ctx.register(&name, frame);
    }
    Ok(ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_validated() {
        for ok in ["a", "sales", "sales_2026", &format!("a{}", "b".repeat(62))] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in [
            "",
            "A",
            "1a",
            "_a",
            "a-b",
            "a.b",
            "..",
            "a/b",
            "../etc",
            "a b",
            "é",
            &format!("a{}", "b".repeat(63)),
        ] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn formats() {
        assert_eq!(
            Format::from_content_type("text/csv; charset=utf-8"),
            Some(Format::Csv)
        );
        assert_eq!(
            Format::from_content_type("application/x-ndjson"),
            Some(Format::JsonLines)
        );
        assert_eq!(
            Format::from_content_type("application/vnd.apache.parquet"),
            Some(Format::Parquet)
        );
        assert_eq!(Format::from_content_type("application/octet-stream"), None);
        assert_eq!(Format::from_query("JSONL"), Some(Format::JsonLines));
        assert_eq!(Format::from_query("xlsx"), None);
    }
}

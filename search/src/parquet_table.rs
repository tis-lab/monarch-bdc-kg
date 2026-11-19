//! Reading the index directory's Parquet tables (concept.parquet, word.parquet, written by
//! scripts/export_search.py) into the server's types.
//!
//! Each row is converted to JSON, then deserialized with serde, so a table's columns are read by
//! the field names (and serde renames) the type already uses for its JSON form. Nested lists
//! and structs come through as JSON arrays and objects.

use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result};
use parquet::file::reader::{FileReader, SerializedFileReader};
use serde::de::DeserializeOwned;

pub struct Table<T> {
    pub rows: Vec<T>,
    /// The file's key/value metadata (DuckDB's `KV_METADATA`).
    pub metadata: HashMap<String, String>,
}

pub fn read<T: DeserializeOwned>(path: &Path) -> Result<Table<T>> {
    let reader = SerializedFileReader::new(File::open(path)?)
        .with_context(|| format!("{} is not a Parquet file", path.display()))?;
    let metadata = reader
        .metadata()
        .file_metadata()
        .key_value_metadata()
        .into_iter()
        .flatten()
        .filter_map(|kv| Some((kv.key.clone(), kv.value.clone()?)))
        .collect();
    let mut rows = Vec::new();
    for (n, row) in reader.get_row_iter(None)?.enumerate() {
        let at = || format!("{} row {}", path.display(), n + 1);
        let row = row.with_context(at)?;
        rows.push(serde_json::from_value(row.to_json_value()).with_context(at)?);
    }
    Ok(Table { rows, metadata })
}

//! Read-only comparison of two saved crawl configurations (issue #1529).
//!
//!
//! Recurring audits change configuration over time, and a results difference between two scans
//! cannot be attributed without knowing whether the settings moved too. This module reads each
//! scan's recorded effective configuration (nothing is re-crawled, nothing is written) and reports
//! a field-by-field diff over the same dotted setting paths.

use crate::error::StoreError;
use crate::scan_schema::read_effective_config;
use indexmap::IndexMap;
use rusqlite::{Connection, OpenFlags};
use serde_json::Map;
use serde_json::{json, Value};
use spiderhead_core::crawl_settings::flatten;
use std::path::Path;

/// Field-by-field diff of two effective crawl configurations.
///
/// Paths are the dotted setting paths of the flattened configuration; a free-form map
/// (`http.headers`) is one leaf, as in the manifest. Values are reported exactly as recorded on
/// each side.
pub fn diff_configs(
    before: &Map<String, Value>,
    after: &Map<String, Value>,
) -> IndexMap<String, Value> {
    let flat_before = flatten(&Value::Object(before.clone().into_iter().collect()));
    let flat_after = flatten(&Value::Object(after.clone().into_iter().collect()));
    let changed: Vec<Value> = flat_before
        .iter()
        .filter_map(|(path, old)| {
            let new = flat_after.get(path)?;
            (old != new).then(|| json!({"path": path, "before": old, "after": new}))
        })
        .collect();
    let added: Vec<Value> = flat_after
        .iter()
        .filter(|(path, _)| !flat_before.contains_key(*path))
        .map(|(path, value)| json!({"path": path, "after": value}))
        .collect();
    let removed: Vec<Value> = flat_before
        .iter()
        .filter(|(path, _)| !flat_after.contains_key(*path))
        .map(|(path, value)| json!({"path": path, "before": value}))
        .collect();
    let settings_compared = flat_before
        .keys()
        .chain(flat_after.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let identical = changed.is_empty() && added.is_empty() && removed.is_empty();
    IndexMap::from([
        ("changed".to_string(), Value::Array(changed)),
        ("added".to_string(), Value::Array(added)),
        ("removed".to_string(), Value::Array(removed)),
        ("identical".to_string(), Value::Bool(identical)),
        (
            "settings_compared".to_string(),
            Value::from(settings_compared as i64),
        ),
    ])
}

/// The recorded effective configuration of one scan, opened read-only.
///
/// A missing file is reported as [`StoreError::Invalid`].
pub fn recorded_config(scan_path: &Path) -> Result<Map<String, Value>, StoreError> {
    if !scan_path.is_file() {
        return Err(StoreError::Invalid(format!(
            "scan file does not exist: {}",
            scan_path.display()
        )));
    }
    let con = Connection::open_with_flags(scan_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    read_effective_config(&con)
}

/// Diff the recorded effective configurations of two saved scans, read-only.
pub fn scan_config_diff(
    before_path: &Path,
    after_path: &Path,
) -> Result<IndexMap<String, Value>, StoreError> {
    let before = recorded_config(before_path)?;
    let after = recorded_config(after_path)?;
    let mut result = diff_configs(&before, &after);
    result.insert(
        "before".to_string(),
        Value::String(before_path.display().to_string()),
    );
    result.insert(
        "after".to_string(),
        Value::String(after_path.display().to_string()),
    );
    Ok(result)
}

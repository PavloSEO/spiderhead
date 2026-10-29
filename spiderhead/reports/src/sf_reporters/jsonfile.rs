//! The `audit.json` machine-readable contract writer.
//!
//! Named `jsonfile` so it does not shadow the `json` crate.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use spiderhead_formats::models::AuditResult;
use thiserror::Error;

use crate::json_out;

/// Failure writing the JSON artifact.
#[derive(Debug, Error)]
pub enum JsonfileError {
    /// Serializing the audit document failed.
    #[error("audit.json serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),
    /// Writing or atomically replacing the artifact failed.
    #[error("audit.json write failed: {0}")]
    Io(#[from] std::io::Error),
}

/// The compatibility JSON document of the audit result.
pub fn to_dict(result: &AuditResult) -> Map<String, Value> {
    result.to_json()
}

/// The audit JSON Schema (draft 2020-12), embedded in `spiderhead-formats`.
pub fn load_schema() -> &'static Value {
    spiderhead_formats::audit_schema::audit_schema()
}

/// Every schema violation message of the audit document; empty when it conforms.
pub fn validate(result: &AuditResult) -> Vec<String> {
    let document = Value::Object(to_dict(result));
    match jsonschema::draft202012::new(load_schema()) {
        Ok(validator) => validator
            .iter_errors(&document)
            .map(|error| {
                // Normalize quote style: the validator emits double quotes around names and values.
                error.to_string().replace('"', "'")
            })
            .collect(),
        Err(error) => vec![error.to_string()],
    }
}

/// Write the document with two-space indent, through a `<path>.tmp` sibling that is
/// renamed into place so readers never see a partial document.
pub fn write_json(result: &AuditResult, path: &str) -> Result<String, JsonfileError> {
    let data = to_dict(result);
    let target = PathBuf::from(path);
    let parent = absolute_parent(&target);
    std::fs::create_dir_all(&parent)?;
    let tmp = PathBuf::from(format!("{path}.tmp"));
    let payload = json_out::dumps(&Value::Object(data));
    std::fs::write(&tmp, payload)?;
    std::fs::rename(&tmp, &target)?;
    Ok(path.to_owned())
}

/// The directory containing `path`, resolved to `.` for a bare filename.
fn absolute_parent(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    absolute
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

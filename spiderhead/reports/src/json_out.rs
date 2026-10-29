//! JSON writer: two-space indent, no ASCII escaping of non-ASCII characters.
//!
//! `serde_json::to_string_pretty` produces the same layout; fixture tests compare
//! the output byte for byte.
//! U+2028 and U+2029 are not escaped by either serializer.

use std::io::Write;

use serde_json::Value;
use thiserror::Error;

/// Failure writing the JSON artifact.
#[derive(Debug, Error)]
pub enum JsonError {
    /// Serialization failed (cannot happen for `Value`, kept for completeness).
    #[error("serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),
    /// Writing the artifact failed.
    #[error("write failed: {0}")]
    Io(#[from] std::io::Error),
}

/// Write `data` to `path` as UTF-8 JSON, two-space indent, no ASCII escaping.
pub fn write_json(data: &Value, path: &std::path::Path) -> Result<(), JsonError> {
    let mut file = std::fs::File::create(path)?;
    // `serde_json` pretty layout matches the two-space indent for objects and arrays.
    let payload = serde_json::to_string_pretty(data)?;
    file.write_all(payload.as_bytes())?;
    Ok(())
}

/// Serialize `data` exactly as `write_json` writes it.
pub fn dumps(data: &Value) -> String {
    serde_json::to_string_pretty(data).unwrap_or_default()
}

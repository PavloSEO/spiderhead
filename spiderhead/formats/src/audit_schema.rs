//! The audit JSON Schema (`audit.schema.json`, draft 2020-12) and its validator.
//!
//! Schema source: `spiderhead/sf/schema/audit.schema.json`, checked by
//! the storage validation path.

use serde_json::Value;
use std::sync::OnceLock;

/// The audit schema document, embedded at build time.
pub const AUDIT_SCHEMA_JSON: &str = include_str!("../data/audit.schema.json");

/// An audit document that does not satisfy the schema or declares an unsupported version.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("{0}")]
pub struct AuditSchemaError(pub String);

/// The parsed schema, built once.
pub fn audit_schema() -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        // The embedded asset is checked by the unit tests; an unparsable one cannot ship.
        serde_json::from_str(AUDIT_SCHEMA_JSON).unwrap_or(Value::Null)
    })
}

/// Validate a parsed audit against the schema, then require `schema_version == "2.0"`.
///
/// The first schema violation is reported (`audit schema: <message>`).
pub fn validate_audit_document(document: &Value) -> Result<(), AuditSchemaError> {
    static VALIDATOR: OnceLock<Result<jsonschema::Validator, String>> = OnceLock::new();
    let validator = VALIDATOR
        .get_or_init(|| jsonschema::draft202012::new(audit_schema()).map_err(|e| e.to_string()));
    let validator = validator
        .as_ref()
        .map_err(|e| AuditSchemaError(format!("audit schema: {e}")))?;
    if let Some(error) = validator.iter_errors(document).next() {
        return Err(AuditSchemaError(format!("audit schema: {error}")));
    }
    match document.get("schema_version") {
        Some(Value::String(version)) if version == "2.0" => Ok(()),
        other => Err(AuditSchemaError(format!(
            "unsupported audit schema_version: {}",
            other.map_or_else(|| "None".to_owned(), py_repr)
        ))),
    }
}

/// Quoted text of a JSON scalar, enough for the version message.
fn py_repr(value: &Value) -> String {
    match value {
        Value::String(s) => format!("'{s}'"),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn schema_asset_parses() {
        assert!(audit_schema().is_object());
    }

    #[test]
    fn rejects_non_object_document() {
        assert!(validate_audit_document(&json!([])).is_err());
    }

    #[test]
    fn rejects_empty_object() {
        let error = validate_audit_document(&json!({})).unwrap_err();
        assert!(error.0.starts_with("audit schema: "));
    }
}

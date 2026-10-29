//! Finite scan body policy; metadata capture remains separate from retention.

use crate::error::StoreError;
use indexmap::IndexMap;
use serde_json::Value;

pub const BYTE_FIELDS: &[&str] = &[
    "max_body_bytes",
    "max_body_store_bytes",
    "min_free_bytes",
    "history_warning_bytes",
];

/// Policy keys of the scan_retention.v1 object, in stored order.
const POLICY_KEYS: &[&str] = &[
    "policy_version",
    "body_mode",
    "max_body_bytes",
    "max_body_store_bytes",
    "min_free_bytes",
    "history_warning_bytes",
    "automatic_delete",
];

/// Policy recorded by artifacts that predate the storage section: no bodies are retained.
fn no_body_retention() -> IndexMap<String, Value> {
    IndexMap::from([
        (
            "policy_version".to_owned(),
            Value::from("scan_retention.v1"),
        ),
        ("body_mode".to_owned(), Value::from("off")),
        ("max_body_bytes".to_owned(), Value::from(0)),
        ("max_body_store_bytes".to_owned(), Value::from(0)),
        ("min_free_bytes".to_owned(), Value::from(0)),
        ("history_warning_bytes".to_owned(), Value::from(0)),
        ("automatic_delete".to_owned(), Value::Bool(false)),
    ])
}

/// True for a JSON integer in `0..=2^63-1`; booleans and floats are rejected.
fn is_byte_count(value: &Value) -> bool {
    value.as_i64().is_some_and(|n| n >= 0)
}

/// Validate a scan_retention.v1 policy object and return a copy with its key order kept.
pub fn validate_policy(policy: &Value) -> Result<IndexMap<String, Value>, StoreError> {
    let obj = policy
        .as_object()
        .ok_or_else(|| StoreError::Invalid("invalid scan_retention.v1 policy fields".to_owned()))?;
    if obj.len() != POLICY_KEYS.len() || POLICY_KEYS.iter().any(|key| !obj.contains_key(*key)) {
        return Err(StoreError::Invalid(
            "invalid scan_retention.v1 policy fields".to_owned(),
        ));
    }
    let body_mode = obj.get("body_mode").and_then(Value::as_str).unwrap_or("");
    let max_body_bytes = obj
        .get("max_body_bytes")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let valid = obj.get("policy_version").and_then(Value::as_str) == Some("scan_retention.v1")
        && matches!(body_mode, "off" | "captured_entity_bytes")
        && obj.get("automatic_delete") == Some(&Value::Bool(false))
        && BYTE_FIELDS
            .iter()
            .all(|key| obj.get(*key).is_some_and(is_byte_count))
        && !(body_mode != "off" && max_body_bytes == 0);
    if !valid {
        return Err(StoreError::Invalid(
            "invalid finite scan body-retention policy".to_owned(),
        ));
    }
    Ok(obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
}

/// Policy recorded for a scan configuration. A config without a `storage` section belongs to an
/// earlier producer: it gets the no-body policy, and current defaults are never inserted.
pub fn policy_for_config(
    config: &IndexMap<String, Value>,
) -> Result<IndexMap<String, Value>, StoreError> {
    let Some(storage) = config.get("storage") else {
        return Ok(no_body_retention());
    };
    let storage = storage
        .as_object()
        .ok_or_else(|| StoreError::Invalid("storage section must be an object".to_owned()))?;
    // `policy_version` comes first, then the storage keys (overwriting in place),
    // then `automatic_delete` is forced to false.
    let mut policy: IndexMap<String, Value> = IndexMap::new();
    policy.insert(
        "policy_version".to_owned(),
        Value::from("scan_retention.v1"),
    );
    for (key, value) in storage {
        if key != "format_version" && key != "capacity_profile" {
            policy.insert(key.clone(), value.clone());
        }
    }
    policy.insert("automatic_delete".to_owned(), Value::Bool(false));
    let as_value = Value::Object(policy.into_iter().collect());
    validate_policy(&as_value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn config_without_storage_section_gets_the_no_body_policy() {
        let policy = policy_for_config(&IndexMap::new()).expect("policy");
        assert_eq!(policy["body_mode"], "off");
        assert_eq!(policy["automatic_delete"], Value::Bool(false));
        assert_eq!(policy.len(), 7);
    }

    #[test]
    fn finite_body_policy_needs_a_positive_byte_bound() {
        let mut policy = no_body_retention();
        policy.insert("body_mode".to_owned(), json!("captured_entity_bytes"));
        let as_value = Value::Object(policy.clone().into_iter().collect());
        assert!(validate_policy(&as_value).is_err(), "zero max_body_bytes");
        policy.insert("max_body_bytes".to_owned(), json!(1024));
        let as_value = Value::Object(policy.into_iter().collect());
        assert!(validate_policy(&as_value).is_ok());
    }

    #[test]
    fn byte_fields_reject_floats_bools_and_negatives() {
        let mut policy = no_body_retention();
        policy.insert("min_free_bytes".to_owned(), json!(1.0));
        assert!(validate_policy(&Value::Object(policy.clone().into_iter().collect())).is_err());
        policy.insert("min_free_bytes".to_owned(), json!(true));
        assert!(validate_policy(&Value::Object(policy.clone().into_iter().collect())).is_err());
        policy.insert("min_free_bytes".to_owned(), json!(-1));
        assert!(validate_policy(&Value::Object(policy.into_iter().collect())).is_err());
    }

    #[test]
    fn storage_section_drops_format_and_capacity_keys() {
        let config = IndexMap::from([(
            "storage".to_owned(),
            json!({"format_version": "x", "capacity_profile": "y", "body_mode": "off",
                   "max_body_bytes": 0, "max_body_store_bytes": 0, "min_free_bytes": 0,
                   "history_warning_bytes": 0}),
        )]);
        let policy = policy_for_config(&config).expect("policy");
        assert!(!policy.contains_key("format_version"));
        assert!(!policy.contains_key("capacity_profile"));
        assert_eq!(policy["policy_version"], "scan_retention.v1");
    }
}

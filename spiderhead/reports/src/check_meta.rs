//! Check metadata for reader-facing labels.
//!
//! The check registry lives in another crate. Until it is wired in, this table
//! carries the three display fields the reports read (`message`, `severity`,
//! `fix`) for every known check id. Keep it in sync with the registry.

use serde_json::Value;

static DATA: &str = include_str!("data/check_meta.json");

fn table() -> &'static serde_json::Map<String, Value> {
    static TABLE: std::sync::LazyLock<serde_json::Map<String, Value>> =
        std::sync::LazyLock::new(|| {
            serde_json::from_str::<Value>(DATA)
                .ok()
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default()
        });
    &TABLE
}

/// Whether `check_id` is a registered check.
pub fn contains(check_id: &str) -> bool {
    table().contains_key(check_id)
}

/// The check's `message` field — the reader-facing issue title.
pub fn message(check_id: &str) -> Option<&'static str> {
    table()
        .get(check_id)
        .and_then(|m| m.get("message"))
        .and_then(Value::as_str)
}

/// The check's registered `severity` (defaults to `notice`).
pub fn severity(check_id: &str) -> &'static str {
    table()
        .get(check_id)
        .and_then(|m| m.get("severity"))
        .and_then(Value::as_str)
        .unwrap_or("notice")
}

/// The check's registered `fix` guidance (`None` when absent).
pub fn fix(check_id: &str) -> Option<&'static str> {
    table()
        .get(check_id)
        .and_then(|m| m.get("fix"))
        .and_then(Value::as_str)
}

/// The check's `source` (`"SF-derived"` for an unregistered id).
pub fn source(check_id: &str) -> &'static str {
    table()
        .get(check_id)
        .and_then(|m| m.get("source"))
        .and_then(Value::as_str)
        .unwrap_or("SF-derived")
}

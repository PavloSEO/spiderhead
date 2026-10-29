//! Typed retained outcomes for the bounded external-destination phase.
#![allow(clippy::too_many_arguments, clippy::type_complexity)]

use crate::discovery_ledger::{dump, is_py_int};
use crate::error::StoreError;
use rusqlite::Connection;
use serde_json::{Map, Value};

pub const MAX_EXTERNAL_CHECKS: i64 = 50000;

/// Largest record accepted (8 MiB), kept in step with `scan_artifact`.
const MAX_RECORD_BYTES: usize = 8 * 1024 * 1024;

/// Sorted so the output order is stable.
pub const OUTCOMES: &[&str] = &["blocked", "failed", "fetched", "skipped"];

fn invalid(msg: &str) -> StoreError {
    StoreError::Invalid(msg.to_string())
}

fn s<'a>(map: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    map.get(key).and_then(Value::as_str)
}

/// Install the additive v2 store; v1 readers stay explicitly unavailable.
pub fn ensure_schema(con: &Connection) -> Result<(), StoreError> {
    con.execute_batch(
        "CREATE TABLE IF NOT EXISTS external_checks (ordinal INTEGER PRIMARY KEY CHECK (ordinal >= 0), outcome TEXT NOT NULL CHECK (outcome IN ('fetched','failed','blocked','skipped')), payload_json TEXT NOT NULL);\
         CREATE TABLE IF NOT EXISTS external_check_summary (singleton INTEGER PRIMARY KEY CHECK (singleton=1), payload_json TEXT NOT NULL);",
    )?;
    Ok(())
}

pub fn summary_item(summary: &Map<String, Value>) -> Result<Map<String, Value>, StoreError> {
    let payload_json = dump(&Value::Object(summary.clone()));
    if payload_json.len() > MAX_RECORD_BYTES {
        return Err(invalid(
            "external check summary exceeds the retained record bound",
        ));
    }
    let mut item = Map::new();
    item.insert("kind".into(), Value::from("external_checks_summary"));
    item.insert("item_key".into(), Value::from("run"));
    item.insert("payload_version".into(), Value::from("scan_context.v1"));
    item.insert("payload_json".into(), Value::from(payload_json));
    item.insert("completeness".into(), Value::from("complete"));
    item.insert("reason".into(), Value::from(""));
    Ok(item)
}

/// Item keys are digits in any Unicode Nd script, not just ASCII.
fn is_digits(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_numeric())
}

pub fn validate_check(item: &Map<String, Value>, payload: &Value) -> Result<(), StoreError> {
    const REQUIRED: [&str; 13] = [
        "url",
        "depth",
        "sources",
        "source_count",
        "outcome",
        "reason",
        "status_code",
        "content_type",
        "redirect_url",
        "redirect_chain",
        "final_url",
        "error",
        "error_kind",
    ];
    let bad = || invalid("native external check context is invalid");
    let key = s(item, "item_key").ok_or_else(bad)?;
    if !is_digits(key) {
        return Err(bad());
    }
    let key_int: i64 = key.parse().map_err(|_| bad())?;
    if key_int >= MAX_EXTERNAL_CHECKS
        || s(item, "completeness") != Some("complete")
        || s(item, "reason").is_none_or(|r| !r.is_empty())
    {
        return Err(bad());
    }
    let Value::Object(p) = payload else {
        return Err(bad());
    };
    if p.len() != REQUIRED.len() || REQUIRED.iter().any(|k| !p.contains_key(*k)) {
        return Err(bad());
    }
    let sources = p["sources"].as_array().ok_or_else(bad)?;
    if sources.len() > 8 || sources.iter().any(|v| !v.is_string()) {
        return Err(bad());
    }
    let depth_ok = is_py_int(&p["depth"]) && p["depth"].as_i64().unwrap_or(-1) >= 0;
    let source_count_ok = is_py_int(&p["source_count"])
        && p["source_count"].as_i64().unwrap_or(-1) >= sources.len() as i64;
    let text_ok = [
        "reason",
        "content_type",
        "redirect_url",
        "final_url",
        "error",
        "error_kind",
    ]
    .iter()
    .all(|k| p[*k].is_string());
    let status_ok = p["status_code"].is_null() || is_py_int(&p["status_code"]);
    let outcome_ok = p["outcome"].as_str().is_some_and(|o| OUTCOMES.contains(&o));
    let chain_ok = p["redirect_chain"]
        .as_array()
        .is_some_and(|c| c.len() <= 10);
    if !(p["url"].is_string()
        && depth_ok
        && source_count_ok
        && outcome_ok
        && text_ok
        && status_ok
        && chain_ok)
    {
        return Err(bad());
    }
    Ok(())
}

pub fn validate_summary(item: &Map<String, Value>, payload: &Value) -> Result<(), StoreError> {
    const REQUIRED: [&str; 15] = [
        "enabled",
        "ran",
        "policy",
        "robots_policy",
        "destinations",
        "records_carried",
        "resumed",
        "fetched",
        "failed",
        "blocked",
        "skipped",
        "skipped_reasons",
        "hosts_contacted",
        "requests",
        "finish_reason",
    ];
    const COUNTS: [&str; 9] = [
        "destinations",
        "records_carried",
        "resumed",
        "fetched",
        "failed",
        "blocked",
        "skipped",
        "hosts_contacted",
        "requests",
    ];
    let bad = || invalid("native external check summary is invalid");
    if s(item, "item_key") != Some("run")
        || s(item, "completeness") != Some("complete")
        || s(item, "reason").is_none_or(|r| !r.is_empty())
    {
        return Err(bad());
    }
    let Value::Object(p) = payload else {
        return Err(bad());
    };
    if p.len() != REQUIRED.len() || REQUIRED.iter().any(|k| !p.contains_key(*k)) {
        return Err(bad());
    }
    let counts_ok = COUNTS
        .iter()
        .all(|k| is_py_int(&p[*k]) && p[*k].as_i64().unwrap_or(-1) >= 0);
    if p["enabled"] != Value::Bool(true)
        || !p["ran"].is_boolean()
        || p["robots_policy"].as_str() != Some("not_evaluated")
        || !p["policy"].is_object()
        || !p["skipped_reasons"].is_object()
        || !counts_ok
        || !p["finish_reason"].is_string()
    {
        return Err(bad());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item() -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("kind".into(), json!("external_check"));
        m.insert("item_key".into(), json!("0"));
        m.insert("completeness".into(), json!("complete"));
        m.insert("reason".into(), json!(""));
        m
    }

    fn payload() -> Value {
        json!({
            "url": "https://example.test/x", "depth": 1, "sources": ["https://example.test/"],
            "source_count": 1, "outcome": "fetched", "reason": "", "status_code": 200,
            "content_type": "text/html", "redirect_url": "", "redirect_chain": [],
            "final_url": "https://example.test/x", "error": "", "error_kind": ""
        })
    }

    #[test]
    fn valid_check_passes() {
        validate_check(&item(), &payload()).unwrap();
    }

    #[test]
    fn check_payload_rejects_invalid_shapes() {
        let cases: Vec<Box<dyn Fn(&mut Value)>> = vec![
            Box::new(|p| p["outcome"] = json!("unknown")),
            Box::new(|p| {
                p["sources"] = json!((0..9)
                    .map(|i| format!("https://example.test/{i}"))
                    .collect::<Vec<_>>());
                p["source_count"] = json!(9);
            }),
            Box::new(|p| p["source_count"] = json!(0)),
            Box::new(|p| p["redirect_chain"] = json!(vec!["x"; 11])),
            Box::new(|p| p["depth"] = json!(-1)),
            Box::new(|p| p["status_code"] = json!("200")),
            Box::new(|p| p["extra"] = json!("field")),
            Box::new(|p| {
                p.as_object_mut().unwrap().remove("error_kind");
            }),
        ];
        for mutate in cases {
            let mut p = payload();
            mutate(&mut p);
            assert!(validate_check(&item(), &p).is_err(), "{p}");
        }
    }

    #[test]
    fn summary_rejects_partial_completeness() {
        let mut it = item();
        it.insert("item_key".into(), json!("run"));
        it.insert("completeness".into(), json!("partial"));
        let summary = json!({
            "enabled": true, "ran": true, "policy": {}, "robots_policy": "not_evaluated",
            "destinations": 0, "records_carried": 0, "resumed": 0, "fetched": 0, "failed": 0,
            "blocked": 0, "skipped": 0, "skipped_reasons": {}, "hosts_contacted": 0,
            "requests": 0, "finish_reason": "done"
        });
        assert!(validate_summary(&it, &summary).is_err());
        it.insert("completeness".into(), json!("complete"));
        validate_summary(&it, &summary).unwrap();
    }
}

//! Versioned interpretation of CrUX current-window field Core Web Vitals.
//!
//! A pure projection: no transport, no
//! credentials. Missing data never becomes a passing metric.

use crate::pyjson;
use crate::pystr;
use serde_json::{json, Map, Value};

/// Identifier of the threshold policy these results are judged against.
pub const POLICY: &str = "web-vitals-2024-03";
/// Public reference for the threshold values.
pub const POLICY_URL: &str = "https://web.dev/articles/defining-core-web-vitals-thresholds";

/// Ordered `(metric, good, poor, unit)` thresholds. The order is part of the `metrics` and
/// `missing_metrics` output, so this stays a fixed list, not a map.
const THRESHOLDS: [(&str, f64, f64, &str); 3] = [
    ("largest_contentful_paint", 2500.0, 4000.0, "ms"),
    ("interaction_to_next_paint", 200.0, 500.0, "ms"),
    ("cumulative_layout_shift", 0.1, 0.25, "score"),
];

const LABELS: [(&str, &str); 3] = [
    ("largest_contentful_paint", "LCP"),
    ("interaction_to_next_paint", "INP"),
    ("cumulative_layout_shift", "CLS"),
];

/// Raised before any assessment when the record is not CrUX field data.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct CwvError(pub String);

impl CwvError {
    /// Error class name reported to callers.
    pub fn python_class(&self) -> &'static str {
        "ValueError"
    }
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

/// A `{year, month, day}` object of non-boolean integers as an ISO date, or None.
fn py_date(value: Option<&Value>) -> Option<String> {
    let map = value?.as_object()?;
    let mut parts = [0i64; 3];
    for (index, name) in ["year", "month", "day"].iter().enumerate() {
        parts[index] = pystr::as_strict_i64(map.get(*name))?;
    }
    let [year, month, day] = parts;
    if !(1..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || !(1..=days_in_month(year, month)).contains(&day)
    {
        return None;
    }
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

/// Number from an int, float or string that is finite and non-negative; booleans and other types
/// are rejected before conversion.
fn py_float(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    let number = match value {
        Value::Bool(_) => return None,
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(s) => pyjson::float_parse(s)?,
        _ => return None,
    };
    if number.is_finite() && number >= 0.0 {
        Some(number)
    } else {
        None
    }
}

/// Assess one CrUX current record; missing data never becomes a pass.
pub fn assess(record: &Value) -> Result<Value, CwvError> {
    let get = |key: &str| record.as_object().and_then(|m| m.get(key));
    if get("metric_source").and_then(Value::as_str) != Some("CrUX current field data") {
        return Err(CwvError(
            "CWV findings require CrUX current field data".to_owned(),
        ));
    }
    let period = get("collection_period");
    let first = period
        .filter(|p| p.is_object())
        .and_then(|p| py_date(p.as_object().and_then(|m| m.get("firstDate"))));
    // Use `lastDate`, or `endDate` when `lastDate` is falsy.
    let last = period.filter(|p| p.is_object()).and_then(|p| {
        let m = p.as_object()?;
        let primary = m.get("lastDate");
        if pystr::truthy(primary) {
            py_date(primary)
        } else {
            py_date(m.get("endDate"))
        }
    });
    let valid_period = matches!((&first, &last), (Some(f), Some(l)) if f <= l);
    let metrics = match get("metrics") {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };
    let record_state = get("state").and_then(Value::as_str);
    let ok_false = get("ok") == Some(&Value::Bool(false));
    let mut results = Map::new();
    for (name, good, poor, unit) in THRESHOLDS {
        let entry = metrics.get(name);
        let raw = entry.and_then(Value::as_object).and_then(|m| m.get("p75"));
        let value = py_float(raw);
        let reason = if record_state == Some("not_configured") {
            Some("not_configured")
        } else if record_state == Some("no_field_data") {
            Some("no_field_data")
        } else if matches!(
            record_state,
            Some("response_too_large") | Some("cache_too_large") | Some("cache_invalid")
        ) {
            record_state
        } else if ok_false {
            Some("provider_failed")
        } else if value.is_none() {
            Some("missing_or_invalid_p75")
        } else if !valid_period {
            Some("missing_or_invalid_collection_period")
        } else {
            None
        };
        let state = if reason.is_some() {
            "unavailable"
        } else if value.unwrap_or(f64::MAX) <= good {
            "good"
        } else if value.unwrap_or(f64::MAX) <= poor {
            "needs_improvement"
        } else {
            "poor"
        };
        let label = LABELS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, l)| *l)
            .unwrap_or(name);
        results.insert(
            name.to_owned(),
            json!({
                "label": label,
                "p75": if reason.is_none() { value.map_or(Value::Null, Value::from) } else { Value::Null },
                "unit": unit,
                "state": state,
                "reason": reason,
            }),
        );
    }
    let states: Vec<&str> = results
        .values()
        .filter_map(|v| v.get("state").and_then(Value::as_str))
        .collect();
    let overall = if states.iter().all(|s| *s == "unavailable") {
        "unavailable"
    } else if states.contains(&"unavailable") {
        "partial"
    } else if states.contains(&"poor") {
        "poor"
    } else if states.contains(&"needs_improvement") {
        "needs_improvement"
    } else {
        "good"
    };
    let missing: Vec<Value> = THRESHOLDS
        .iter()
        .map(|(n, ..)| *n)
        .filter(|n| {
            results
                .get(*n)
                .and_then(|e| e.get("state"))
                .and_then(Value::as_str)
                == Some("unavailable")
        })
        .map(|n| Value::String(n.to_owned()))
        .collect();
    // Use `record_target`, or `target` when `record_target` is falsy.
    let record_target = get("record_target").cloned().unwrap_or(Value::Null);
    let target = if pystr::truthy(Some(&record_target)) {
        record_target.clone()
    } else {
        get("target").cloned().unwrap_or(Value::Null)
    };
    let form_factor = get("form_factor").cloned().unwrap_or(Value::Null);
    let form_factor = if pystr::truthy(Some(&form_factor)) {
        form_factor
    } else {
        Value::String("ALL_FORM_FACTORS".to_owned())
    };
    Ok(json!({
        "schema": "spiderhead.field-cwv/1",
        "provider": "crux",
        "metric_source": "field",
        "policy": POLICY,
        "policy_url": POLICY_URL,
        "target": target,
        "requested_target": get("target").cloned().unwrap_or(Value::Null),
        "record_target": record_target,
        "target_kind": get("target_kind").cloned().unwrap_or(Value::Null),
        "form_factor": form_factor,
        "collection_period": {"first_date": first, "last_date": last},
        "retrieved_at": get("retrieved_at").cloned().unwrap_or(Value::Null),
        "provider_access": "read_only",
        "cost_mode": "free_within_quota",
        "quota_mode": "Google Cloud API quota",
        "overall": overall,
        "metrics": results,
        "missing_metrics": missing,
    }))
}

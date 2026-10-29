//! Disabled-by-default local monitoring.
//!
//! Stores bounded caller-imported observations and provides one explicit one-shot
//! collection path. It never starts a timer, daemon, background scheduler, or
//! delivery transport. Collection itself is injected through [`MonitorFetch`] so
//! the projects layer performs no network IO on its own.

use spiderhead_core::filesystem::PrivateMode;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::competitor_scans::resolve_lenient;
use crate::error::{io, py_named, value, ProjectsResult};
use crate::evidence::{artifact_path, digest};
use crate::io::{check_owner, load_document};
use crate::pyjson::{dumps_compact, dumps_default};
use crate::pytime::{monotonic, zulu};
use crate::runtime::{project_policy, write_document};
use crate::service_delivery::{DeliveryReceipts, MonitorServiceDelivery};
use crate::workspace::load as workspace_load;

mod configure;
mod measurement;
mod run;
pub use configure::*;
pub use measurement::*;
pub use run::*;

/// `FORMAT`.
pub const FORMAT: &str = "spiderhead.monitor.v1";
/// `NAME`.
pub const NAME: &str = "monitor.json";
const SEVERITIES: [&str; 3] = ["notice", "warning", "critical"];
const QUALIFIERS: [&str; 6] = [
    "fresh",
    "revalidated",
    "stale",
    "unavailable",
    "partial",
    "failed",
];
const TRACKED_FIELDS: [&str; 7] = [
    "status",
    "indexability",
    "canonical",
    "robots",
    "metadata",
    "content",
    "links",
];
const DEFAULT_LEASE_SECONDS: i64 = 300;

fn cache_states(qualifier: &str) -> &'static [&'static str] {
    match qualifier {
        "fresh" => &["fresh"],
        "revalidated" => &["revalidated"],
        "stale" => &["cached"],
        _ => &["unavailable"],
    }
}

fn py_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().unwrap_or_default() != 0.0,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Equality across JSON scalars where booleans match 0 and 1, and `1 == 1.0`.
fn py_eq(a: &Value, b: &Value) -> bool {
    fn numeric(v: &Value) -> Option<f64> {
        match v {
            Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            Value::Number(n) => n.as_f64(),
            _ => None,
        }
    }
    match (a, b) {
        (Value::Bool(_) | Value::Number(_), Value::Bool(_) | Value::Number(_)) => {
            numeric(a) == numeric(b)
        }
        _ => a == b,
    }
}

fn py_list(value: &Value) -> ProjectsResult<Vec<Value>> {
    match value {
        Value::Array(items) => Ok(items.clone()),
        Value::Object(map) => Ok(map.keys().cloned().map(Value::String).collect()),
        Value::String(s) => Ok(s.chars().map(|c| Value::String(c.to_string())).collect()),
        _ => Err(py_named(
            "TypeError",
            format!("'{}' object is not iterable", py_typename(value)),
        )),
    }
}

fn py_typename(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(_) => "float",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn int_field(policy: &Value, key: &str) -> ProjectsResult<i64> {
    match policy.get(key) {
        Some(Value::Number(n)) => n
            .as_i64()
            .ok_or_else(|| value(format!("monitor {key} must be a positive integer"))),
        _ => Err(value(format!("monitor {key} must be a positive integer"))),
    }
}

/// `_expiry`: `fromisoformat(stamp) + timedelta(seconds=seconds)` as ISO text.
fn expiry(stamp: &str, seconds: i64) -> ProjectsResult<String> {
    let (civil, offset) = parse_py_iso(stamp)?;
    let shifted = civil
        .checked_add(jiff::Span::new().seconds(seconds))
        .map_err(|e| value(e.to_string()))?;
    Ok(isoformat(&shifted, offset.as_deref()))
}

/// Parse an ISO-8601 timestamp: civil time plus optional offset text.
fn parse_py_iso(stamp: &str) -> ProjectsResult<(jiff::civil::DateTime, Option<String>)> {
    let s = stamp.replacen('Z', "+00:00", 1);
    let split = s
        .char_indices()
        .skip_while(|(i, _)| *i < 10)
        .find(|(_, c)| *c == '+' || *c == '-')
        .map(|(i, _)| i);
    let (civil_s, offset) = match split {
        Some(i) => (&s[..i], Some(s[i..].to_string())),
        None => (s.as_str(), None),
    };
    let civil = civil_s
        .parse::<jiff::civil::DateTime>()
        .map_err(|_| value(format!("Invalid isoformat string: '{stamp}'")))?;
    if let Some(text) = &offset {
        let valid = text.len() == 6
            && text.starts_with(['+', '-'])
            && text[1..3].chars().all(|c| c.is_ascii_digit())
            && text.as_bytes()[3] == b':'
            && text[4..6].chars().all(|c| c.is_ascii_digit());
        if !valid {
            return Err(value(format!("Invalid isoformat string: '{stamp}'")));
        }
    }
    Ok((civil, offset))
}

/// `datetime.isoformat()` with `.replace("+00:00", "Z")` applied.
fn isoformat(civil: &jiff::civil::DateTime, offset: Option<&str>) -> String {
    let mut out = civil.strftime("%Y-%m-%dT%H:%M:%S").to_string();
    let micros = civil.subsec_nanosecond() / 1000;
    if micros != 0 {
        out.push_str(&format!(".{micros:06}"));
    }
    if let Some(text) = offset {
        if text == "+00:00" {
            out.push('Z');
        } else {
            out.push_str(text);
        }
    }
    out
}

/// Instant comparison for `fromisoformat` pairs; naive stamps raise TypeError.
fn iso_ge(stamp: &str, other: &str) -> ProjectsResult<bool> {
    let (a, ao) = parse_py_iso(stamp)?;
    let (b, bo) = parse_py_iso(other)?;
    match (ao, bo) {
        (Some(ao), Some(bo)) => {
            let instant = |civil: jiff::civil::DateTime, off: &str| -> i64 {
                let sign = if off.starts_with('-') { -1i64 } else { 1i64 };
                let hours: i64 = off[1..3].parse().unwrap_or_default();
                let minutes: i64 = off[4..6].parse().unwrap_or_default();
                civil
                    .to_zoned(jiff::tz::TimeZone::UTC)
                    .map(|z| z.timestamp().as_second())
                    .unwrap_or_default()
                    - sign * (hours * 3600 + minutes * 60)
            };
            Ok(instant(a, &ao) >= instant(b, &bo))
        }
        _ => Err(py_named(
            "TypeError",
            "can't compare offset-naive and offset-aware datetimes",
        )),
    }
}

fn default_document(project_uuid: &str) -> Value {
    json!({
        "format": FORMAT,
        "revision": 0,
        "project_uuid": project_uuid,
        "policy": null,
        "runs": [],
        "runner": {"state": "idle", "runs_since_full_refresh": 0, "next_url_offset": 0},
    })
}

/// `_load`: open the workspace, read the monitor document, apply invariants.
pub(crate) fn monitor_load(directory: &Path) -> ProjectsResult<(PathBuf, Value)> {
    let (root, project, mut document) =
        load_document(directory, NAME, &|uuid| default_document(uuid))?;
    check_owner(
        &document,
        project["project_uuid"].as_str().unwrap_or_default(),
        FORMAT,
        "monitor",
    )?;
    match document.get("runs") {
        Some(Value::Array(items)) if items.len() <= 10_000 => {}
        _ => return Err(value("monitor has invalid retained run history")),
    }
    if document.get("runner").is_none() {
        document["runner"] = json!({"state": "idle", "runs_since_full_refresh": 0});
    }
    match document["runner"].as_object_mut() {
        Some(runner) => {
            runner.entry("next_url_offset").or_insert_with(|| json!(0));
        }
        None => {
            return Err(py_named(
                "AttributeError",
                format!(
                    "'{}' object has no attribute 'setdefault'",
                    py_typename(&document["runner"])
                ),
            ))
        }
    }
    Ok((root, document))
}

/// `_policy`: validate a bounded incremental-scope policy declaration.
fn monitor_policy(input: &Value) -> ProjectsResult<Value> {
    const REQUIRED: [&str; 5] = [
        "enabled",
        "urls",
        "max_urls",
        "max_requests",
        "full_refresh_every",
    ];
    const OPTIONAL: [&str; 4] = [
        "interval_seconds",
        "max_render_requests",
        "suppression_runs",
        "severity_threshold",
    ];
    let map = match input.as_object() {
        Some(map) if REQUIRED.iter().all(|key| map.contains_key(*key)) => map,
        _ => {
            return Err(value(
                "monitor policy must declare bounded incremental scope",
            ))
        }
    };
    if map
        .keys()
        .any(|key| !REQUIRED.contains(&key.as_str()) && !OPTIONAL.contains(&key.as_str()))
    {
        return Err(value("monitor policy has unsupported fields"));
    }
    let mut policy = json!({
        "interval_seconds": 3600,
        "max_render_requests": 0,
        "suppression_runs": 1,
        "severity_threshold": "warning",
    });
    for (key, item) in map {
        policy[key] = item.clone();
    }
    if !policy["enabled"].is_boolean() {
        return Err(value("monitor policy enabled must be boolean"));
    }
    let urls = policy["urls"].as_array();
    let urls_ok = match urls {
        Some(items) if !items.is_empty() && items.len() <= 500 => {
            // `len(set(urls))` runs before the isinstance check: unhashable items TypeError.
            let mut unique = HashSet::new();
            for item in items {
                match item {
                    Value::Object(_) | Value::Array(_) => {
                        return Err(py_named(
                            "TypeError",
                            format!("unhashable type: '{}'", py_typename(item)),
                        ));
                    }
                    _ => {
                        unique.insert(dumps_compact(item));
                    }
                }
            }
            unique.len() == items.len()
                && items.iter().all(|item| {
                    item.as_str()
                        .map(|url| url.starts_with("http://") || url.starts_with("https://"))
                        .unwrap_or(false)
                })
        }
        _ => false,
    };
    if !urls_ok {
        return Err(value(
            "monitor URLs must be a unique bounded absolute HTTP(S) list",
        ));
    }
    for key in [
        "max_urls",
        "max_requests",
        "full_refresh_every",
        "interval_seconds",
        "suppression_runs",
    ] {
        if int_field(&policy, key).is_err() || int_field(&policy, key)? < 1 {
            return Err(value(format!("monitor {key} must be a positive integer")));
        }
    }
    match policy["max_render_requests"].as_i64() {
        Some(n) if n >= 0 => {}
        _ => {
            return Err(value(
                "monitor max_render_requests must be a nonnegative integer",
            ))
        }
    }
    let urls_len = policy["urls"].as_array().map(Vec::len).unwrap_or_default() as i64;
    if int_field(&policy, "max_urls")? > urls_len || int_field(&policy, "max_requests")? < urls_len
    {
        return Err(value("monitor request and URL budgets are inconsistent"));
    }
    if !SEVERITIES.contains(&policy["severity_threshold"].as_str().unwrap_or_default()) {
        return Err(value(
            "monitor severity_threshold must be notice, warning or critical",
        ));
    }
    Ok(policy)
}

fn obj_get<'a>(object: &'a Value, key: &str) -> ProjectsResult<&'a Value> {
    match object {
        Value::Object(map) => map
            .get(key)
            .ok_or_else(|| py_named("KeyError", format!("'{key}'"))),
        _ => Err(py_named(
            "TypeError",
            format!("'{}' object is not subscriptable", py_typename(object)),
        )),
    }
}

fn py_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Bool(b) => Some(if *b { 1 } else { 0 }),
        Value::Number(n) => n.as_i64(),
        _ => None,
    }
}

/// `_due`: compute the next bounded pass without contacting anything.
fn due(document: &Value) -> ProjectsResult<Value> {
    let policy = document.get("policy").cloned().unwrap_or(Value::Null);
    if policy.is_null() {
        return Ok(json!({
            "state": "unconfigured",
            "reason": "configure a bounded policy first",
        }));
    }
    let runner = obj_get(document, "runner")?;
    let enabled = obj_get(&policy, "enabled")?;
    if !py_truthy(enabled) {
        return Ok(json!({
            "state": "disabled",
            "reason": "monitoring is disabled by policy",
        }));
    }
    if runner.get("state").and_then(Value::as_str) == Some("running") {
        return Ok(json!({
            "state": "claimed",
            "reason": "a local runner already owns the next bounded pass",
        }));
    }
    let runs = document["runs"].as_array().cloned().unwrap_or_default();
    let refresh_every = obj_get(&policy, "full_refresh_every")?;
    let since = runner
        .get("runs_since_full_refresh")
        .cloned()
        .unwrap_or(json!(0));
    let full = runs.is_empty()
        || match (py_i64(&since), py_i64(refresh_every)) {
            (Some(a), Some(b)) => a >= b,
            _ => {
                return Err(py_named(
                    "TypeError",
                    "comparison between incompatible types",
                ))
            }
        };
    let urls = obj_get(&policy, "urls")?
        .as_array()
        .cloned()
        .unwrap_or_default();
    let len = urls.len() as i64;
    if len == 0 {
        return Err(py_named(
            "ZeroDivisionError",
            "integer division or modulo by zero",
        ));
    }
    let offset_raw = runner.get("next_url_offset").cloned().unwrap_or(json!(0));
    let offset = py_i64(&offset_raw)
        .ok_or_else(|| py_named("TypeError", "not all arguments converted during formatting"))?;
    let offset = offset.rem_euclid(len);
    let planned_urls: Vec<Value> = if full {
        urls.clone()
    } else {
        let max_urls = py_i64(obj_get(&policy, "max_urls")?)
            .ok_or_else(|| py_named("TypeError", "range() argument must be an integer"))?;
        (0..max_urls)
            .map(|item| urls[((offset + item).rem_euclid(len)) as usize].clone())
            .collect()
    };
    Ok(json!({
        "state": "due",
        "mode": if full { "full" } else { "incremental" },
        "url_limit": planned_urls.len(),
        "planned_urls": planned_urls,
        "request_budget": policy["max_requests"],
        "render_request_budget": policy["max_render_requests"],
        "interval_seconds": policy["interval_seconds"],
        "next_due_at": runner.get("next_due_at").cloned().unwrap_or(Value::Null),
        "reason": if full {
            "periodic full coverage refresh"
        } else {
            "bounded incremental refresh"
        },
    }))
}

/// `_planned_due`: keep a claimed full pass full while the runner is active.
fn planned_due(document: &Value) -> ProjectsResult<Value> {
    let runner = obj_get(document, "runner")?;
    let plan = runner.get("plan");
    if runner.get("state").and_then(Value::as_str) == Some("running")
        && plan.map(Value::is_object).unwrap_or(false)
        && plan.and_then(|p| p.get("state")).and_then(Value::as_str) == Some("due")
    {
        return Ok(plan.cloned().unwrap_or(Value::Null));
    }
    due(document)
}

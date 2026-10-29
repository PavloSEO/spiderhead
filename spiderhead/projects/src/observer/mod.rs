//! Read-only projection for observers.
//!
//! Composes the project, coverage, preparation, scan-history and inbox stores.
//! It owns no crawl, task, finding, or job state. The live-recompute policy from
//! The recompute policy (`classify_trigger`, `needs_recompute`) is implemented locally
//! because its owning module is not available to this crate.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{json, Map, Value};

use crate::bindings::py_str;
use crate::competitor_scans::{resolve_lenient, task_competitor_run_reference};
use crate::error::{py_exception_name, py_named, value, ProjectsError, ProjectsResult};
use crate::evidence_cache::{
    evidence_cache, evidence_cache_key, evidence_cache_limit, evidence_state, findings_cache,
    findings_cache_limit, findings_cache_max_bytes, log as evidence_log, lru_get, lru_put,
    relative_artifact, revisioned,
};
use crate::progress::{item_state, project_progress};
use crate::projection::{
    bounded_value, coverage_summary, finding_summary, group_size_map, legacy_groups,
    method_coverage, open_companion, pagination, projection_truncated, scan_row, severity_rank,
    v2_findings_page, v2_group_sizes, CHECKLIST_SORTS, CHECKLIST_STATES, FINDING_SORTS,
    MAX_FINDING_PAGE_SIZE,
};

mod finding_page_result;
mod task_detail;
mod v2_evidence;
pub use finding_page_result::*;
pub use task_detail::*;
pub use v2_evidence::*;

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

fn is_hex64(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `datetime.now(timezone.utc).isoformat()` — `+00:00`, never `Z`.
fn utc_now_iso() -> String {
    let dt = jiff::Zoned::now()
        .with_time_zone(jiff::tz::TimeZone::UTC)
        .datetime();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}+00:00",
        dt.year(),
        dt.month(),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second(),
        dt.subsec_nanosecond() / 1000
    )
}

fn reason(error: &ProjectsError) -> String {
    py_exception_name(error).1
}

/// Text form of a SQLite value, used as a JSON object key.
fn sql_text(value: &rusqlite::types::Value) -> String {
    match value {
        rusqlite::types::Value::Null => "None".to_string(),
        rusqlite::types::Value::Integer(n) => n.to_string(),
        rusqlite::types::Value::Real(f) => crate::pyjson::python_float_repr(*f),
        rusqlite::types::Value::Text(t) => t.clone(),
        rusqlite::types::Value::Blob(b) => format!("{b:?}"),
    }
}

fn falsy(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(b) => !*b,
        Value::Number(n) => n.as_f64().unwrap_or_default() == 0.0,
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
    }
}

/// `needs_recompute`: whether one trigger event forces a rebuild.
fn needs_recompute(trigger: &str) -> ProjectsResult<(bool, &'static str)> {
    match trigger {
        "crawl_started" => Ok((
            true,
            "a started crawl replaces the previously projected statistics",
        )),
        "crawl_finished" => Ok((
            true,
            "a finished crawl finalizes the audit evidence and its counters",
        )),
        "segments_changed" => Ok((
            true,
            "the declared segment scope changed, so segment aggregates are stale",
        )),
        "config_changed" => Ok((
            true,
            "the configuration changed, so prior aggregates describe other settings",
        )),
        "scan_loaded" => Ok((
            false,
            "saved audit counters are shown as persisted; loading never recomputes",
        )),
        other => Err(value(format!("unknown recompute trigger: {other:?}"))),
    }
}

/// `classify_trigger`: map a state transition onto an event name.
fn classify_trigger(before: Option<&Value>, after: &Value) -> ProjectsResult<&'static str> {
    if !after.is_object() {
        return Err(value("the current scan state must be a mapping"));
    }
    let Some(before) = before else {
        return Ok("scan_loaded");
    };
    if !before.is_object() {
        return Err(value("the previous scan state must be a mapping or None"));
    }
    let lifecycle = after.get("lifecycle");
    if lifecycle == Some(&json!("running")) && before.get("lifecycle") != Some(&json!("running")) {
        return Ok("crawl_started");
    }
    if before.get("lifecycle") == Some(&json!("running"))
        && ["finished", "failed", "interrupted"]
            .contains(&lifecycle.and_then(Value::as_str).unwrap_or_default())
    {
        return Ok("crawl_finished");
    }
    if before.get("segments").cloned().unwrap_or_else(|| json!([]))
        != after.get("segments").cloned().unwrap_or_else(|| json!([]))
    {
        return Ok("segments_changed");
    }
    if before.get("config_fingerprint") != after.get("config_fingerprint")
        || before.get("evidence_revision") != after.get("evidence_revision")
    {
        return Ok("config_changed");
    }
    Ok("scan_loaded")
}

/// `_read_retained_findings`: read findings, bounding bytes before caching.
pub(crate) fn read_retained_findings(row: &Map<String, Value>) -> ProjectsResult<Value> {
    let path = row
        .get("path")
        .ok_or_else(|| py_named("KeyError", "'path'"))?
        .as_str()
        .unwrap_or_default();
    let scan_path = Path::new(path);
    let audit;
    let issues;
    if spiderhead_store::audit_v2::audit_v2_path(scan_path).exists() {
        let reader = spiderhead_store::audit_v2::AuditV2Reader::new(scan_path, true, true, None)
            .map_err(ProjectsError::Store)?;
        let con = open_companion(&reader.path)?;
        let size: i64 = con
            .query_row(
                "SELECT COALESCE(SUM(length(CAST(value_json AS BLOB))),0) \
                 FROM items WHERE pointer='/issues'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| value(e.to_string()))?;
        if size > findings_cache_max_bytes() as i64 {
            return Err(value(
                "finding collection exceeds the in-memory cache; use bounded finding pages",
            ));
        }
        audit = reader.header.clone();
        issues = json!(reader
            .iter_collection("/issues")
            .map_err(ProjectsError::Store)?);
    } else {
        audit =
            spiderhead_store::scan_artifact::read_audit(scan_path).map_err(ProjectsError::Store)?;
        issues = audit.get("issues").cloned().unwrap_or(Value::Null);
    }
    let list = match &issues {
        Value::Array(items) if items.iter().all(Value::is_object) => items.clone(),
        _ => {
            return Err(value("retained scan audit has no readable finding list"));
        }
    };
    let run = audit.get("run");
    let skipped = run
        .filter(|r| r.is_object())
        .and_then(|r| r.get("checks_skipped"))
        .cloned()
        .unwrap_or_else(|| json!([]));
    let summary = audit.get("summary").cloned().unwrap_or(Value::Null);
    let coverage_input = if matches!(summary, Value::Object(_)) {
        summary
            .get("check_coverage")
            .cloned()
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    Ok(json!({
        "issues": list,
        "skipped_checks": skipped,
        "check_coverage": spiderhead_store::issue_counters::coverage_summary(&coverage_input),
        "schema_version": audit.get("schema_version").cloned().unwrap_or(Value::Null),
        "generated_at": run
            .filter(|r| r.is_object())
            .and_then(|r| r.get("generated_at"))
            .cloned()
            .unwrap_or(Value::Null),
    }))
}

/// `_retained_findings`: reuse bounded finding rows while state matches.
pub(crate) fn retained_findings(row: &Map<String, Value>) -> ProjectsResult<Value> {
    let key = evidence_cache_key(row)?;
    if let Some(cached) = lru_get(findings_cache(), &key) {
        return Ok(cached);
    }
    let findings = read_retained_findings(row)?;
    if evidence_cache_key(row)? != key {
        return Ok(findings);
    }
    let size = crate::pyjson::dumps_compact(&findings).len();
    if size <= findings_cache_max_bytes() {
        lru_put(
            findings_cache(),
            findings_cache_limit(),
            key,
            findings.clone(),
        );
    }
    Ok(findings)
}

/// `_read_scan_evidence`: read collector evidence even before an audit exists.
pub fn read_scan_evidence(row: &Map<String, Value>) -> ProjectsResult<Value> {
    let path = row
        .get("path")
        .ok_or_else(|| py_named("KeyError", "'path'"))?
        .as_str()
        .unwrap_or_default()
        .to_string();
    if row.get("source_kind").and_then(Value::as_str) == Some("native")
        && row.get("lifecycle").and_then(Value::as_str) == Some("running")
    {
        return match spiderhead_store::scan_schema::NativeScanSchemaMixin::observe(
            Path::new(&path),
            0.2,
        ) {
            Ok(live) => {
                let counts = &live["counts"];
                Ok(json!({
                    "state": "available",
                    "validation": live.get("validation").cloned().unwrap_or(Value::Null),
                    "frontier": {
                        "state": "available",
                        "reason": "committed metadata snapshot",
                        "counts": {
                            "queued": counts.get("queued").cloned().unwrap_or(json!(0)),
                            "inflight": counts.get("inflight").cloned().unwrap_or(json!(0)),
                            "done": counts.get("done").cloned().unwrap_or(json!(0)),
                            "excluded": counts.get("excluded").cloned().unwrap_or(json!(0)),
                        },
                    },
                    "committed_page_outcomes": null,
                    "committed_pages": counts.get("pages").cloned().unwrap_or(json!(0)),
                    "sitemaps": {
                        "fetch_summaries": {},
                        "state": "unavailable",
                        "reason": "not in lightweight collection snapshot",
                    },
                    "findings": {
                        "state": "unavailable",
                        "reason": "collection still running; no current finalized audit projected",
                        "total": null,
                        "by_severity": {},
                        "items": [],
                        "truncated": false,
                    },
                    "skipped_checks": null,
                    "check_coverage": {
                        "state": "unavailable",
                        "reason": "collection still running; no finalized audit coverage",
                        "checks_run": null,
                        "checks_skipped": null,
                        "checks_disabled": null,
                        "checks_total": null,
                    },
                }))
            }
            Err(err) => Ok(json!({
                "state": "unavailable",
                "reason": err.to_string(),
            })),
        };
    }
    type Probe = Result<(Map<String, Value>, Vec<(String, i64)>), String>;
    let probe = (|| -> Probe {
        let status =
            spiderhead_store::status::scan_status(&path, false).map_err(|e| e.to_string())?;
        let (con, _mode) =
            spiderhead_store::scan_artifact::open_scan_mode(Path::new(&path), false, None, true)
                .map_err(|e| e.to_string())?;
        let mut statement = con
            .prepare(
                "SELECT completeness,COUNT(*) FROM context_items \
                 WHERE kind='sitemap_fetch_summary' GROUP BY completeness",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([], |r| {
                Ok((
                    sql_text(&r.get::<_, rusqlite::types::Value>(0)?),
                    r.get::<_, i64>(1)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok((status, rows))
    })();
    let (status, sitemap_rows) = match probe {
        Ok(pair) => pair,
        Err(reason_text) => {
            return Ok(json!({"state": "unavailable", "reason": reason_text}));
        }
    };
    let mut evidence = json!({
        "state": "available",
        "validation": status.get("validation").cloned().unwrap_or(Value::Null),
        "frontier": status.get("frontier").cloned().unwrap_or(Value::Null),
        "committed_page_outcomes": status.get("committed_page_outcomes").cloned().unwrap_or(Value::Null),
        "sitemaps": {
            "fetch_summaries": sitemap_rows
                .iter()
                .map(|(key, count)| (key.clone(), json!(count)))
                .collect::<Map<String, Value>>(),
        },
        "check_coverage": {
            "state": "unavailable",
            "reason": "saved audit check coverage is unavailable",
            "checks_run": null,
            "checks_skipped": null,
            "checks_disabled": null,
            "checks_total": null,
        },
    });
    let unavailable = |reason_text: &str| -> (Value, Value, Value) {
        (
            json!({
                "state": "unavailable",
                "reason": reason_text,
                "total": null,
                "by_severity": {},
                "by_check": {},
                "items": [],
                "truncated": false,
            }),
            Value::Null,
            json!({
                "state": "unavailable",
                "reason": reason_text,
                "checks_run": null,
                "checks_skipped": null,
                "checks_disabled": null,
                "checks_total": null,
            }),
        )
    };
    let retained = if spiderhead_store::audit_v2::audit_v2_path(Path::new(&path)).exists() {
        match v2_evidence(Path::new(&path)) {
            Ok(update) => {
                for (key, val) in update.as_object().cloned().unwrap_or_default() {
                    evidence[key] = val;
                }
                return Ok(evidence);
            }
            Err(err) => {
                let (findings, skipped, coverage_v) = unavailable(&reason(&err));
                evidence["findings"] = findings;
                evidence["skipped_checks"] = skipped;
                evidence["check_coverage"] = coverage_v;
                return Ok(evidence);
            }
        }
    } else {
        match retained_findings(row) {
            Ok(retained) => retained,
            Err(err) => {
                let (findings, skipped, coverage_v) = unavailable(&reason(&err));
                evidence["findings"] = findings;
                evidence["skipped_checks"] = skipped;
                evidence["check_coverage"] = coverage_v;
                return Ok(evidence);
            }
        }
    };
    let mut by_severity: BTreeMap<String, i64> = BTreeMap::new();
    let mut by_check: BTreeMap<String, i64> = BTreeMap::new();
    let mut first_issues: Vec<Value> = Vec::new();
    for issue in retained["issues"].as_array().cloned().unwrap_or_default() {
        if let Some(severity) = issue.get("severity").and_then(Value::as_str) {
            *by_severity.entry(severity.to_string()).or_insert(0) += 1;
        }
        if let Some(check) = issue.get("check").and_then(Value::as_str) {
            *by_check.entry(check.to_string()).or_insert(0) += 1;
        }
        if issue.is_object() && first_issues.len() < 20 {
            first_issues.push(issue);
        }
    }
    let by_severity: Map<String, Value> = by_severity
        .into_iter()
        .map(|(k, v)| (k, json!(v)))
        .collect();
    let by_check: Map<String, Value> = by_check.into_iter().map(|(k, v)| (k, json!(v))).collect();
    let wanted: Vec<Option<String>> = first_issues
        .iter()
        .map(|issue| {
            issue
                .get("group_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    let group_sizes = if wanted.iter().any(Option::is_some) {
        group_size_map(&legacy_groups(row), &wanted)
    } else {
        BTreeMap::new()
    };
    let mut finding_items = Vec::new();
    for issue in &first_issues {
        let mut item = Map::new();
        for key in [
            "id",
            "check",
            "severity",
            "target_url",
            "message",
            "fingerprint",
        ] {
            item.insert(
                key.to_string(),
                issue.get(key).cloned().unwrap_or(Value::Null),
            );
        }
        if let Some(group_id) = issue.get("group_id").and_then(Value::as_str) {
            if let Some(size) = group_sizes.get(group_id) {
                item.insert("affected_urls".to_string(), json!(size));
            }
        }
        finding_items.push(Value::Object(item));
    }
    let total: i64 = by_severity
        .values()
        .map(|v| v.as_i64().unwrap_or_default())
        .sum();
    let issues_len = retained["issues"]
        .as_array()
        .map(Vec::len)
        .unwrap_or_default();
    evidence["findings"] = json!({
        "state": "available",
        "reason": null,
        "total": total,
        "by_severity": by_severity,
        "by_check": by_check,
        "items": finding_items,
        "truncated": issues_len > finding_items.len(),
        "next_offset": if issues_len > finding_items.len() {
            json!(finding_items.len())
        } else {
            Value::Null
        },
    });
    evidence["skipped_checks"] = retained["skipped_checks"].clone();
    evidence["check_coverage"] = retained["check_coverage"].clone();
    Ok(evidence)
}

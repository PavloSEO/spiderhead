//! Exact, whole-audit issue and check counters for saved scan query summaries.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Map, Value};

use crate::audit_v2::{audit_v2_path, AuditV2Reader};
use crate::error::StoreError;
use crate::inputs::resolve_audit_source;

/// The companion's header with the skipped/disabled/silent check inventories lifted out of
/// their collections into `run` and `summary.check_coverage`.
fn companion_header(source: &AuditV2Reader) -> Result<Value, StoreError> {
    let mut header = source.header.clone();
    let lift = |pointer: &str| -> Result<Option<Value>, StoreError> {
        if source.collections.contains_key(pointer) {
            Ok(Some(Value::Array(source.iter_collection(pointer)?)))
        } else {
            Ok(None)
        }
    };
    let object = |value: Option<&Value>| {
        value
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    };
    let mut run = object(header.get("run"));
    for (target, pointer) in [
        ("checks_skipped", "/run/checks_skipped"),
        ("checks_disabled", "/run/checks_disabled"),
    ] {
        if let Some(rows) = lift(pointer)? {
            run.insert(target.into(), rows);
        }
    }
    let mut summary = object(header.get("summary"));
    let mut coverage = object(summary.get("check_coverage"));
    for (target, pointer) in [
        (
            "checks_silent_ids",
            "/summary/check_coverage/checks_silent_ids",
        ),
        (
            "checks_disabled_ids",
            "/summary/check_coverage/checks_disabled_ids",
        ),
    ] {
        if let Some(rows) = lift(pointer)? {
            coverage.insert(target.into(), rows);
        }
    }
    summary.insert("check_coverage".into(), Value::Object(coverage));
    if let Some(map) = header.as_object_mut() {
        map.insert("run".into(), Value::Object(run));
        map.insert("summary".into(), Value::Object(summary));
    }
    Ok(header)
}

/// Return persisted audit totals and registry-wide measured/skipped states.
///
/// The summary is source evidence written by the audit engine. Missing fields or a missing
/// audit stay unavailable; this function never reconstructs absent counts as zero.
/// `registry` is the check registry (`CHECKS`), passed in as data.
pub fn summarize(
    path: &Path,
    registry: &Value,
    scan_connection: Option<&Connection>,
    expected_source: Option<&Map<String, Value>>,
    timeout_seconds: f64,
) -> Map<String, Value> {
    match summarize_inner(
        path,
        registry,
        scan_connection,
        expected_source,
        timeout_seconds,
    ) {
        Ok(summary) => summary,
        Err(reason) => unavailable(&format!("saved audit counters are unavailable: {reason}")),
    }
}

fn summarize_inner(
    path: &Path,
    registry: &Value,
    scan_connection: Option<&Connection>,
    expected_source: Option<&Map<String, Value>>,
    timeout_seconds: f64,
) -> Result<Map<String, Value>, String> {
    let header: Value = if audit_v2_path(path).exists() {
        let source = AuditV2Reader::new(path, true, true, Some(timeout_seconds))
            .map_err(|e| e.to_string())?;
        if let Some(expected) = expected_source {
            if source.binding.get("scan_uuid") != expected.get("scan_uuid")
                || source.binding.get("evidence_revision") != expected.get("evidence_revision")
            {
                return Ok(unavailable(
                    "saved audit belongs to a different scan evidence revision",
                ));
            }
        }
        companion_header(&source).map_err(|e| e.to_string())?
    } else if let Some(con) = scan_connection {
        let row: Option<(String, Option<i64>)> = con
            .query_row(
                "SELECT document_json, evidence_revision FROM audit WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let Some((document_json, evidence_revision)) = row else {
            return Ok(unavailable("saved scan has no current audit"));
        };
        if let Some(expected) = expected_source {
            if evidence_revision != expected.get("evidence_revision").and_then(Value::as_i64) {
                return Ok(unavailable(
                    "saved audit belongs to a different scan evidence revision",
                ));
            }
        }
        crate::scan_artifact::loads(&document_json, "audit").map_err(|e| e.to_string())?
    } else {
        resolve_audit_source(&json!(path.display().to_string()))
            .map_err(|e| e.to_string())?
            .0
            .header()
            .clone()
    };
    let summary = header.get("summary").and_then(Value::as_object);
    let run = header.get("run").and_then(Value::as_object);
    let (Some(summary), Some(run)) = (summary, run) else {
        return Ok(unavailable(
            "saved audit does not contain a complete summary",
        ));
    };
    let totals = summary.get("totals").and_then(Value::as_object);
    let by_severity = summary.get("by_severity").and_then(Value::as_object);
    let by_check = summary.get("by_check").and_then(Value::as_object);
    let coverage = summary.get("check_coverage").and_then(Value::as_object);
    let (Some(totals), Some(by_severity), Some(by_check), Some(coverage)) =
        (totals, by_severity, by_check, coverage)
    else {
        return Ok(unavailable("saved audit summary counters are missing"));
    };
    let issues_total = totals.get("issues_total").and_then(Value::as_i64);
    let findings_total = totals
        .get("findings_total")
        .and_then(Value::as_i64)
        .or(issues_total);
    let (Some(issues_total), Some(findings_total)) = (issues_total, findings_total) else {
        return Ok(unavailable("saved audit issue totals are missing"));
    };
    if issues_total < 0 || findings_total < 0 {
        return Ok(unavailable("saved audit issue totals are missing"));
    }
    let mut severities = Map::new();
    for name in ["critical", "warning", "notice", "opportunity"] {
        match by_severity.get(name).and_then(Value::as_i64) {
            Some(v) if v >= 0 => {
                severities.insert(name.into(), json!(v));
            }
            _ => {
                return Ok(unavailable(&format!(
                    "saved audit {name} counter is missing"
                )))
            }
        }
    }
    let mut checks = Map::new();
    for (key, value) in by_check {
        match value.as_i64() {
            Some(v) if v >= 0 => {
                checks.insert(key.clone(), json!(v));
            }
            _ => return Ok(unavailable("saved audit per-check counters are invalid")),
        }
    }
    let exclusions = summary
        .get("finding_exclusions")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let suppressed_by_check = exclusions
        .get("by_check")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let check_states = check_states(
        registry,
        &Value::Object(checks.clone()),
        &Value::Object(coverage.clone()),
        &Value::Object(run.clone()),
        &suppressed_by_check,
    );
    let mut out = Map::new();
    out.insert("state".into(), json!("exact"));
    out.insert("reason".into(), json!(""));
    out.insert("issues_total".into(), json!(issues_total));
    out.insert("findings_total".into(), json!(findings_total));
    out.insert("by_severity".into(), Value::Object(severities));
    out.insert("by_check".into(), Value::Object(checks));
    for (key, value) in coverage_summary(&Value::Object(coverage.clone())) {
        out.insert(key, value);
    }
    out.insert(
        "checks".into(),
        Value::Array(check_states.into_iter().map(Value::Object).collect()),
    );
    Ok(out)
}

/// Compute the per-check states of the whole registry, given the saved audit counters.
///
/// Precedence: fired, silent, suppressed by policy, skipped, disabled, then unavailable.
pub fn check_states(
    registry: &Value,
    by_check: &Value,
    coverage: &Value,
    run: &Value,
    suppressed_by_check: &Value,
) -> Vec<Map<String, Value>> {
    let skipped: std::collections::BTreeMap<String, String> = run
        .get("checks_skipped")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .map(|item| {
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_uppercase();
            let reason = item
                .get("reason")
                .and_then(Value::as_str)
                .filter(|r| !r.is_empty())
                .unwrap_or("check was skipped")
                .to_string();
            (id, reason)
        })
        .collect();
    let mut disabled: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for item in run
        .get("checks_disabled")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match item.as_object() {
            Some(obj) => {
                let id = obj
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_uppercase();
                let reason = obj
                    .get("reason")
                    .and_then(Value::as_str)
                    .filter(|r| !r.is_empty())
                    .unwrap_or("disabled in profile");
                disabled.insert(id, reason.to_string());
            }
            None => {
                let id = item.as_str().map(str::to_uppercase).unwrap_or_default();
                disabled.insert(id, "disabled in profile".into());
            }
        }
    }
    for value in coverage
        .get("checks_disabled_ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let id = value.as_str().map(str::to_uppercase).unwrap_or_default();
        disabled.insert(id, "disabled in profile".into());
    }
    let silent: std::collections::BTreeSet<String> = coverage
        .get("checks_silent_ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_uppercase)
        .collect();
    let checks: std::collections::BTreeMap<String, Value> = by_check
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, v)| (k.to_uppercase(), v.clone()))
        .collect();
    let suppressed: std::collections::BTreeMap<String, Value> = suppressed_by_check
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, v)| (k.to_uppercase(), v.clone()))
        .collect();
    let mut output = Vec::new();
    let Some(registry) = registry.as_object() else {
        return output;
    };
    for (check_id, definition) in registry {
        let key = check_id.to_uppercase();
        let (state, reason, findings): (&str, String, Value) = if let Some(v) = checks.get(&key) {
            ("fired", String::new(), v.clone())
        } else if silent.contains(&key) {
            ("silent", String::new(), json!(0))
        } else if suppressed.contains_key(&key) {
            (
                "fired_suppressed",
                "all findings suppressed by saved policy".into(),
                json!(0),
            )
        } else if let Some(reason) = skipped.get(&key) {
            ("skipped", reason.clone(), Value::Null)
        } else if let Some(reason) = disabled.get(&key) {
            ("disabled", reason.clone(), Value::Null)
        } else {
            (
                "unavailable",
                "check state is absent from saved audit".into(),
                Value::Null,
            )
        };
        let mut entry = Map::new();
        entry.insert("id".into(), json!(check_id));
        entry.insert(
            "severity".into(),
            definition.get("severity").cloned().unwrap_or(Value::Null),
        );
        entry.insert("state".into(), json!(state));
        entry.insert("findings".into(), findings);
        entry.insert(
            "suppressed_findings".into(),
            suppressed.get(&key).cloned().unwrap_or(json!(0)),
        );
        entry.insert("reason".into(), json!(reason));
        output.push(entry);
    }
    output
}

/// Unavailable summary with every counter explicitly null.
pub fn unavailable(reason: &str) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("state".into(), json!("unavailable"));
    out.insert("reason".into(), json!(reason));
    for key in [
        "issues_total",
        "findings_total",
        "by_severity",
        "by_check",
        "checks_run",
        "checks_skipped",
        "checks_disabled",
        "checks_total",
        "checks",
    ] {
        out.insert(key.into(), Value::Null);
    }
    out
}

/// Project exact evaluated/skipped/disabled registry totals from saved coverage metadata.
pub fn coverage_summary(coverage: &Value) -> Map<String, Value> {
    let keys = [
        "checks_total",
        "checks_fired",
        "checks_skipped",
        "checks_disabled",
        "checks_silent",
    ];
    let valid = keys.iter().all(|key| {
        coverage
            .get(*key)
            .and_then(Value::as_i64)
            .is_some_and(|v| v >= 0)
    });
    let mut out = Map::new();
    if !coverage.is_object() || !valid {
        out.insert("state".into(), json!("unavailable"));
        out.insert(
            "reason".into(),
            json!("saved audit check coverage counters are missing"),
        );
        for key in [
            "checks_run",
            "checks_skipped",
            "checks_disabled",
            "checks_total",
        ] {
            out.insert(key.into(), Value::Null);
        }
        return out;
    }
    let get = |key: &str| coverage[key].as_i64().unwrap_or(0);
    out.insert("state".into(), json!("exact"));
    out.insert("reason".into(), json!(""));
    out.insert(
        "checks_run".into(),
        json!(get("checks_fired") + get("checks_silent")),
    );
    out.insert("checks_skipped".into(), json!(get("checks_skipped")));
    out.insert("checks_disabled".into(), json!(get("checks_disabled")));
    out.insert("checks_total".into(), json!(get("checks_total")));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    /// Unique scratch directory; the tests remove it themselves.
    fn scratch_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("spiderhead-store-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn write_audit(dir: &Path, audit: &Value) -> PathBuf {
        let path = dir.join("scan.json");
        std::fs::write(&path, audit.to_string()).expect("write audit");
        path
    }

    fn registry() -> Value {
        json!({
            "FIRED": {"severity": "critical"},
            "SILENT": {"severity": "warning"},
            "SKIPPED": {"severity": "notice"},
            "DISABLED": {"severity": "warning"}
        })
    }

    #[test]
    fn summary_keeps_fired_silent_skipped_and_disabled_checks_distinct() {
        let audit = json!({
            "run": {
                "checks_skipped": [{"id": "SKIPPED", "reason": "no_source_export"}],
                "checks_disabled": [{"id": "DISABLED", "reason": "disabled_in_profile"}]
            },
            "summary": {
                "totals": {"issues_total": 2},
                "by_severity": {"critical": 1, "warning": 1, "notice": 0, "opportunity": 0},
                "by_check": {"FIRED": 2},
                "check_coverage": {
                    "checks_total": 4,
                    "checks_fired": 1,
                    "checks_skipped": 1,
                    "checks_disabled": 1,
                    "checks_silent": 1,
                    "checks_silent_ids": ["SILENT"],
                    "checks_disabled_ids": ["DISABLED"]
                }
            }
        });
        let dir = scratch_dir("counters-a");
        let path = write_audit(&dir, &audit);
        let got = summarize(&path, &registry(), None, None, 5.0);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(got["state"], json!("exact"));
        assert_eq!(got["issues_total"], json!(2));
        assert_eq!(
            got["by_severity"],
            json!({"critical": 1, "warning": 1, "notice": 0, "opportunity": 0})
        );
        assert_eq!(got["checks_run"], json!(2));
        assert_eq!(got["checks_total"], json!(4));
        let rows = got["checks"].as_array().expect("checks");
        let states: BTreeMap<&str, &str> = rows
            .iter()
            .map(|row| {
                (
                    row["id"].as_str().unwrap_or(""),
                    row["state"].as_str().unwrap_or(""),
                )
            })
            .collect();
        assert_eq!(states["FIRED"], "fired");
        assert_eq!(states["SILENT"], "silent");
        assert_eq!(states["SKIPPED"], "skipped");
        assert_eq!(states["DISABLED"], "disabled");
        let skipped = rows
            .iter()
            .find(|r| r["id"] == "SKIPPED")
            .expect("skipped row");
        assert_eq!(skipped["reason"], json!("no_source_export"));
    }

    #[test]
    fn missing_counter_fields_are_unavailable_not_zero() {
        let audit = json!({"run": {}, "summary": {"totals": {}}});
        let dir = scratch_dir("counters-b");
        let path = write_audit(&dir, &audit);
        let got = summarize(&path, &json!({}), None, None, 5.0);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(got["state"], json!("unavailable"));
        assert_eq!(got["issues_total"], Value::Null);
        assert_eq!(got["by_severity"], Value::Null);
    }
}

//! Lossless semantic projection for technical-audit PDF reports.
//!
//! The model contains evidence and stable source references only. Layout, locale,
//! branding and rendering belong to later report layers.

use std::collections::BTreeSet;

use serde_json::{json, Map, Value};
use thiserror::Error;

use crate::client_findings::project_finding;
use crate::document::{detect_kind, normalize_sf_audit};

pub const SCHEMA: &str = "spiderhead.technical-audit-pdf/1";

/// A malformed audit document cannot support a lossless projection.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct PdfModelError(pub String);

impl PdfModelError {
    fn new(message: impl Into<String>) -> Self {
        PdfModelError(message.into())
    }
}

type Res<T> = Result<T, PdfModelError>;

/// `source_ref` — JSON-pointer back into the source document.
fn source_ref(collection: &str, index: Option<usize>, record: Option<&Value>) -> Value {
    let mut pointer = "#".to_owned();
    for part in collection.split('.') {
        pointer.push('/');
        pointer.push_str(&part.replace('~', "~0").replace('/', "~1"));
    }
    if let Some(index) = index {
        pointer.push_str(&format!("/{index}"));
    }
    let mut result = Map::new();
    result.insert("collection".to_owned(), json!(collection));
    result.insert(
        "index".to_owned(),
        index.map(|i| json!(i)).unwrap_or(Value::Null),
    );
    result.insert("pointer".to_owned(), json!(pointer));
    if let Some(record) = record.filter(|r| r.is_object()) {
        for key in ["id", "finding_id", "observation_id"] {
            if let Some(value) = record.get(key) {
                if !value.is_null() {
                    result.insert("id".to_owned(), value.clone());
                    break;
                }
            }
        }
    }
    Value::Object(result)
}

/// `declared` — reported/not_reported value wrapper.
fn declared(value: Option<&Value>, present: bool) -> Value {
    json!({
        "state": if present { "reported" } else { "not_reported" },
        "value": if present { value.cloned().unwrap_or(Value::Null) } else { Value::Null },
    })
}

/// `collection` — preserve absent collections separately from empty ones.
pub(crate) fn collection(value: Option<&Value>, name: &str, record_kind: &str) -> Res<Value> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(json!({
            "state": "unavailable",
            "source_count": Value::Null,
            "projected_count": 0,
            "records": [],
        }));
    };
    let items = value
        .as_array()
        .ok_or_else(|| PdfModelError::new(format!("{name} must be a list when present")))?;
    let mut records = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        if record_kind == "mapping" && !item.is_object() {
            return Err(PdfModelError::new(format!(
                "{name}[{index}] must be an object"
            )));
        }
        if record_kind == "string" && !item.is_string() {
            return Err(PdfModelError::new(format!(
                "{name}[{index}] must be a string"
            )));
        }
        records.push(item.clone());
    }
    Ok(json!({
        "state": "reported",
        "source_count": items.len(),
        "projected_count": records.len(),
        "records": records,
    }))
}

pub(crate) fn coverage_record(
    collection: &str,
    index: usize,
    record: &Value,
    state: &str,
    identifier: Option<&Value>,
    reason: Option<&Value>,
) -> Value {
    json!({
        "source_ref": source_ref(collection, Some(index), if record.is_object() { Some(record) } else { None }),
        "id": identifier.cloned().unwrap_or(Value::Null),
        "state": state,
        "reason": reason.cloned().unwrap_or(Value::Null),
        "record": record.clone(),
    })
}

/// `coverage` — the run/capability groups with per-record source references.
fn coverage(document: &Value, summary: &Value, kind: &str) -> Res<Value> {
    let mut checks: Vec<Value> = Vec::new();
    let mut groups: Map<String, Value> = Map::new();

    let add_group = |groups: &mut Map<String, Value>,
                     checks: &mut Vec<Value>,
                     name: &str,
                     coll: &str,
                     raw: Option<&Value>,
                     state: &str,
                     id_field: Option<&str>,
                     reason_field: Option<&str>,
                     record_kind: &str|
     -> Res<()> {
        let group = collection(raw, coll, record_kind)?;
        groups.insert(name.to_owned(), group.clone());
        for (index, record) in group["records"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .enumerate()
        {
            let identifier = if let (Some(field), true) = (id_field, record.is_object()) {
                record.get(field).cloned()
            } else if record.is_string() {
                Some(record.clone())
            } else {
                None
            };
            let reason = match (reason_field, record.is_object()) {
                (Some(field), true) => record.get(field).cloned(),
                _ => None,
            };
            let mut record_state = state.to_owned();
            if name == "capabilities" && record.is_object() {
                let row_state = record.get("state");
                record_state = row_state
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| "unreported".to_owned());
            }
            let mut cov = coverage_record(
                coll,
                index,
                record,
                &record_state,
                identifier.as_ref(),
                reason.as_ref(),
            );
            if name == "fired" && record.is_object() {
                let key = crate::py::str_value(record.get("id").unwrap_or(&Value::Null));
                if !key.is_empty() {
                    let escaped = key.replace('~', "~0").replace('/', "~1");
                    let obj = cov.as_object_mut().unwrap();
                    let sr = obj
                        .get_mut("source_ref")
                        .and_then(Value::as_object_mut)
                        .expect("source_ref");
                    sr.insert(
                        "pointer".to_owned(),
                        json!(format!("#/summary/by_check/{escaped}")),
                    );
                    sr.insert("key".to_owned(), json!(key));
                    sr.insert("index".to_owned(), Value::Null);
                }
            }
            checks.push(cov);
        }
        Ok(())
    };

    let check_coverage: Option<&Value> = if kind == "site-audit" {
        add_group(
            &mut groups,
            &mut checks,
            "ran",
            "summary.tools_run",
            summary.get("tools_run"),
            "ran",
            None,
            None,
            "string",
        )?;
        add_group(
            &mut groups,
            &mut checks,
            "failed",
            "summary.tools_failed",
            summary.get("tools_failed"),
            "failed",
            Some("tool"),
            Some("error"),
            "mapping",
        )?;
        add_group(
            &mut groups,
            &mut checks,
            "page_tools_failed",
            "summary.page_tools_failed",
            summary.get("page_tools_failed"),
            "failed",
            Some("tool"),
            Some("reason"),
            "mapping",
        )?;
        add_group(
            &mut groups,
            &mut checks,
            "disabled",
            "summary.checks_disabled",
            summary.get("checks_disabled"),
            "disabled",
            Some("id"),
            Some("reason"),
            "mapping",
        )?;
        let evidence = summary.get("evidence_contract");
        if evidence.is_some_and(|e| !e.is_null()) && !evidence.is_some_and(Value::is_object) {
            return Err(PdfModelError::new(
                "summary.evidence_contract must be an object when present",
            ));
        }
        let capabilities = evidence
            .filter(|v| v.is_object())
            .and_then(|e| e.get("capability_rows"));
        add_group(
            &mut groups,
            &mut checks,
            "capabilities",
            "summary.evidence_contract.capability_rows",
            capabilities,
            "unreported",
            Some("check"),
            Some("reason"),
            "mapping",
        )?;
        summary.get("check_coverage")
    } else {
        let by_check = summary.get("by_check");
        if by_check.is_some_and(|v| !v.is_null()) && !by_check.is_some_and(Value::is_object) {
            return Err(PdfModelError::new(
                "summary.by_check must be an object when present",
            ));
        }
        let fired = Value::Array(
            by_check
                .filter(|v| v.is_object())
                .map(|m| {
                    m.as_object()
                        .unwrap()
                        .iter()
                        .map(|(name, count)| json!({"id": name, "finding_count": count}))
                        .collect()
                })
                .unwrap_or_default(),
        );
        add_group(
            &mut groups,
            &mut checks,
            "fired",
            "summary.by_check",
            if by_check.is_some_and(|v| !v.is_null()) {
                Some(&fired)
            } else {
                None
            },
            "has_findings",
            Some("id"),
            None,
            "mapping",
        )?;
        let coverage_value = summary.get("check_coverage");
        if coverage_value.is_some_and(|v| !v.is_null())
            && !coverage_value.is_some_and(Value::is_object)
        {
            return Err(PdfModelError::new(
                "summary.check_coverage must be an object when present",
            ));
        }
        let silent_ids = coverage_value
            .filter(|v| v.is_object())
            .and_then(|c| c.get("checks_silent_ids"));
        add_group(
            &mut groups,
            &mut checks,
            "silent",
            "summary.check_coverage.checks_silent_ids",
            silent_ids,
            "ran_no_findings",
            None,
            None,
            "string",
        )?;
        let run = document
            .get("run")
            .filter(|v| crate::py::truthy(Some(v)))
            .cloned()
            .unwrap_or_else(|| json!({}));
        if !run.is_object() {
            return Err(PdfModelError::new("run must be an object"));
        }
        add_group(
            &mut groups,
            &mut checks,
            "skipped",
            "run.checks_skipped",
            run.get("checks_skipped"),
            "skipped",
            Some("id"),
            Some("reason"),
            "mapping",
        )?;
        add_group(
            &mut groups,
            &mut checks,
            "disabled",
            "run.checks_disabled",
            run.get("checks_disabled"),
            "disabled",
            Some("id"),
            Some("reason"),
            "mapping",
        )?;
        coverage_value
    };

    let cc = check_coverage;
    if cc.is_some_and(|v| !v.is_null()) && !cc.is_some_and(Value::is_object) {
        return Err(PdfModelError::new(
            "summary.check_coverage must be an object when present",
        ));
    }
    let evidence_state = if summary
        .get("evidence_contract")
        .is_some_and(Value::is_object)
    {
        "reported"
    } else {
        "unavailable"
    };
    let check_state = if cc.is_some_and(Value::is_object) {
        "reported"
    } else {
        "unavailable"
    };
    let has_records = groups
        .values()
        .any(|g| g["state"].as_str() == Some("reported"))
        || evidence_state == "reported"
        || check_state == "reported";
    Ok(json!({
        "state": if has_records { "reported" } else { "unavailable" },
        "source_evidence": {
            "state": evidence_state,
            "record": if evidence_state == "reported" {
                summary.get("evidence_contract").cloned().unwrap_or(Value::Null)
            } else {
                Value::Null
            },
        },
        "source_check_coverage": {
            "state": check_state,
            "record": if check_state == "reported" {
                cc.cloned().unwrap_or(Value::Null)
            } else {
                Value::Null
            },
        },
        "groups": groups,
        "checks": checks,
    }))
}

/// `backlog` — project checklist state bound to the report.
fn backlog(
    document: &Value,
    project: Option<&str>,
    kind: &str,
    host: Option<&crate::project_coverage::ProjectsHost<'_>>,
) -> Res<Value> {
    let mut snapshot: Option<Value> = None;
    let mut snapshot_ref: Option<&str> = None;
    if let Some(project) = project {
        let host = host.ok_or_else(|| {
            PdfModelError::new("project-bound reports require an injected projects host".to_owned())
        })?;
        let (_, snap) = crate::project_coverage::load_snapshot(project, document, kind, host)
            .map_err(|e| PdfModelError::new(e.to_string()))?;
        snapshot = Some(snap);
        snapshot_ref = Some("project_snapshot");
    } else {
        let candidate = summary(document).and_then(|s| s.get("project_coverage"));
        if let Some(candidate) = candidate {
            if !candidate.is_object() {
                return Err(PdfModelError::new(
                    "summary.project_coverage must be an object when present",
                ));
            }
            snapshot = Some(candidate.clone());
            snapshot_ref = Some("summary.project_coverage");
        }
    }
    let Some(snapshot) = snapshot else {
        return Ok(json!({
            "state": "not_requested",
            "source_count": Value::Null,
            "declared_count": {"state": "not_requested", "value": Value::Null},
            "projected_count": 0,
            "project": Value::Null,
            "status": Value::Null,
            "items": [],
        }));
    };
    let status = snapshot.get("status").cloned().unwrap_or(Value::Null);
    if !status.is_object() {
        return Ok(json!({
            "state": "unavailable",
            "source_count": Value::Null,
            "declared_count": {"state": "not_reported", "value": Value::Null},
            "projected_count": 0,
            "project": snapshot.get("project").cloned().unwrap_or(Value::Null),
            "status": status,
            "items": [],
        }));
    }
    let raw_items = status.get("items");
    let (state, source_count, items) = match raw_items {
        None | Some(Value::Null) => ("unavailable", Value::Null, Vec::new()),
        Some(Value::Array(rows)) => {
            if rows.iter().any(|item| !item.is_object()) {
                return Err(PdfModelError::new(
                    "project checklist status.items must contain objects",
                ));
            }
            let items: Vec<Value> = rows
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    json!({
                        "source_ref": source_ref(
                            &format!("{}.status.items", snapshot_ref.unwrap_or_default()),
                            Some(index),
                            Some(item),
                        ),
                        "record": item,
                    })
                })
                .collect();
            ("recorded", json!(rows.len()), items)
        }
        _ => {
            return Err(PdfModelError::new(
                "project checklist status.items must be a list when present",
            ))
        }
    };
    let counts = status.get("counts");
    let declared_present = counts.is_some_and(|c| c.is_object() && c.get("total").is_some());
    Ok(json!({
        "state": state,
        "source_count": source_count,
        "declared_count": declared(
            counts.filter(|v| v.is_object()).and_then(|c| c.get("total")),
            declared_present,
        ),
        "projected_count": items.len(),
        "project": snapshot.get("project").cloned().unwrap_or(Value::Null),
        "status": status,
        "items": items,
    }))
}

fn summary(document: &Value) -> Option<&Value> {
    document.get("summary").filter(|v| v.is_object())
}

/// `validate_optional_containers` — reject malformed optional evidence.
fn validate_optional_containers(document: &Value, summary: &Value, kind: &str) -> Res<()> {
    fn validate_count(value: Option<&Value>, name: &str, positive: bool) -> Res<()> {
        let valid = matches!(value, Some(Value::Number(n)) if (n.is_i64() || n.is_u64())
            && n.as_i64().is_some_and(|v| v >= if positive { 1 } else { 0 }));
        if !valid {
            let qualifier = if positive { "positive" } else { "non-negative" };
            return Err(PdfModelError::new(format!(
                "{name} must be a {qualifier} integer"
            )));
        }
        Ok(())
    }

    fn string_ids(raw: Option<&Value>, name: &str) -> Res<Option<BTreeSet<String>>> {
        let Some(raw) = raw.filter(|v| !v.is_null()) else {
            return Ok(None);
        };
        let items = raw.as_array();
        let valid = items.is_some_and(|list| {
            list.iter()
                .all(|v| v.as_str().is_some_and(|s| !s.is_empty()))
        });
        if !valid {
            return Err(PdfModelError::new(format!(
                "{name} must be a list of non-empty strings"
            )));
        }
        let list = items.unwrap();
        let ids: BTreeSet<String> = list
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
        if ids.len() != list.len() {
            return Err(PdfModelError::new(format!(
                "{name} must not contain duplicate IDs"
            )));
        }
        Ok(Some(ids))
    }

    let (mapping_fields, list_fields, container): (Vec<&str>, Vec<&str>, &Value) =
        if kind == "site-audit" {
            (
                vec![
                    "findings_by_severity",
                    "evidence_contract",
                    "check_coverage",
                    "project_coverage",
                ],
                vec![
                    "tools_run",
                    "tools_failed",
                    "page_tools_failed",
                    "checks_disabled",
                ],
                summary,
            )
        } else {
            for name in [
                "totals",
                "by_severity",
                "by_check",
                "check_coverage",
                "project_coverage",
            ] {
                if summary
                    .get(name)
                    .is_some_and(|v| !v.is_null() && !v.is_object())
                {
                    return Err(PdfModelError::new(format!(
                        "summary.{name} must be an object when present"
                    )));
                }
            }
            let run = document.get("run").cloned().unwrap_or_else(|| json!({}));
            let run = if crate::py::truthy(Some(&run)) {
                run
            } else {
                json!({})
            };
            for name in ["checks_skipped", "checks_disabled"] {
                if run.get(name).is_some_and(|v| !v.is_null() && !v.is_array()) {
                    return Err(PdfModelError::new(format!(
                        "run.{name} must be a list when present"
                    )));
                }
            }
            (vec![], vec![], &Value::Null)
        };
    let label = "summary";
    for name in &mapping_fields {
        if container
            .get(*name)
            .is_some_and(|v| !v.is_null() && !v.is_object())
        {
            return Err(PdfModelError::new(format!(
                "{label}.{name} must be an object when present"
            )));
        }
    }
    for name in &list_fields {
        if container
            .get(*name)
            .is_some_and(|v| !v.is_null() && !v.is_array())
        {
            return Err(PdfModelError::new(format!(
                "{label}.{name} must be a list when present"
            )));
        }
    }

    let (severity, severity_prefix) = if kind == "site-audit" {
        let page_failures = summary
            .get("page_tools_failed")
            .cloned()
            .unwrap_or(Value::Null);
        if !page_failures.is_null()
            && page_failures
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| !row.is_object()))
        {
            return Err(PdfModelError::new(
                "summary.page_tools_failed must contain objects",
            ));
        }
        for (index, row) in page_failures
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            for name in ["failed_pages", "pages_checked"] {
                if row.get(name).is_some() {
                    validate_count(
                        row.get(name),
                        &format!("summary.page_tools_failed[{index}].{name}"),
                        false,
                    )?;
                }
            }
            let failed = row.get("failed_pages").and_then(crate::py::to_i64);
            let checked = row.get("pages_checked").and_then(crate::py::to_i64);
            if let (Some(f), Some(c)) = (failed, checked) {
                if f > c {
                    return Err(PdfModelError::new(format!(
                        "summary.page_tools_failed[{index}].failed_pages exceeds pages_checked"
                    )));
                }
            }
        }
        (
            summary.get("findings_by_severity").cloned(),
            "summary.findings_by_severity",
        )
    } else {
        if let Some(by_check) = summary.get("by_check").filter(|v| v.is_object()) {
            for (check, count) in by_check.as_object().unwrap() {
                if check.is_empty() {
                    return Err(PdfModelError::new(
                        "summary.by_check keys must be non-empty strings",
                    ));
                }
                let ok = matches!(count, Value::Number(n) if n.is_i64() && n.as_i64().is_some_and(|v| v > 0));
                if !ok {
                    return Err(PdfModelError::new(format!(
                        "summary.by_check[{}] must be a positive integer",
                        crate::py::repr(&Value::String(check.clone()))
                    )));
                }
            }
        }
        let totals = summary.get("totals").cloned().unwrap_or(Value::Null);
        let totals = if crate::py::truthy(Some(&totals)) {
            totals
        } else {
            json!({})
        };
        for (name, value) in totals.as_object().cloned().unwrap_or_default() {
            if name == "pages_by_representation" {
                if !value.is_object() {
                    return Err(PdfModelError::new(
                        "summary.totals.pages_by_representation must be an object",
                    ));
                }
                for (representation, amount) in value.as_object().unwrap() {
                    if representation.is_empty() {
                        return Err(PdfModelError::new(
                            "summary.totals.pages_by_representation keys must be non-empty strings",
                        ));
                    }
                    validate_count(
                        Some(amount),
                        &format!(
                            "summary.totals.pages_by_representation[{}]",
                            crate::py::repr(&Value::String(representation.clone()))
                        ),
                        false,
                    )?;
                }
            } else {
                validate_count(Some(&value), &format!("summary.totals.{name}"), false)?;
            }
        }
        (summary.get("by_severity").cloned(), "summary.by_severity")
    };

    if let Some(severity) = severity.filter(|v| v.is_object()) {
        for (name, value) in severity.as_object().unwrap() {
            if name.is_empty() {
                return Err(PdfModelError::new(format!(
                    "{severity_prefix} keys must be non-empty strings"
                )));
            }
            validate_count(Some(value), &format!("{severity_prefix}.{name}"), false)?;
        }
    }

    if kind == "site-audit" {
        for name in ["findings_total", "pages_checked"] {
            if let Some(v) = summary.get(name).filter(|v| !v.is_null()) {
                validate_count(Some(v), &format!("summary.{name}"), false)?;
            }
        }
    }

    let evidence = summary.get("evidence_contract");
    if let Some(rows) = evidence
        .filter(|v| v.is_object())
        .and_then(|e| e.get("capability_rows"))
    {
        if !rows.is_null()
            && (!rows.is_array()
                || rows
                    .as_array()
                    .is_some_and(|list| list.iter().any(|row| !row.is_object())))
        {
            return Err(PdfModelError::new(
                "summary.evidence_contract.capability_rows must contain objects",
            ));
        }
    }

    if kind != "sf-audit" {
        return Ok(());
    }

    let check_coverage = summary
        .get("check_coverage")
        .cloned()
        .unwrap_or(Value::Null);
    let run = document.get("run").cloned().unwrap_or(Value::Null);
    let run = if crate::py::truthy(Some(&run)) {
        run
    } else {
        json!({})
    };
    // A truthy non-list value would iterate into non-object items (keys, characters);
    // the object check rejects those.
    let truthy_rows = |value: Option<&Value>| -> Vec<Value> {
        match value.filter(|v| crate::py::truthy(Some(v))) {
            None => Vec::new(),
            Some(Value::Array(items)) => items.clone(),
            Some(other) => vec![other.clone()],
        }
    };
    let skipped_rows = truthy_rows(run.get("checks_skipped"));
    let disabled_rows = truthy_rows(run.get("checks_disabled"));
    for (collection_name, rows) in [
        ("run.checks_skipped", &skipped_rows),
        ("run.checks_disabled", &disabled_rows),
    ] {
        if rows.iter().any(|row| !row.is_object()) {
            return Err(PdfModelError::new(format!(
                "{collection_name} must contain objects"
            )));
        }
        let ids: Vec<Value> = rows
            .iter()
            .map(|row| row.get("id").cloned().unwrap_or(Value::Null))
            .collect();
        string_ids(Some(&Value::Array(ids)), &format!("{collection_name} IDs"))?;
    }
    let skipped_ids: BTreeSet<String> = skipped_rows
        .iter()
        .filter_map(|row| row["id"].as_str().map(str::to_owned))
        .collect();
    let run_disabled_ids: BTreeSet<String> = disabled_rows
        .iter()
        .filter_map(|row| row["id"].as_str().map(str::to_owned))
        .collect();

    if !check_coverage.is_object() {
        return Ok(());
    }
    let cc = check_coverage.as_object().unwrap();
    for (name, value) in cc {
        if name.starts_with("checks_") && !name.ends_with("_ids") {
            validate_count(
                Some(value),
                &format!("summary.check_coverage.{name}"),
                false,
            )?;
        }
    }
    let silent_ids = string_ids(
        cc.get("checks_silent_ids"),
        "summary.check_coverage.checks_silent_ids",
    )?;
    let disabled_ids = string_ids(
        cc.get("checks_disabled_ids"),
        "summary.check_coverage.checks_disabled_ids",
    )?;
    let fired_ids: Option<BTreeSet<String>> = summary
        .get("by_check")
        .filter(|v| v.is_object())
        .map(|m| m.as_object().unwrap().keys().cloned().collect());
    let mut effective_disabled_ids = run_disabled_ids.clone();
    if let Some(ids) = &disabled_ids {
        effective_disabled_ids.extend(ids.iter().cloned());
    }

    if let (Some(silent), Some(fired)) = (&silent_ids, &fired_ids) {
        let intersection: BTreeSet<_> = silent.intersection(fired).collect();
        if !intersection.is_empty() {
            return Err(PdfModelError::new(
                "summary.by_check and checks_silent_ids contain the same check",
            ));
        }
    }
    if let Some(fired) = &fired_ids {
        let overlap = fired.iter().any(|id| effective_disabled_ids.contains(id));
        if overlap {
            return Err(PdfModelError::new(
                "summary.by_check contains a disabled check",
            ));
        }
    }
    if let Some(silent) = &silent_ids {
        if silent.iter().any(|id| effective_disabled_ids.contains(id)) {
            return Err(PdfModelError::new(
                "checks_silent_ids contains a disabled check",
            ));
        }
        if silent.iter().any(|id| skipped_ids.contains(id)) {
            return Err(PdfModelError::new(
                "checks_silent_ids contains a skipped check",
            ));
        }
    }
    if let Some(ids) = &disabled_ids {
        if run.get("checks_disabled").is_some() && ids != &run_disabled_ids {
            return Err(PdfModelError::new(
                "checks_disabled_ids disagrees with run.checks_disabled",
            ));
        }
    }

    let count_fields = &check_coverage;
    let checks_fired = count_fields.get("checks_fired");
    let checks_silent = count_fields.get("checks_silent");
    let checks_disabled = count_fields.get("checks_disabled");
    let checks_skipped = count_fields.get("checks_skipped");
    let checks_total = count_fields.get("checks_total");
    if checks_fired.is_some_and(|v| !v.is_null())
        && fired_ids.is_some()
        && checks_fired.and_then(crate::py::to_i64)
            != Some(fired_ids.as_ref().unwrap().len() as i64)
    {
        return Err(PdfModelError::new(
            "checks_fired disagrees with summary.by_check",
        ));
    }
    if checks_silent.is_some_and(|v| !v.is_null())
        && silent_ids.is_some()
        && checks_silent.and_then(crate::py::to_i64)
            != Some(silent_ids.as_ref().unwrap().len() as i64)
    {
        return Err(PdfModelError::new(
            "checks_silent disagrees with checks_silent_ids",
        ));
    }
    if checks_disabled.is_some_and(|v| !v.is_null()) {
        let declared_disabled = &effective_disabled_ids;
        if (disabled_ids.is_some() || !disabled_rows.is_empty())
            && checks_disabled.and_then(crate::py::to_i64) != Some(declared_disabled.len() as i64)
        {
            return Err(PdfModelError::new(
                "checks_disabled disagrees with disabled check IDs",
            ));
        }
    }
    if checks_skipped.is_some_and(|v| !v.is_null())
        && (!skipped_rows.is_empty() || fired_ids.is_some() || !effective_disabled_ids.is_empty())
    {
        let empty = BTreeSet::new();
        let fired = fired_ids.as_ref().unwrap_or(&empty);
        let effective_skipped: BTreeSet<String> = skipped_ids
            .difference(fired)
            .filter(|id| !effective_disabled_ids.contains(*id))
            .cloned()
            .collect();
        if checks_skipped.and_then(crate::py::to_i64) != Some(effective_skipped.len() as i64) {
            return Err(PdfModelError::new(
                "checks_skipped disagrees with run.checks_skipped",
            ));
        }
    }
    let all = [checks_fired, checks_skipped, checks_disabled, checks_silent]
        .iter()
        .all(|v| v.is_some_and(|v| !v.is_null()));
    if checks_total.is_some_and(|v| !v.is_null()) && all {
        let sum = [checks_fired, checks_skipped, checks_disabled, checks_silent]
            .iter()
            .map(|v| v.and_then(crate::py::to_i64).unwrap_or(0))
            .sum::<i64>();
        if checks_total.and_then(crate::py::to_i64) != Some(sum) {
            return Err(PdfModelError::new(
                "check coverage counts do not sum to checks_total",
            ));
        }
    }
    Ok(())
}

/// Build a lossless semantic model from one recognized saved audit.
///
/// The function is offline and does not write files. It preserves source rows,
/// exact source summary values and their counts separately from projected
/// counts. `diagnostics` receives input diagnostics from `document::load` when
/// the input is a path/scan reference; a document input produces none.
pub fn build_pdf_model(
    document: &Value,
    diagnostics: &[Value],
    project: Option<&str>,
    host: Option<&crate::project_coverage::ProjectsHost<'_>>,
) -> Res<Value> {
    let (kind, error) = detect_kind(document);
    let Some(kind) = kind else {
        return Err(PdfModelError::new(error.unwrap_or_else(|| {
            let mut keys: Vec<&String> = document
                .as_object()
                .map(|m| m.keys().collect())
                .unwrap_or_default();
            keys.sort();
            format!(
                "audit document schema not recognized: {}",
                crate::py::repr(&Value::Array(keys.into_iter().map(|k| json!(k)).collect()))
            )
        })));
    };

    let summary = document.get("summary").cloned().unwrap_or(Value::Null);
    if !summary.is_object() {
        return Err(PdfModelError::new("audit summary must be an object"));
    }
    validate_optional_containers(document, &summary, kind)?;
    let findings_collection = if kind == "site-audit" {
        "findings"
    } else {
        "issues"
    };
    let source_findings = document.get(findings_collection);
    let source_pages = document.get("pages");
    let findings_ok = source_findings.is_some_and(|v| {
        v.as_array()
            .is_some_and(|list| list.iter().all(Value::is_object))
    });
    if !findings_ok {
        return Err(PdfModelError::new(format!(
            "{findings_collection} must contain only finding objects"
        )));
    }
    let pages_ok = source_pages.is_some_and(|v| {
        v.as_array()
            .is_some_and(|list| list.iter().all(Value::is_object))
    });
    if !pages_ok {
        return Err(PdfModelError::new("pages must contain only page objects"));
    }
    let source_findings = source_findings
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let source_pages = source_pages
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let normalized = if kind == "site-audit" {
        document.clone()
    } else {
        normalize_sf_audit(document)
    };
    let normalized_findings = normalized
        .get("findings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let display_findings: Vec<Value> = normalized_findings.iter().map(project_finding).collect();
    if normalized_findings.len() != source_findings.len()
        || display_findings.len() != source_findings.len()
    {
        return Err(PdfModelError::new(
            "finding projection did not preserve the source row count",
        ));
    }
    let normalized_pages = normalized
        .get("pages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if normalized_pages.len() != source_pages.len() {
        return Err(PdfModelError::new(
            "page projection did not preserve the source row count",
        ));
    }

    let mut findings = Vec::with_capacity(source_findings.len());
    for (index, (raw, (projected, display))) in source_findings
        .iter()
        .zip(normalized_findings.iter().zip(display_findings.iter()))
        .enumerate()
    {
        findings.push(json!({
            "source_ref": source_ref(findings_collection, Some(index), Some(raw)),
            "record": raw,
            "display": {
                "check_key": projected.get("check").cloned().unwrap_or(Value::Null),
                "title": display.get("client_title").cloned().unwrap_or(Value::Null),
                "observation": display.get("client_observation").cloned().unwrap_or(Value::Null),
                "reproduction": display.get("client_reproduction").cloned().unwrap_or(Value::Null),
                "details": display.get("client_details").filter(|v| crate::py::truthy(Some(v))).cloned().unwrap_or_else(|| json!([])),
                "locations": display.get("client_locations").filter(|v| crate::py::truthy(Some(v))).cloned().unwrap_or_else(|| json!([])),
                "evidence_reference": display.get("client_evidence").cloned().unwrap_or(Value::Null),
            },
        }));
    }
    let pages: Vec<Value> = source_pages
        .iter()
        .zip(normalized_pages.iter())
        .enumerate()
        .map(|(index, (raw, projected))| {
            json!({
                "source_ref": source_ref("pages", Some(index), Some(raw)),
                "record": raw,
                "display": projected,
            })
        })
        .collect();

    let raw_run = if kind == "sf-audit" {
        document.get("run").cloned().unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    let raw_run = if raw_run.is_object() {
        raw_run
    } else {
        json!({})
    };
    let has = |obj: &Value, key: &str| obj.as_object().is_some_and(|m| m.contains_key(key));
    let crawl_valid_present = has(&raw_run, "crawl_valid") || has(&summary, "crawl_valid");
    let crawl_valid = raw_run
        .get("crawl_valid")
        .or_else(|| summary.get("crawl_valid"))
        .cloned();
    let crawl_partial_present = has(&raw_run, "crawl_partial") || has(&summary, "crawl_partial");
    let crawl_partial = raw_run
        .get("crawl_partial")
        .or_else(|| summary.get("crawl_partial"))
        .cloned();
    let run_state = if crawl_valid == Some(Value::Bool(false)) {
        "failed"
    } else if crawl_partial == Some(Value::Bool(true)) {
        "partial"
    } else if crawl_valid == Some(Value::Bool(true)) && crawl_partial == Some(Value::Bool(false)) {
        "complete"
    } else if document.get("ok") == Some(&Value::Bool(false)) {
        "failed"
    } else if document.get("ok") == Some(&Value::Bool(true)) {
        "complete"
    } else {
        "unknown"
    };
    let mut run_reasons = Vec::new();
    for (name, value) in [
        (
            "crawl_invalid_reason",
            raw_run
                .get("crawl_invalid_reason")
                .filter(|v| crate::py::truthy(Some(v)))
                .cloned()
                .or_else(|| summary.get("crawl_invalid_reason").cloned()),
        ),
        (
            "crawl_finish_reason",
            raw_run.get("crawl_finish_reason").cloned(),
        ),
        (
            "crawl_stopped_reason",
            raw_run.get("crawl_stopped_reason").cloned(),
        ),
    ] {
        if let Some(value) = value.filter(|v| !v.is_null()) {
            run_reasons.push(json!({"source_field": name, "value": value}));
        }
    }
    let mut scope = Map::new();
    for name in [
        "source",
        "project",
        "crawl_valid",
        "crawl_partial",
        "crawl_finish_reason",
        "crawl_stopped_reason",
        "crawl_scope_note",
    ] {
        if let Some(value) = raw_run.get(name) {
            scope.insert(name.to_owned(), value.clone());
        }
    }
    if !scope.contains_key("crawl_scope_note") {
        if let Some(value) = summary.get("crawl_scope_note") {
            scope.insert("crawl_scope_note".to_owned(), value.clone());
        }
    }
    if let Some(value) = summary.get("health_score_scope") {
        scope.insert("health_score_scope".to_owned(), value.clone());
    }
    if summary.get("finding_view").is_some_and(Value::is_object) {
        scope.insert("finding_view".to_owned(), summary["finding_view"].clone());
    }
    if kind == "site-audit" {
        scope.insert("operation".to_owned(), json!("bounded_site_audit"));
    }

    let coverage = coverage(document, &summary, kind)?;
    let backlog = backlog(document, project, kind, host)?;

    let (
        declared_findings_key,
        declared_findings,
        findings_declared,
        declared_pages_key,
        declared_pages,
        pages_declared,
        severity_key,
    ) = if kind == "site-audit" {
        (
            "findings_total",
            summary.get("findings_total").cloned(),
            summary.get("findings_total").is_some(),
            "pages_checked",
            summary.get("pages_checked").cloned(),
            summary.get("pages_checked").is_some(),
            "findings_by_severity",
        )
    } else {
        let totals = summary.get("totals").cloned().unwrap_or_else(|| json!({}));
        (
            "totals.issues_total",
            totals.get("issues_total").cloned(),
            totals.get("issues_total").is_some(),
            "totals.urls_crawled",
            totals.get("urls_crawled").cloned(),
            totals.get("urls_crawled").is_some(),
            "by_severity",
        )
    };
    let declared_severity = summary.get(severity_key).cloned();
    let severity_reported = declared_severity.as_ref().is_some_and(|v| v.is_object());

    let group_count = |name: &str| -> Value {
        let group = coverage["groups"].get(name).cloned().unwrap_or_else(
            || json!({"state": "unavailable", "source_count": Value::Null, "projected_count": 0}),
        );
        json!({
            "state": group["state"],
            "source_count": group["source_count"],
            "projected_count": group["projected_count"],
        })
    };

    let check_coverage = summary.get("check_coverage");
    let check_total_present =
        check_coverage.is_some_and(|c| c.is_object() && c.get("checks_total").is_some());
    let check_total = check_coverage
        .filter(|v| v.is_object())
        .and_then(|c| c.get("checks_total"))
        .cloned();
    // `run_id` takes `scan_uuid`, then `run_id`; a falsy first value falls through,
    // and only a missing value (`None`) triggers the contract.
    let run_id = if raw_run
        .get("scan_uuid")
        .is_some_and(|v| crate::py::truthy(Some(v)))
    {
        raw_run.get("scan_uuid").cloned()
    } else {
        raw_run.get("run_id").cloned()
    };
    let run_id = run_id.or_else(|| {
        summary
            .get("evidence_contract")
            .filter(|v| v.is_object())
            .and_then(|e| e.get("scan_uuid"))
            .cloned()
    });
    let source_schema = if kind == "site-audit" {
        document.get("schema")
    } else {
        document.get("schema_version")
    }
    .cloned();

    let mut source = Map::new();
    source.insert("kind".to_owned(), json!(kind));
    if kind == "sf-audit" {
        source.insert(
            "collector".to_owned(),
            raw_run.get("collector").cloned().unwrap_or(Value::Null),
        );
        source.insert(
            "input_mode".to_owned(),
            raw_run.get("input_mode").cloned().unwrap_or(Value::Null),
        );
    }
    source.insert("schema".to_owned(), source_schema.unwrap_or(Value::Null));
    source.insert(
        "domain".to_owned(),
        document
            .get("domain")
            .filter(|v| crate::py::truthy(Some(v)))
            .or_else(|| raw_run.get("project"))
            .cloned()
            .unwrap_or(Value::Null),
    );
    source.insert(
        "url".to_owned(),
        document
            .get("url")
            .filter(|v| crate::py::truthy(Some(v)))
            .or_else(|| raw_run.get("source"))
            .cloned()
            .unwrap_or(Value::Null),
    );
    source.insert(
        "generated_at".to_owned(),
        document
            .get("generated_at")
            .filter(|v| crate::py::truthy(Some(v)))
            .or_else(|| raw_run.get("generated_at"))
            .cloned()
            .unwrap_or(Value::Null),
    );
    source.insert("run_id".to_owned(), run_id.unwrap_or(Value::Null));
    source.insert(
        "input_diagnostics".to_owned(),
        Value::Array(diagnostics.to_vec()),
    );

    Ok(json!({
        "schema": SCHEMA,
        "source": source,
        "run": {
            "state": run_state,
            "validity": declared(crawl_valid.as_ref(), crawl_valid_present),
            "partial": declared(crawl_partial.as_ref(), crawl_partial_present),
            "scope": scope,
            "reasons": run_reasons,
        },
        "summary": {
            "source": summary,
            "counts": {
                "findings": {
                    "source_count": source_findings.len(),
                    "declared_total": declared(declared_findings.as_ref(), findings_declared),
                    "projected_count": source_findings.len(),
                    "declared_total_source_field": declared_findings_key,
                    "declared_by_severity": declared(declared_severity.as_ref(), severity_reported),
                },
                "pages": {
                    "source_count": source_pages.len(),
                    "declared_total": declared(declared_pages.as_ref(), pages_declared),
                    "projected_count": source_pages.len(),
                    "declared_total_source_field": declared_pages_key,
                },
                "checks": {
                    "declared_total": declared(check_total.as_ref(), check_total_present),
                    "ran": group_count(if kind == "site-audit" { "ran" } else { "fired" }),
                    "skipped": group_count("skipped"),
                    "failed": group_count("failed"),
                    "page_tools_failed": group_count("page_tools_failed"),
                    "disabled": group_count("disabled"),
                    "capabilities": group_count("capabilities"),
                    "silent": group_count("silent"),
                },
                "backlog": {
                    "state": backlog["state"],
                    "source_count": backlog["source_count"],
                    "declared_count": backlog["declared_count"],
                    "projected_count": backlog["projected_count"],
                },
            },
        },
        "coverage": coverage,
        "findings": findings,
        "pages": pages,
        "backlog": backlog,
        "omissions": [],
    }))
}

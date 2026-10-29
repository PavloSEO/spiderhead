//! Deterministic remediation report: JSON data and Markdown review file.
//!
//! `remediation_report`, `remediation_markdown` and `write_remediation_report`.

use std::io::Write;
use std::path::Path;

use serde_json::{json, Value};

use super::ingest::Record;
use super::schema::{ledger_error, LIFECYCLE_STATES};
use super::LedgerRef;
use crate::error::StoreError;

use super::queries::*;

/// One deterministic, bounded page of before/after evidence rows.
///
/// The summary always covers the complete ledger population. Exact cases are deliberately
/// paginated, so a million-row retained audit cannot turn a routine report read into a second
/// full in-memory audit export.
pub fn remediation_report(
    ledger: LedgerRef<'_>,
    limit: i64,
    offset: i64,
) -> Result<Record, StoreError> {
    let summary = remediation_summary(ledger)?;
    let cases = read_cases(ledger, &CaseFilter::default(), Some(limit), offset)?;
    let mut rows = Vec::new();
    for finding in cases["findings"].as_array().into_iter().flatten() {
        for occurrence in finding["occurrences"].as_array().into_iter().flatten() {
            let decisions = occurrence["decisions"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let observations = occurrence["observations"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            rows.push(json!({
                "finding_key": finding["finding_key"],
                "check": finding["check"],
                "subject": occurrence["subject_value"],
                "occurrence_key": occurrence["occurrence_key"],
                "baseline": observations.first().cloned().unwrap_or(Value::Null),
                "state": occurrence["current_state"],
                "decision": decisions.last().cloned().unwrap_or(Value::Null),
                "observations": observations,
            }));
        }
    }
    let mut out = Record::new();
    out.insert("schema_version".into(), json!("remediation-report.v1"));
    out.insert("summary".into(), Value::Object(summary));
    out.insert(
        "pagination".into(),
        json!({
            "total": cases["total"],
            "limit": cases["limit"],
            "offset": cases["offset"],
            "returned": cases["findings"].as_array().map_or(0, Vec::len),
        }),
    );
    out.insert("cases".into(), Value::Array(rows));
    Ok(out)
}

fn percent_text(value: Option<&Value>) -> String {
    match value {
        Some(v) if !v.is_null() => format!("{}%", py_str(v)),
        _ => "unavailable".into(),
    }
}

/// Render the retained remediation state without calculating or fetching evidence.
pub fn remediation_markdown(document: &Record) -> Result<String, StoreError> {
    if document.get("schema_version").and_then(Value::as_str) != Some("remediation-report.v1") {
        return Err(ledger_error("unsupported remediation report document"));
    }
    let (Some(summary), Some(cases), Some(pagination)) = (
        document.get("summary").and_then(Value::as_object),
        document.get("cases").and_then(Value::as_array),
        document.get("pagination").and_then(Value::as_object),
    ) else {
        return Err(ledger_error("invalid remediation report document"));
    };
    let counts = summary
        .get("counts")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let pg = |k: &str| py_str(pagination.get(k).unwrap_or(&Value::Null));
    let mut lines: Vec<String> = vec![
        "# Remediation evidence report".into(),
        String::new(),
        format!(
            "Ledger revision: {}",
            py_str(summary.get("ledger_revision").unwrap_or(&Value::Null))
        ),
        format!(
            "Cases: {} shown from {} (limit={}, offset={})",
            pg("returned"),
            pg("total"),
            pg("limit"),
            pg("offset")
        ),
        String::new(),
        "| State | Check | Subject | Evidence |".into(),
        "|---|---|---|---|".into(),
    ];
    for case in cases {
        let Some(case) = case.as_object() else {
            return Err(ledger_error("invalid remediation report case"));
        };
        let decision = case.get("decision").filter(|d| super::ingest::truthy(d));
        let verification = decision
            .and_then(|d| d.get("verification"))
            .filter(|v| v.is_object());
        let evidence = match (verification, decision) {
            (Some(v), _) => format!(
                "verification {} / {} / baseline {} / scope {}",
                py_str(&v["artifact_sha256"]),
                py_str(&v["outcome"]),
                py_str(&v["baseline_audit_sha256"]),
                py_str(&v["scope_sha256"])
            ),
            (None, Some(d)) => format!(
                "decision {} / observation {}",
                py_str(d.get("decision_id").unwrap_or(&Value::Null)),
                py_str(d.get("observation_id").unwrap_or(&Value::Null))
            ),
            (None, None) => "no lifecycle decision".into(),
        };
        let cells = [
            text_of(case.get("state").unwrap_or(&Value::Null)),
            text_of(case.get("check").unwrap_or(&Value::Null)),
            text_of(case.get("subject").unwrap_or(&Value::Null)),
            evidence,
        ];
        let cleaned: Vec<String> = cells
            .iter()
            .map(|c| c.replace('|', "\\|").replace('\n', " "))
            .collect();
        lines.push(format!("| {} |", cleaned.join(" | ")));
    }
    lines.push(String::new());
    lines.push(format!(
        "Counts: {}",
        LIFECYCLE_STATES
            .iter()
            .map(|s| format!("{s}={}", py_str(counts.get(*s).unwrap_or(&json!(0)))))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    lines.push(String::new());
    lines.push(format!(
        "Resolved percentage: {}",
        percent_text(summary.get("resolved_percent"))
    ));
    lines.push(format!(
        "Represented resolved percentage: {}",
        percent_text(summary.get("represented_resolved_percent"))
    ));
    lines.push(format!(
        "Rechecked percentage: {}",
        percent_text(summary.get("rechecked_percent"))
    ));
    lines.push(format!(
        "Represented rechecked percentage: {}",
        percent_text(summary.get("represented_rechecked_percent"))
    ));
    let scope_state = summary
        .get("scope")
        .and_then(|s| s.get("state"))
        .map(text_of)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());
    lines.push(format!("Membership scope: {scope_state}"));
    lines.push(String::new());
    if let Some(tasks) = summary
        .get("tasks")
        .and_then(Value::as_object)
        .filter(|t| !t.is_empty())
    {
        lines.push("## Task coverage".into());
        lines.push(String::new());
        lines.push("| Task | Status | Scope | Represented resolved | Full-scope resolved |".into());
        lines.push("|---|---|---|---:|---:|".into());
        let mut names: Vec<&String> = tasks.keys().collect();
        names.sort();
        for name in names {
            let task = &tasks[name];
            let field = |k: &str, fallback: &str| {
                let t = text_of(task.get(k).unwrap_or(&Value::Null));
                if t.is_empty() {
                    fallback.to_string()
                } else {
                    t
                }
            };
            lines.push(format!(
                "| {} |",
                [
                    name.clone(),
                    field("status", "unavailable"),
                    field("scope_state", "unknown"),
                    percent_text(task.get("represented_resolved_percent")),
                    percent_text(task.get("resolved_percent")),
                ]
                .join(" | ")
            ));
        }
        lines.push(String::new());
    }
    Ok(lines.join("\n"))
}

fn sorted(value: &Value) -> Value {
    match value {
        Value::Object(m) => {
            let mut entries: Vec<(&String, &Value)> = m.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(k, v)| (k.clone(), sorted(v)))
                    .collect(),
            )
        }
        Value::Array(a) => Value::Array(a.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

fn write_new(destination: &Path, name: &str, content: &str) -> Result<(), StoreError> {
    let temporary = destination.join(format!(".remediation-{}", uuid::Uuid::new_v4().simple()));
    let result = (|| -> Result<(), StoreError> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(content.as_bytes())?;
        file.flush()?;
        file.sync_all()?;
        std::fs::hard_link(&temporary, destination.join(name))?;
        Ok(())
    })();
    let _ = std::fs::remove_file(&temporary);
    result
}

/// Write one new JSON/Markdown report directory from existing ledger evidence.
///
/// The function never overwrites a report or mutates the ledger. A caller can retain this
/// directory alongside the ledger as an exact review snapshot.
pub fn write_remediation_report(
    ledger: LedgerRef<'_>,
    out_dir: &Path,
    limit: i64,
    offset: i64,
) -> Result<Record, StoreError> {
    let destination = std::path::absolute(out_dir)?;
    if destination.exists() {
        return Err(StoreError::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "remediation report output already exists: {}",
                destination.display()
            ),
        )));
    }
    std::fs::create_dir_all(&destination)?;
    let document = remediation_report(ledger, limit, offset)?;
    let json_path = destination.join("remediation.json");
    let markdown_path = destination.join("remediation.md");
    let pretty = serde_json::to_string_pretty(&sorted(&Value::Object(document.clone())))?;
    write_new(&destination, "remediation.json", &format!("{pretty}\n"))?;
    write_new(
        &destination,
        "remediation.md",
        &remediation_markdown(&document)?,
    )?;
    let mut out = Record::new();
    out.insert("ok".into(), json!(true));
    out.insert("report".into(), json!(markdown_path.to_string_lossy()));
    out.insert("data".into(), json!(json_path.to_string_lossy()));
    out.insert("summary".into(), document["summary"].clone());
    Ok(out)
}

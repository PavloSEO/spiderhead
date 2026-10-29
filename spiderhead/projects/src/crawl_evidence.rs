//! Turn a project crawl into checklist evidence for the automatic checks it
//! measured.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::coverage::{coverage_status, record_execution};
use crate::error::{value, ProjectsError, ProjectsResult};
use crate::schedule::falsy;

fn check_items(status: &Value) -> Vec<Value> {
    status
        .get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|item| {
                    item.get("kind").and_then(Value::as_str) == Some("check")
                        && !falsy(item.get("enabled").unwrap_or(&Value::Null))
                        && !matches!(
                            item.get("applicability").and_then(Value::as_str),
                            Some("excluded") | Some("not_agreed")
                        )
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn saved_audit(path: &Path) -> ProjectsResult<Value> {
    let con = spiderhead_store::scan_artifact::open_scan(path, true, None)
        .map_err(ProjectsError::Store)?;
    let document: Option<String> = con
        .query_row(
            "SELECT document_json FROM audit WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .ok();
    let raw = document.ok_or_else(|| value("'NoneType' object is not subscriptable"))?;
    serde_json::from_str(&raw).map_err(ProjectsError::Json)
}

fn resolve(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|_| std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()))
}

fn key_error(key: &str) -> ProjectsError {
    value(format!("'{key}'"))
}

/// Mark every automatic check failed when a crawl did not finish.
pub fn record_failure(root: &Path, reason: &str) -> ProjectsResult<Value> {
    let mut status = coverage_status(root, true)?;
    if status.get("items").is_none() {
        return Ok(serde_json::json!({
            "skipped": status.get("reason").cloned().unwrap_or_else(|| Value::String("checklist not initialized".into()))
        }));
    }
    let items = check_items(&status);
    let mut failed = Vec::new();
    let mut refused = Vec::new();
    for item in &items {
        if item.get("execution_kind").and_then(Value::as_str) != Some("automatic") {
            continue;
        }
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| key_error("id"))?;
        let entry = serde_json::json!({
            "status": "failed",
            "reason": reason.chars().take(500).collect::<String>(),
        });
        let revision = status
            .get("revision")
            .and_then(Value::as_i64)
            .ok_or_else(|| key_error("revision"))?;
        match record_execution(root, id, &entry, revision) {
            Ok(updated) => {
                status = updated;
                failed.push(id.to_string());
            }
            Err(ProjectsError::Value(message)) => {
                refused.push(serde_json::json!({"id": id, "reason": message}));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(serde_json::json!({"failed": failed, "refused": refused}))
}

/// Record a saved project crawl as evidence for the automatic checks it measured
pub fn record_scan(root: &Path, scan: &Path) -> ProjectsResult<Value> {
    let mut status = coverage_status(root, true)?;
    if status.get("items").is_none() {
        return Ok(serde_json::json!({
            "skipped": status.get("reason").cloned().unwrap_or_else(|| Value::String("checklist not initialized".into()))
        }));
    }
    let resolved_root = resolve(root);
    let relative_and_audit = resolve(scan)
        .strip_prefix(&resolved_root)
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .map_err(|_| value("saved crawl is outside the project"))
        .and_then(|relative| saved_audit(scan).map(|audit| (relative, audit)));
    let (relative, audit) = match relative_and_audit {
        Ok(pair) => pair,
        Err(error) => {
            return record_failure(root, &format!("saved crawl has no usable audit: {error}"));
        }
    };
    let run = audit.get("run").cloned().unwrap_or(Value::Null);
    let coverage = audit
        .get("summary")
        .and_then(|summary| summary.get("check_coverage"))
        .cloned()
        .unwrap_or(Value::Null);
    let mut executed: Vec<String> = coverage
        .get("checks_silent_ids")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if let Some(issues) = audit.get("issues").and_then(Value::as_array) {
        for issue in issues {
            if let Some(check) = issue.get("check").and_then(Value::as_str) {
                executed.push(check.to_string());
            }
        }
    }
    let mut unavailable: Vec<(String, String)> = Vec::new();
    for key in ["checks_skipped", "checks_disabled"] {
        if let Some(rows) = run.get(key).and_then(Value::as_array) {
            for row in rows {
                let id = crate::pyjson::dumps_compact(row.get("id").unwrap_or(&Value::Null));
                let reason = row
                    .get("reason")
                    .cloned()
                    .filter(|r| !falsy(r))
                    .map(|r| match r {
                        Value::String(text) => text,
                        other => crate::bindings::py_str(&other),
                    })
                    .unwrap_or_else(|| "check unavailable in the crawl".to_string());
                if let Some(existing) = unavailable.iter_mut().find(|(k, _)| k == &id) {
                    existing.1 = reason;
                } else {
                    unavailable.push((id.to_string(), reason));
                }
            }
        }
    }
    if let Some(ids) = coverage
        .get("checks_disabled_ids")
        .and_then(Value::as_array)
    {
        for raw in ids {
            let id = crate::pyjson::dumps_compact(raw);
            if !unavailable.iter().any(|(k, _)| k == &id) {
                unavailable.push((id, "check disabled for the crawl".to_string()));
            }
        }
    }
    let items = check_items(&status);
    let mut recorded = Vec::new();
    let mut unavailable_items = Vec::new();
    let mut unmeasured = Vec::new();
    let mut refused = Vec::new();
    for item in &items {
        if item.get("execution_kind").and_then(Value::as_str) != Some("automatic") {
            continue;
        }
        let item_id = item
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| key_error("id"))?;
        let check_id = item_id.strip_prefix("check:").unwrap_or(item_id);
        let entry = if executed.iter().any(|id| id == check_id) {
            serde_json::json!({
                "status": "succeeded",
                "reason": "Executed in the saved project crawl",
                "artifact": relative,
            })
        } else if let Some((_, reason)) = unavailable
            .iter()
            .find(|(k, _)| k == &crate::pyjson::dumps_compact(&Value::String(check_id.into())))
        {
            serde_json::json!({
                "status": "unavailable",
                "reason": reason.chars().take(500).collect::<String>(),
            })
        } else {
            unmeasured.push(item_id.to_string());
            continue;
        };
        let succeeded = entry.get("status").and_then(Value::as_str) == Some("succeeded");
        let revision = status
            .get("revision")
            .and_then(Value::as_i64)
            .ok_or_else(|| key_error("revision"))?;
        match record_execution(root, item_id, &entry, revision) {
            Ok(updated) => status = updated,
            Err(ProjectsError::Value(message)) => {
                refused.push(serde_json::json!({"id": item_id, "reason": message}));
                continue;
            }
            Err(error) => return Err(error),
        }
        if succeeded {
            recorded.push(item_id.to_string());
        } else {
            unavailable_items.push(item_id.to_string());
        }
    }
    Ok(serde_json::json!({
        "succeeded": recorded,
        "unavailable": unavailable_items,
        "unmeasured": unmeasured,
        "refused": refused,
    }))
}

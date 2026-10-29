//! Project checklist coverage for report writers.
//!
//! Binds a `spiderhead.site-audit/1` report to a saved project and renders the recorded
//! checklist state. Project DB access would cross a layer boundary (projects depend on
//! reports), so live and status paths report `unavailable` rather than computing state.
//! The destination guards below are pure filesystem work.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use thiserror::Error;

use crate::document::{safe_segment, AuditV2Rows};
use crate::py;

/// Error raised when project coverage cannot be attached.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct CoverageError(pub String);

/// Compact sorted-key JSON for containers; plain text for scalars.
pub fn value_text(value: Option<&Value>) -> String {
    fn dumps(value: &Value) -> String {
        match value {
            Value::Null => "null".to_owned(),
            Value::Bool(flag) => flag.to_string(),
            Value::Number(n) => n.to_string(),
            Value::String(text) => serde_json::to_string(text).unwrap_or_default(),
            Value::Array(items) => format!(
                "[{}]",
                items.iter().map(dumps).collect::<Vec<_>>().join(",")
            ),
            Value::Object(map) => {
                let sorted: BTreeMap<&String, &Value> = map.iter().collect();
                let inner: Vec<String> = sorted
                    .iter()
                    .map(|(k, v)| {
                        format!(
                            "{}:{}",
                            serde_json::to_string(k).unwrap_or_default(),
                            dumps(v)
                        )
                    })
                    .collect();
                format!("{{{}}}", inner.join(","))
            }
        }
    }
    match value {
        Some(Value::Null) | None => String::new(),
        Some(v @ (Value::Object(_) | Value::Array(_))) => dumps(v),
        Some(v) => py::str_value(v),
    }
}

/// Priority display: recorded label, else level-mapped fallback.
pub fn priority_text(item: &Value) -> String {
    let label = value_text(py::get(item, "priority_label"));
    if !label.is_empty() {
        return label;
    }
    let level = py::get(item, "priority_level");
    if !py::truthy(level) {
        return String::new();
    }
    format!("P{}", py::str_value(level.unwrap_or(&Value::Null)))
}

/// audit.v2 coverage: counts from the declared `/checklist` collection rows.
///
/// Rows are `{id, title, kind, execution_kind, priority, state, ...}` objects from the
/// manifest; an unavailable collection gives an empty status.
pub fn coverage_from_rows(checklist_rows: &AuditV2Rows<'_>) -> Value {
    let mut counts: Map<String, Value> = Map::new();
    let mut items: Vec<Value> = Vec::new();
    for item in checklist_rows.to_vec().unwrap_or_default() {
        let state = py::get(&item, "state")
            .map(py::str_value)
            .unwrap_or_else(|| "unknown".into());
        let counter = counts.entry(state.clone()).or_insert(json!(0));
        if let Some(n) = counter.as_i64() {
            *counter = json!(n + 1);
        }
        items.push(item);
    }
    json!({
        "state": "recorded",
        "counts": counts,
        "items": items,
    })
}

/// Stream the coverage block to an open ZIP as a companion JSON part.
pub fn write_audit_v2<W: Write + std::io::Seek>(
    zip: &mut zip::ZipWriter<W>,
    document: &Value,
) -> Result<(), CoverageError> {
    let coverage = py::get_map(
        document.get("summary").unwrap_or(&Value::Null),
        "project_coverage",
    );
    let payload = serde_json::to_vec(&json!({
        "schema": "project-coverage/1",
        "domain": py::get(document, "domain").cloned().unwrap_or(json!("")),
        "project": coverage.get("project").cloned().unwrap_or(json!({})),
        "status": coverage.get("status").cloned().unwrap_or(json!({})),
    }))
    .map_err(|e| CoverageError(e.to_string()))?;
    let domain = py::str_value(py::get(document, "domain").unwrap_or(&Value::Null));
    zip.start_file(
        format!(
            "project_coverage.{}.json",
            safe_segment(&serde_json::json!(domain))
        ),
        zip::write::FileOptions::<()>::default(),
    )
    .map_err(|e| CoverageError(e.to_string()))?;
    zip.write_all(&payload)
        .map_err(|e| CoverageError(e.to_string()))?;
    Ok(())
}

/// Canonicalize the deepest existing ancestor, then append the remaining components lexically.
fn resolve(path: &Path) -> PathBuf {
    let mut probe = path.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(canon) = std::fs::canonicalize(&probe) {
            let mut out = canon;
            for part in tail.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match probe.file_name() {
            Some(name) => {
                tail.push(name.to_os_string());
                probe.pop();
            }
            None => return probe,
        }
    }
}

/// `protected_project_paths`: immutable project controls plus every evidence
/// artifact still referenced by a checklist record.
pub fn protected_project_paths(root: &Path) -> BTreeSet<PathBuf> {
    let resolved = resolve(root);
    let mut protected = BTreeSet::from([
        resolved.join("project.json"),
        resolved.join("coverage.json"),
    ]);
    let coverage = resolved.join("coverage.json");
    if !coverage.is_file() {
        return protected;
    }
    let document: Value = match std::fs::read_to_string(&coverage)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
    {
        Some(value) => value,
        None => return protected,
    };
    let Some(items) = document.get("items").and_then(Value::as_object) else {
        return protected;
    };
    for item in items.values() {
        let Some(records) = item.get("records").and_then(Value::as_array) else {
            continue;
        };
        for record in records {
            let Some(artifact) = record.get("artifact").and_then(Value::as_str) else {
                continue;
            };
            let relative = Path::new(artifact);
            if relative.is_absolute()
                || relative
                    .components()
                    .any(|c| c == std::path::Component::ParentDir)
            {
                continue;
            }
            protected.insert(resolve(&resolved.join(relative)));
        }
    }
    protected
}

/// `protected_destination`: explain whether a report would replace project
/// control files or saved evidence. Returns `None` when every target is safe.
pub fn protected_destination(root: &Path, targets: &[PathBuf]) -> Option<String> {
    let protected = protected_project_paths(root);
    for target in targets {
        if protected.contains(&resolve(target)) {
            return Some(
                "report output must not overwrite project control files or referenced evidence"
                    .to_owned(),
            );
        }
    }
    None
}

/// `projects.workspace.open_project` — the workspace's open document, or `str(exc)`.
pub type ProjectOpener<'a> = dyn Fn(&str) -> Result<Value, String> + 'a;
/// `projects.workspace.project_status` — `{ok, checklist, ...}` for the root, or `str(exc)`.
pub type ProjectStatus<'a> = dyn Fn(&str) -> Result<Value, String> + 'a;

/// `finding_views.apply_view_details` — the saved-view projection
/// `(view_result, selected_rows)` for one document, or `str(exc)`.
pub type ApplyViewDetails<'a> =
    dyn Fn(&str, &str, &Value, i64) -> Result<(Value, Vec<Value>), String> + 'a;

/// The workspace reads a project-bound report performs.
///
/// The projects crate depends on this one, so the reads arrive injected rather
/// than imported — binding to a live project stays explicit, never a silent
/// summary fallback.
#[derive(Clone, Copy)]
pub struct ProjectsHost<'a> {
    /// `workspace.open_project`.
    pub open_project: &'a ProjectOpener<'a>,
    /// `workspace.project_status`.
    pub project_status: &'a ProjectStatus<'a>,
    /// `finding_views.apply_view_details`.
    pub apply_view_details: &'a ApplyViewDetails<'a>,
}

/// The audit's own site identity, the field a project must match.
fn audit_domain(document: &Value, kind: &str) -> String {
    if kind == "sf-audit" {
        let empty = Value::Object(Map::new());
        let run = document.get("run").unwrap_or(&empty);
        return crate::facts::crawl_domain(run);
    }
    if let Some(url) = document.get("url").and_then(Value::as_str) {
        let host = url::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_default();
        if !host.is_empty() {
            return spiderhead_core::net::normalize_domain(&host);
        }
    }
    // `str(document.get("domain") or "")` — falsy values collapse to "".
    let domain = document
        .get("domain")
        .filter(|d| crate::py::truthy(Some(d)))
        .cloned()
        .unwrap_or(Value::Null);
    spiderhead_core::net::normalize_domain(&crate::py::str_value(&domain))
}

/// Bind a report to one project workspace.
///
/// `load_snapshot`: open the workspace, require the audit's own site identity to
/// match the project site, and return the root plus the checklist snapshot the
/// human writers embed read-only.
pub fn load_snapshot(
    project: &str,
    document: &Value,
    kind: &str,
    host: &ProjectsHost<'_>,
) -> Result<(String, Value), CoverageError> {
    let opened = (host.open_project)(project).map_err(CoverageError)?;
    let project_document = opened.get("project").cloned().unwrap_or(Value::Null);
    let subscript = |key: &str| -> Result<Value, CoverageError> {
        project_document
            .get("site")
            .and_then(|s| s.get(key))
            .cloned()
            .ok_or_else(|| CoverageError(format!("KeyError: '{key}'")))
    };
    let actual = audit_domain(document, kind);
    let expected_value = subscript("host")?;
    let expected = crate::py::str_value(&expected_value);
    if actual.is_empty() {
        return Err(CoverageError(
            "audit has no discoverable site identity for the requested project".to_owned(),
        ));
    }
    if actual != expected {
        return Err(CoverageError(format!(
            "project site {} does not match audit source identity {}",
            crate::py::repr(&expected_value),
            crate::py::repr(&json!(actual)),
        )));
    }
    let path = match opened.get("path").and_then(Value::as_str) {
        Some(path) => path.to_owned(),
        None => return Err(CoverageError("KeyError: 'path'".to_owned())),
    };
    let status = (host.project_status)(&path).map_err(CoverageError)?;
    let snapshot = status.get("checklist").cloned().unwrap_or(Value::Null);
    Ok((
        path,
        json!({
            "project": {
                "uuid": project_document.get("project_uuid").cloned().unwrap_or(Value::Null),
                "site": subscript("target")?,
            },
            "status": snapshot,
        }),
    ))
}

/// `attach_snapshot`: copy the document with the snapshot embedded read-only.
pub fn attach_snapshot(document: &Value, snapshot: &Value) -> Value {
    let mut projected = document.clone();
    let mut summary = document
        .get("summary")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    summary.insert("project_coverage".to_owned(), snapshot.clone());
    projected["summary"] = Value::Object(summary);
    projected
}

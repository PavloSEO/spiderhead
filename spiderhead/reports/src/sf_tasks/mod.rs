//! Prioritized task backlog derived from `audit.json`.
//!
//! A thin, configurable pipeline that groups issues into work items, maps severity to
//! priority and effort, caps URL lists and emits both `tasks.json` and `tasks.md`.
//! The `tasks_pipeline` defaults are kept local: the analysis crate is not a dependency here.

mod grouping;
mod render;

pub use render::render_tasks_md;

use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use spiderhead_formats::check_registry::{builtin as check_registry, CheckMeta, Severity};
use thiserror::Error;

use crate::client_findings::reproduction;
use crate::facts::crawl_domain;
use crate::py;
use crate::segments;

/// Configuration or I/O failure of the tasks pipeline.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct TasksError(pub String);

/// Priority order for the sorted backlog; unknown priorities sort last.
pub(crate) const PRIORITY_ORDER: [(&str, i64); 4] = [("P1", 0), ("P2", 1), ("P3", 2), ("P4", 3)];
/// Checks whose tasks carry a bounded broken-link location list.
pub(crate) const LINK_CHECKS: [&str; 3] = [
    "BROKEN_INTERNAL_LINK",
    "LINK_TO_5XX",
    "INTERNAL_LINK_TO_REDIRECT",
];
/// Assignment kinds a declared task assignment may carry.
const ASSIGNMENT_KINDS: [&str; 2] = ["template", "component"];
/// Assignment states a declared task assignment may carry.
const ASSIGNMENT_STATES: [&str; 2] = ["declared", "confirmed"];

/// The default pipeline table.
fn default_pipeline() -> Value {
    json!({
        "include_severities": ["critical", "warning", "notice", "opportunity"],
        "group_by": "check",
        "priority_map": {"critical": "P1", "warning": "P2", "notice": "P3", "opportunity": "P3"},
        "effort_map": {"critical": "high", "warning": "medium", "notice": "low", "opportunity": "low"},
        "max_urls_per_task": 25,
        "min_occurrences": 1,
        "include_checks": [],
        "exclude_checks": [],
        "assignments": [],
    })
}

/// The default table with the caller's `tasks_pipeline` config merged over it; new keys
/// append in caller order.
pub(crate) fn pipeline_cfg(config: Option<&Value>) -> Value {
    let mut base = default_pipeline().as_object().cloned().unwrap_or_default();
    if let Some(cfg) = config
        .filter(|c| py::truthy(Some(*c)))
        .and_then(|c| c.get("tasks_pipeline"))
        .and_then(Value::as_object)
    {
        for (key, value) in cfg {
            base.insert(key.clone(), value.clone());
        }
    }
    Value::Object(base)
}

/// `task_id`: `TASK-<sha1(check|key)[:8]>`.
pub(crate) fn task_id(check: &str, key: &str) -> String {
    use sha1::Digest;
    let digest = sha1::Sha1::digest(format!("{check}|{key}").as_bytes());
    format!("TASK-{}", format!("{digest:x}")[..8].to_owned())
}

/// `reproductions`: deduplicated, capped reproduction rows without tool labels.
pub(crate) fn reproductions(issues: &[&Value], cap: usize) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut rows = Vec::new();
    for issue in issues {
        let finding = json!({
            "url": issue.get("target_url"),
            "status_code": issue.get("status_code"),
            "locations": issue.get("locations"),
            "details": issue.get("details"),
            "text": issue.get("message"),
        });
        let row = reproduction(&finding, "");
        if seen.insert(row.clone()) {
            rows.push(row);
        }
        if rows.len() >= cap {
            break;
        }
    }
    rows
}

/// `evidence_references`: closed saved-observation references only — absent or
/// incomplete contracts stay a named `unavailable` record.
pub(crate) fn evidence_references(issues: &[&Value], cap: usize) -> (Vec<Value>, usize) {
    let mut rows: Vec<Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for issue in issues {
        let evidence = issue.get("evidence").and_then(Value::as_object);
        let contract = evidence
            .and_then(|e| e.get("contract"))
            .and_then(Value::as_object);
        let state = contract
            .and_then(|c| c.get("state"))
            .and_then(Value::as_str);
        let record = if matches!(state, Some("measured") | Some("imported_projection")) {
            let empty_contract = Map::new();
            let c = contract.unwrap_or(&empty_contract);
            let entry = json!({
                "state": str_of(c.get("state")),
                "id": truthy_str(c.get("id")),
                "source_table": truthy_str(c.get("source_table")),
                "observation_id": truthy_str(c.get("observation_id")),
            });
            if entry
                .as_object()
                .map(|m| m.values().all(|v| py::truthy(Some(v))))
                .unwrap_or(false)
            {
                entry
            } else {
                json!({
                    "state": "unavailable",
                    "reason": "saved evidence reference is incomplete",
                })
            }
        } else {
            let reason = contract
                .and_then(|c| c.get("reason"))
                .filter(|r| py::truthy(Some(*r)))
                .map(str_of_opt)
                .unwrap_or_else(|| "no stable saved-evidence reference is present".to_owned());
            json!({
                "state": "unavailable",
                "reason": reason,
            })
        };
        let key: Vec<String> = ["state", "id", "source_table", "observation_id", "reason"]
            .iter()
            .map(|name| str_of(record.get(*name)))
            .collect();
        if seen.insert(key) {
            rows.push(record);
        }
    }
    let truncated = rows.len().saturating_sub(cap);
    rows.truncate(cap);
    (rows, truncated)
}

/// Text of a contract cell: `None` or absent becomes `""`; other values keep their text.
fn str_of(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(v) => str_of_opt(v),
    }
}

fn str_of_opt(value: &Value) -> String {
    match value {
        Value::Null => "None".to_owned(),
        Value::Object(_) | Value::Array(_) => py::repr(value),
        v => py::str_value(v),
    }
}

/// Text of a named contract cell: falsy cells, including `0` and `false`, become `""`.
fn truthy_str(value: Option<&Value>) -> String {
    match value.filter(|v| py::truthy(Some(*v))) {
        None => String::new(),
        Some(v) => str_of_opt(v),
    }
}

/// `check_meta(check)` for task titles/hints: registry meta or the documented
/// fallback (`id`/`message` = the raw check id, `fix` absent).
pub(crate) fn check_meta_fallback(check: &str) -> CheckMeta {
    match check_registry() {
        Ok(registry) => registry.meta_or_fallback(check),
        Err(_) => CheckMeta {
            id: check.to_owned(),
            severity: Severity::Notice,
            source: "SF-derived".to_owned(),
            message: check.to_owned(),
            fix: None,
            doc_url: None,
        },
    }
}

/// `unassigned_assignment`: the closed fallback assignment record.
pub(crate) fn unassigned_assignment(reason: &str) -> Value {
    json!({
        "id": "unassigned",
        "name": "Unassigned",
        "kind": "unknown",
        "state": "unassigned",
        "rationale": reason,
        "provenance": {"kind": "unassigned", "reason": reason},
    })
}

/// `declared_assignments`: the closed URL→assignment map from
/// `tasks_pipeline.assignments`, validated with the same rules as the assignment reader.
pub(crate) fn declared_assignments(cfg: &Value) -> Result<IndexMap<String, Value>, TasksError> {
    let raw = cfg.get("assignments").cloned().unwrap_or_else(|| json!([]));
    let Value::Array(items) = raw else {
        return Err(TasksError(
            "tasks_pipeline.assignments must be a list".to_owned(),
        ));
    };
    let mut result: IndexMap<String, Value> = IndexMap::new();
    for (index, item) in items.iter().enumerate() {
        let Some(item) = item.as_object() else {
            return Err(TasksError(
                "each task assignment must be an object".to_owned(),
            ));
        };
        let name = item.get("name").and_then(Value::as_str);
        let kind = item.get("kind").and_then(Value::as_str);
        let rationale = item.get("rationale").and_then(Value::as_str);
        let urls = item.get("urls");
        let state = item
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("declared");
        if !name.is_some_and(|n| !n.trim().is_empty()) {
            return Err(TasksError(
                "task assignment name must be a non-empty string".to_owned(),
            ));
        }
        if !kind.is_some_and(|k| ASSIGNMENT_KINDS.contains(&k)) {
            return Err(TasksError(
                "task assignment kind must be template or component".to_owned(),
            ));
        }
        if !rationale.is_some_and(|r| !r.trim().is_empty()) {
            return Err(TasksError(
                "task assignment rationale must be a non-empty string".to_owned(),
            ));
        }
        if !ASSIGNMENT_STATES.contains(&state) {
            return Err(TasksError(
                "task assignment state must be declared or confirmed".to_owned(),
            ));
        }
        if state == "confirmed"
            && !["confirmed_by", "confirmation"].iter().all(|key| {
                item.get(*key)
                    .and_then(Value::as_str)
                    .is_some_and(|v| !v.trim().is_empty())
            })
        {
            return Err(TasksError(
                "a confirmed task assignment needs confirmed_by and confirmation".to_owned(),
            ));
        }
        let valid_urls = urls.and_then(Value::as_array).is_some_and(|list| {
            !list.is_empty()
                && list
                    .iter()
                    .all(|u| u.as_str().is_some_and(|s| !s.is_empty()))
        });
        if !valid_urls {
            return Err(TasksError(
                "task assignment urls must be a non-empty list of URLs".to_owned(),
            ));
        }
        let name = name.unwrap_or_default();
        let kind = kind.unwrap_or_default();
        let mut assignment = json!({
            "id": format!("operator:{kind}:{name}"),
            "name": name,
            "kind": kind,
            "state": state,
            "rationale": rationale.unwrap_or_default(),
            "provenance": {
                "kind": "operator_assignment",
                "source": "tasks_pipeline.assignments",
                "index": index,
            },
        });
        if state == "confirmed" {
            let obj = assignment.as_object_mut().unwrap_or_else(|| unreachable!());
            obj.insert("confirmed_by".to_owned(), item["confirmed_by"].clone());
            obj.insert("confirmation".to_owned(), item["confirmation"].clone());
        }
        for url_value in urls.and_then(Value::as_array).into_iter().flatten() {
            let url = url_value.as_str().unwrap_or_default().to_owned();
            if result.contains_key(&url) {
                return Err(TasksError(format!(
                    "task assignment URL appears more than once: {url}"
                )));
            }
            result.insert(url, assignment.clone());
        }
    }
    Ok(result)
}

/// `audit_segment_definitions`: the crawl's validated segment format —
/// `analysis.segments` verbatim, else `scope.segments` rewritten to rules.
fn audit_segment_definitions(audit: &Value) -> Vec<Value> {
    let empty = Map::new();
    let config = audit
        .get("run")
        .and_then(|r| r.get("crawl_config"))
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    if let Some(analysis) = config
        .get("analysis.segments")
        .and_then(Value::as_array)
        .filter(|rows| !rows.is_empty())
    {
        return analysis.clone();
    }
    let Some(scope) = config.get("scope.segments").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut definitions = Vec::new();
    for item in scope {
        let Some(item) = item.as_object() else {
            return Vec::new();
        };
        let Some(name) = item.get("name").and_then(Value::as_str) else {
            return Vec::new();
        };
        let mut rules = Vec::new();
        if let Some(prefix) = item.get("prefix").filter(|v| py::truthy(Some(*v))) {
            rules.push(json!({"op": "prefix", "field": "path", "value": prefix}));
        }
        if let Some(host) = item.get("host").filter(|v| py::truthy(Some(*v))) {
            rules.push(json!({"op": "eq", "field": "host", "value": host}));
        }
        if let Some(pattern) = item.get("pattern").filter(|v| py::truthy(Some(*v))) {
            rules.push(json!({"op": "regex", "field": "url", "value": pattern}));
        }
        if rules.is_empty() {
            return Vec::new();
        }
        definitions.push(json!({"name": name, "rules": rules}));
    }
    definitions
}

/// Split a URL into the parts segment rules read: `path` and lowercased `hostname`.
/// Unparseable input still yields a path rather than an error.
fn split_path_host(url: &str) -> (String, String) {
    if let Ok(parsed) = url::Url::parse(url) {
        let host = parsed.host_str().unwrap_or_default().to_lowercase();
        return (parsed.path().to_owned(), host);
    }
    // urlsplit accepts almost anything: strip fragment/query, then treat the
    // remainder as a path when no `//authority` prefix is present.
    let body = url.split(['#', '?']).next().unwrap_or_default();
    match body.find("://") {
        Some(at) => {
            let rest = &body[at + 3..];
            match rest.find('/') {
                Some(slash) => (rest[slash..].to_owned(), rest[..slash].to_lowercase()),
                None => (String::new(), rest.to_lowercase()),
            }
        }
        None => (body.to_owned(), String::new()),
    }
}

/// `segment_assignments`: candidate segment assignments from the audit's own
/// validated rules — never a claimed template match on partial evidence.
fn segment_assignments(audit: &Value) -> Result<IndexMap<String, Value>, TasksError> {
    let definitions = audit_segment_definitions(audit);
    if definitions.is_empty() {
        return Ok(IndexMap::new());
    }
    let mut pages = Vec::new();
    if let Some(list) = audit.get("pages").and_then(Value::as_array) {
        for page in list {
            let Some(url) = page.get("url").and_then(Value::as_str) else {
                continue;
            };
            if url.is_empty() {
                continue;
            }
            let (path, host) = split_path_host(url);
            let mut record = page.clone();
            let obj = record.as_object_mut().unwrap_or_else(|| unreachable!());
            obj.insert("path".to_owned(), Value::String(path));
            obj.insert("host".to_owned(), Value::String(host));
            pages.push(record);
        }
    }
    let assigned = segments::assign_segments(&pages, &definitions).map_err(|e| TasksError(e.0))?;
    let empty = Map::new();
    let primary = assigned
        .get("primary")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let partial = py::truthy(audit.get("run").and_then(|r| r.get("crawl_partial")));
    let mut targets = Vec::new();
    if let Some(issues) = audit.get("issues").and_then(Value::as_array) {
        for issue in issues {
            if let Some(target) = issue.get("target_url").and_then(Value::as_str) {
                if !target.is_empty() && !targets.contains(&target) {
                    targets.push(target);
                }
            }
        }
    }
    let mut result: IndexMap<String, Value> = IndexMap::new();
    for target in targets {
        let mut name = primary
            .get(target)
            .and_then(Value::as_str)
            .map(str::to_owned);
        // A target absent from a complete retained page set is unassigned. For
        // a partial or page-less audit the same rules make a candidate only.
        if name.is_none() && (pages.is_empty() || partial) {
            let (path, host) = split_path_host(target);
            let single = vec![json!({ "url": target, "path": path, "host": host })];
            let single_assigned =
                segments::assign_segments(&single, &definitions).map_err(|e| TasksError(e.0))?;
            name = single_assigned
                .get("primary")
                .and_then(|p| p.get(target))
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        let Some(name) = name.filter(|n| n != segments::UNSEGMENTED) else {
            continue;
        };
        let rules = definitions
            .iter()
            .find(|item| item.get("name").and_then(Value::as_str) == Some(name.as_str()))
            .and_then(|item| item.get("rules"))
            .cloned()
            .unwrap_or_else(|| json!([]));
        result.insert(
            target.to_owned(),
            json!({
                "id": format!("segment:{name}"),
                "name": name,
                "kind": "segment",
                "state": "candidate",
                "rationale": "Matched the audit's validated segment rule; shared implementation remains unconfirmed.",
                "provenance": {
                    "kind": "audit_segment_rules",
                    "schema_version": segments::SCHEMA_VERSION,
                    "rules": rules,
                },
            }),
        );
    }
    Ok(result)
}

/// `build_tasks`: the task backlog of an `audit.json` document.
pub fn build_tasks(audit: &Value, config: Option<&Value>) -> Result<Value, TasksError> {
    let cfg = pipeline_cfg(config);
    let sev_filter = cfg
        .get("include_severities")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let include = cfg
        .get("include_checks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let exclude = cfg
        .get("exclude_checks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let cap = cfg
        .get("max_urls_per_task")
        .and_then(py::to_i64)
        .unwrap_or_default()
        .max(0) as usize;
    // Locations have their own cap, so the URL cap cannot silently truncate them.
    let loc_cap = cfg
        .get("max_locations_per_task")
        .and_then(py::to_i64)
        .unwrap_or(25)
        .max(0) as usize;
    let min_occ = cfg
        .get("min_occurrences")
        .and_then(py::to_i64)
        .unwrap_or_default();
    let group_by = cfg
        .get("group_by")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let prio = cfg
        .get("priority_map")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let effort = cfg.get("effort_map").cloned().unwrap_or_else(|| json!({}));

    let issues: Vec<&Value> = audit
        .get("issues")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|issue| {
            let severity = issue
                .get("severity")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let check = issue
                .get("check")
                .and_then(Value::as_str)
                .unwrap_or_default();
            sev_filter.iter().any(|s| s.as_str() == Some(severity))
                && (include.is_empty() || include.iter().any(|c| c.as_str() == Some(check)))
                && !exclude.iter().any(|c| c.as_str() == Some(check))
        })
        .collect();

    let mut grouping: Option<Value> = None;
    let mut tasks = match group_by {
        "check" => grouping::group_by_check(&issues, &prio, &effort, cap, loc_cap, min_occ),
        "issue" => grouping::per_issue(&issues, &prio, &effort, cap, loc_cap, min_occ),
        "check_assignment" => {
            let declared = declared_assignments(&cfg)?;
            let has_declared = !declared.is_empty();
            let assignments = if has_declared {
                declared
            } else {
                segment_assignments(audit)?
            };
            let source = if has_declared {
                "operator_assignments"
            } else if !assignments.is_empty() {
                "audit_segments"
            } else {
                "none"
            };
            let tasks = grouping::group_by_check_assignment(
                &issues,
                &assignments,
                &prio,
                &effort,
                cap,
                loc_cap,
                min_occ,
            );
            let assignment_count: usize = assignments
                .values()
                .filter_map(|a| a.get("id").and_then(Value::as_str))
                .collect::<std::collections::HashSet<_>>()
                .len();
            let unassigned_findings = issues
                .iter()
                .filter(|issue| {
                    issue
                        .get("target_url")
                        .and_then(Value::as_str)
                        .is_none_or(|url| !assignments.contains_key(url))
                })
                .count();
            grouping = Some(json!({
                "mode": source,
                "assignment_count": assignment_count,
                "unassigned_findings": unassigned_findings,
            }));
            tasks
        }
        _ => {
            return Err(TasksError(
                "tasks_pipeline.group_by must be check, issue, or check_assignment".to_owned(),
            ))
        }
    };
    tasks.sort_by(|a, b| {
        let key = |task: &Value| {
            let prio = task
                .get("priority")
                .and_then(Value::as_str)
                .and_then(|p| {
                    PRIORITY_ORDER
                        .iter()
                        .find(|(k, _)| *k == p)
                        .map(|(_, v)| *v)
                })
                .unwrap_or(9);
            let affected = task
                .get("affected_count")
                .and_then(py::to_i64)
                .unwrap_or_default();
            (prio, -affected)
        };
        key(a).cmp(&key(b))
    });

    let mut by_priority: Map<String, Value> = Map::new();
    for task in &tasks {
        let prio = task
            .get("priority")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let entry = by_priority.entry(prio).or_insert_with(|| Value::from(0));
        if let Some(count) = entry.as_i64() {
            *entry = Value::from(count + 1);
        }
    }

    let run = audit.get("run").cloned().unwrap_or_else(|| json!({}));
    let summary = audit.get("summary").cloned().unwrap_or_else(|| json!({}));
    let domain = crawl_domain(&run);
    let mut source = json!({
        "project": run.get("project"),
                // Resolved in the order start_url, source, project, so the heading names the site.
        "site_name": if domain.is_empty() { Value::Null } else { Value::String(domain) },
        "generated_at": run.get("generated_at"),
        "health_score": summary.get("health_score"),
        "crawl_valid": run.get("crawl_valid").cloned().unwrap_or(Value::Bool(true)),
        "crawl_invalid_reason": run.get("crawl_invalid_reason"),
        // Carried through, not recomputed: a partial or coverage-limited audit
        // must keep saying so in the backlog a developer reads.
        "crawl_partial": run.get("crawl_partial").cloned().unwrap_or(Value::Bool(false)),
        "crawl_finish_reason": run.get("crawl_finish_reason"),
        "health_score_scope": summary.get("health_score_scope"),
        "health_score_basis": summary.get("health_score_basis"),
        "check_coverage": summary.get("check_coverage"),
    });
    if let Some(exclusions) = summary.get("finding_exclusions").filter(|v| !v.is_null()) {
        source
            .as_object_mut()
            .unwrap_or_else(|| unreachable!())
            .insert("finding_exclusions".to_owned(), exclusions.clone());
    }
    let mut output = json!({
        "schema_version": "1.0",
        "source": source,
        "pipeline": cfg,
        "summary": {"tasks_total": tasks.len(), "by_priority": by_priority},
        "tasks": tasks,
    });
    if let Some(grouping) = grouping {
        output
            .as_object_mut()
            .unwrap_or_else(|| unreachable!())
            .insert("grouping".to_owned(), grouping);
    }
    Ok(output)
}

/// `build_tasks_from_audit_v2`: the default backlog from an audit.v2 companion,
/// streamed — only `check` and `check_assignment` grouping are bounded.
pub fn build_tasks_from_audit_v2(
    scan_path: &Path,
    config: Option<&Value>,
) -> Result<Value, TasksError> {
    let cfg = pipeline_cfg(config);
    let group_by = cfg
        .get("group_by")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !matches!(group_by, "check" | "check_assignment") {
        return Err(TasksError(
            "streamed audit.v2 tasks require tasks_pipeline.group_by=check or check_assignment"
                .to_owned(),
        ));
    }
    let declared = if group_by == "check_assignment" {
        declared_assignments(&cfg)?
    } else {
        IndexMap::new()
    };
    if group_by == "check_assignment" && declared.is_empty() {
        return Err(TasksError(
            "streamed audit.v2 check_assignment tasks require declared assignments".to_owned(),
        ));
    }
    let reader = spiderhead_store::audit_v2::AuditV2Reader::new(scan_path, true, false, None)
        .map_err(|e| TasksError(e.to_string()))?;
    if !reader.collections.contains_key("/issues") {
        return Err(TasksError("audit.v2 has no findings collection".to_owned()));
    }
    let empty = Map::new();
    let run = reader
        .header
        .get("run")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_else(|| empty.clone());
    let summary = reader
        .header
        .get("summary")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or(empty);
    let rows = reader
        .iter_collection("/issues")
        .map_err(|e| TasksError(e.to_string()))?;
    let row_refs: Vec<&Value> = rows.iter().collect();
    grouping::stream_grouped_tasks(
        &Value::Object(run),
        &Value::Object(summary),
        row_refs,
        &cfg,
        &declared,
    )
}

/// `write_tasks`: persist `tasks.json` + `tasks.md`, creating parents.
pub fn write_tasks(
    backlog: &Value,
    json_path: &str,
    md_path: &str,
) -> Result<(String, String), TasksError> {
    let json_target = PathBuf::from(json_path);
    create_parent(&json_target)?;
    std::fs::write(&json_target, crate::json_out::dumps(backlog))
        .map_err(|e| TasksError(e.to_string()))?;
    let md_target = PathBuf::from(md_path);
    create_parent(&md_target)?;
    std::fs::write(&md_target, render_tasks_md(backlog)).map_err(|e| TasksError(e.to_string()))?;
    Ok((json_path.to_owned(), md_path.to_owned()))
}

/// Create the parent directory of `path` if it is missing.
fn create_parent(path: &Path) -> Result<(), TasksError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let parent = absolute
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(parent).map_err(|e| TasksError(e.to_string()))
}

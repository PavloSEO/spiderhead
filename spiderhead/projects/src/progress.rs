//! Compact, bounded progress and next-action view for local projects

use std::path::Path;

use serde_json::{json, Map, Value};

use crate::error::{value, ProjectsResult};

/// Default page size.
pub const DEFAULT_LIMIT: i64 = 20;
/// Maximum page size.
pub const MAX_LIMIT: i64 = 100;
/// Maximum offset.
pub const MAX_OFFSET: i64 = 1_000_000;
/// Next-action ceiling.
pub const MAX_NEXT_ACTIONS: usize = 5;

const STATE_NAMES: &[&str] = &[
    "completed",
    "remaining",
    "running",
    "blocked",
    "stale",
    "unavailable",
    "review",
    "deliverable",
    "excluded",
    "not_agreed",
    "not_run",
];

pub(crate) fn item_state(item: &Value) -> &'static str {
    let applicability = item["applicability"].as_str().unwrap_or_default();
    if applicability == "excluded" {
        return "excluded";
    }
    if applicability == "not_agreed" {
        return "not_agreed";
    }
    if item["stale"].as_bool() == Some(true) {
        return "stale";
    }
    if item["attempt_status"].as_str() == Some("running") {
        return "running";
    }
    if item["attempt_status"].as_str() == Some("unavailable") {
        return "unavailable";
    }
    if !item["blocked_by"].as_array().is_none_or(Vec::is_empty)
        || item["attempt_status"].as_str() == Some("failed")
    {
        return "blocked";
    }
    if item["complete"].as_bool() == Some(true) {
        return "completed";
    }
    if applicability == "pending_exclusion" {
        return "review";
    }
    if item["execution_kind"].as_str() == Some("manual") {
        // `item.get("attempt_status", "not_run")`: a present-but-null status is not "not_run".
        let not_run = item
            .get("attempt_status")
            .is_none_or(|v| v.as_str() == Some("not_run"));
        return if not_run { "not_run" } else { "review" };
    }
    if item["execution_kind"].as_str() == Some("deliverable") {
        return "deliverable";
    }
    "remaining"
}

fn has_explicit_plans(
    directory: &Path,
    checklist: &Value,
    coverage_views: Option<&Map<String, Value>>,
) -> (bool, Option<String>) {
    let Some(sites) = checklist.get("sites").and_then(Value::as_array) else {
        return (checklist.get("plan").is_some_and(|p| !p.is_null()), None);
    };
    if sites.is_empty() {
        return (
            false,
            Some("the aggregated checklist contains no site scope".into()),
        );
    }
    let root = directory
        .canonicalize()
        .unwrap_or_else(|_| directory.to_path_buf());
    for site in sites {
        let relative = site.get("directory").and_then(Value::as_str);
        let Some(relative) = relative else {
            return (
                false,
                Some("a site scope has no validated project directory".into()),
            );
        };
        let child = Path::new(relative);
        if child.is_absolute() || child.components().any(|c| c.as_os_str() == "..") {
            return (
                false,
                Some("a site scope points outside the project".into()),
            );
        }
        let current = if relative == "." {
            checklist.clone()
        } else {
            let target = root.join(child);
            if target.is_symlink() {
                return (
                    false,
                    Some("a site scope points through a symbolic link".into()),
                );
            }
            let resolved = target.canonicalize().unwrap_or(target);
            if !resolved.starts_with(&root) {
                return (
                    false,
                    Some("a site scope points outside the project".into()),
                );
            }
            match coverage_views.and_then(|v| v.get(resolved.to_string_lossy().as_ref())) {
                Some(view) => view.clone(),
                None => match crate::coverage::coverage_status(&resolved, true) {
                    Ok(view) => view,
                    Err(_) => {
                        return (
                            false,
                            Some("a declared site scope could not be read".into()),
                        )
                    }
                },
            }
        };
        if current.get("state").and_then(Value::as_str) != Some("initialized")
            || current.get("plan").is_none()
            || current["plan"].is_null()
        {
            return (
                false,
                Some("every site needs an explicitly recorded audit plan".into()),
            );
        }
    }
    (true, None)
}

/// Truthiness of a JSON value: falsy values fall back to the default.
fn falsy(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(b) => !*b,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
    }
}

/// `value or default`: missing or falsy reasons fall back to the default text.
fn or_reason(value: Option<&Value>, default: &str) -> Value {
    match value {
        Some(v) if !falsy(v) => v.clone(),
        _ => json!(default),
    }
}

/// Round to one decimal place, half to even on the exact float value.
fn pyround1(v: f64) -> f64 {
    let scaled = v * 10.0;
    let floor = scaled.floor();
    let diff = scaled - floor;
    let rounded = if diff > 0.5 || (diff == 0.5 && (floor as i64) % 2 != 0) {
        floor + 1.0
    } else {
        floor
    };
    rounded / 10.0
}

fn task_completion(checklist: &Value, explicit_scope: bool, scope_reason: Option<&str>) -> Value {
    let axis = &checklist["coverage"]["audit_tasks"];
    let mut result = json!({
        "label": "Audit-task completion",
        "basis": null,
        "numerator": null,
        "denominator": null,
        "percent": null,
        "state": "unknown",
        "reason": null,
    });
    if !explicit_scope {
        result["reason"] =
            json!(scope_reason
                .unwrap_or("record an explicit audit plan before reporting a percentage"));
        return result;
    }
    if !axis.is_object() {
        result["reason"] = or_reason(
            checklist.get("reason"),
            "audit-task coverage is unavailable",
        );
        return result;
    }
    result["basis"] = axis.get("basis").cloned().unwrap_or(Value::Null);
    if axis.get("state").and_then(Value::as_str) != Some("measured") {
        result["reason"] = or_reason(
            axis.get("reason"),
            "the agreed audit-task denominator is unavailable",
        );
        return result;
    }
    let numerator = axis.get("numerator").and_then(Value::as_i64);
    let denominator = axis.get("denominator").and_then(Value::as_i64);
    let (Some(numerator), Some(denominator)) = (numerator, denominator) else {
        result["reason"] = or_reason(
            axis.get("reason"),
            "the agreed audit-task ratio is undefined",
        );
        return result;
    };
    if denominator <= 0 || !(0..=denominator).contains(&numerator) {
        result["reason"] = or_reason(
            axis.get("reason"),
            "the agreed audit-task ratio is undefined",
        );
        return result;
    }
    result["numerator"] = json!(numerator);
    result["denominator"] = json!(denominator);
    result["percent"] = json!(pyround1(100.0 * numerator as f64 / denominator as f64));
    result["state"] = json!("measured");
    result
}

fn scope_summary(checklist: &Value, explicit_scope: bool, reason: Option<&str>) -> Value {
    let plan = checklist.get("plan").filter(|p| p.is_object());
    let population = plan
        .and_then(|p| p.get("population"))
        .filter(|p| p.is_object());
    let tasks = plan.and_then(|p| p.get("tasks")).filter(|p| p.is_object());
    json!({
        "recorded_for_all_sites": explicit_scope,
        "site_count": checklist.get("sites").and_then(Value::as_array).map_or(1, |s| s.len().max(1)),
        "plan_revision": plan.and_then(|p| p.get("revision")).cloned().unwrap_or(Value::Null),
        "reviewer": plan.and_then(|p| p.get("reviewer")).cloned().unwrap_or(Value::Null),
        "population": population.map(|p| json!({
            "kind": p.get("kind").cloned().unwrap_or(Value::Null),
            "size": p.get("size").cloned().unwrap_or(Value::Null),
            "name": p.get("name").cloned().unwrap_or(Value::Null),
            "source": p.get("source").cloned().unwrap_or(Value::Null),
            "reason": p.get("reason").cloned().unwrap_or(Value::Null),
        })).unwrap_or(Value::Null),
        "task_set": tasks.map(|t| json!({
            "kind": t.get("kind").cloned().unwrap_or(Value::Null),
            "selected_count": t.get("ids").and_then(Value::as_array).map(|ids| json!(ids.len())).unwrap_or(Value::Null),
            "source": t.get("source").cloned().unwrap_or(Value::Null),
        })).unwrap_or(Value::Null),
        "reason": reason,
    })
}

fn next_action(item: &Value, state: &str) -> Option<Value> {
    let action = match state {
        "remaining" => "run this agreed checklist task",
        "stale" => "refresh the stale evidence and rerun the task",
        "unavailable" => "resolve the unavailable input and retry the task",
        "not_run" => "complete the agreed manual check and record its attempt",
        "review" => "review the exclusion or complete the required signoff",
        "deliverable" => "prepare the deliverable and record its approved review",
        "blocked" if item["blocked_by"].as_array().is_none_or(Vec::is_empty) => {
            "inspect the failed attempt and retry when its cause is resolved"
        }
        _ => return None,
    };
    Some(json!({
        "kind": "task",
        "id": item.get("id").cloned().unwrap_or(Value::Null),
        "title": item.get("title").cloned().unwrap_or(Value::Null),
        "state": state,
        "action": action,
        "reason": item.get("reason").cloned().unwrap_or(Value::Null),
        "blocked_by": item.get("blocked_by").cloned().unwrap_or(json!([])),
    }))
}

/// `project_progress`: a bounded progress page using the checklist's own coverage axes.
pub fn project_progress(
    directory: &Path,
    limit: i64,
    offset: i64,
    status: Option<Value>,
    coverage_views: Option<&Map<String, Value>>,
) -> ProjectsResult<Value> {
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(value(format!(
            "limit must be an integer from 1 to {MAX_LIMIT}"
        )));
    }
    if !(0..=MAX_OFFSET).contains(&offset) {
        return Err(value(format!(
            "offset must be an integer from 0 to {MAX_OFFSET}"
        )));
    }
    let status = match status {
        Some(s) => s,
        None => crate::workspace::project_status(directory, None, true)?,
    };
    let checklist = status["checklist"].clone();
    let rows = checklist["items"].as_array().cloned().unwrap_or_default();
    let (explicit_scope, scope_reason) = has_explicit_plans(directory, &checklist, coverage_views);
    let states: Vec<&str> = rows.iter().map(item_state).collect();
    let state_counts: Map<String, Value> = STATE_NAMES
        .iter()
        .map(|name| {
            (
                (*name).to_owned(),
                json!(states.iter().filter(|s| **s == *name).count() as i64),
            )
        })
        .collect();
    let counts = checklist.get("counts").cloned().unwrap_or(json!({}));
    let url_axis = checklist["coverage"]["url_population"].clone();
    let mut next_actions: Vec<Value> = Vec::new();
    if checklist.get("state").and_then(Value::as_str) != Some("initialized") {
        next_actions.push(json!({
            "kind": "scope",
            "id": "scope:initialize",
            "state": "review",
            "action": "initialize the checklist and record the agreed audit plan",
            "reason": or_reason(checklist.get("reason"), "the project checklist is not initialized"),
        }));
    } else if !explicit_scope {
        next_actions.push(json!({
            "kind": "scope",
            "id": "scope:agree",
            "state": "review",
            "action": "record the agreed site population and applicable task set",
            "reason": scope_reason,
        }));
    } else if matches!(
        url_axis.get("state").and_then(Value::as_str),
        Some("unknown" | "partial")
    ) {
        next_actions.push(json!({
            "kind": "scope",
            "id": "scope:url-population",
            "state": "review",
            "action": "establish or verify URL-population coverage",
            "reason": url_axis.get("reason").cloned().unwrap_or(Value::Null),
        }));
    }
    for (item, state_name) in rows.iter().zip(&states) {
        if let Some(action) = next_action(item, state_name) {
            next_actions.push(action);
        }
        if next_actions.len() >= MAX_NEXT_ACTIONS {
            break;
        }
    }
    let total = rows.len() as i64;
    let page: Vec<&Value> = rows
        .iter()
        .skip(offset as usize)
        .take(limit as usize)
        .collect();
    let items: Vec<Value> = page
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let state_name = states[offset as usize + i];
            json!({
                "id": item.get("id").cloned().unwrap_or(Value::Null),
                "title": item.get("title").cloned().unwrap_or(Value::Null),
                "kind": item.get("execution_kind").cloned().unwrap_or(Value::Null),
                "state": state_name,
                "complete": item.get("complete").and_then(Value::as_bool).unwrap_or(false),
                "stale": item.get("stale").and_then(Value::as_bool).unwrap_or(false),
                "attempt_status": item.get("attempt_status").cloned().unwrap_or(Value::Null),
                "applicability": item.get("applicability").cloned().unwrap_or(Value::Null),
                "blocked_by": item.get("blocked_by").cloned().unwrap_or(json!([])),
                "reason": item.get("reason").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    let next_offset = if offset + (page.len() as i64) < total {
        json!(offset + page.len() as i64)
    } else {
        Value::Null
    };
    let site = &status["project"]["site"];
    Ok(json!({
        "state": checklist.get("state").cloned().unwrap_or(json!("unknown")),
        "project": {
            "site": site.get("host").cloned().unwrap_or(Value::Null),
            "label": site.get("label").cloned().unwrap_or(Value::Null),
        },
        "revision": checklist.get("revision").cloned().unwrap_or(Value::Null),
        "scope": scope_summary(&checklist, explicit_scope, scope_reason.as_deref()),
        "audit_task_completion": task_completion(&checklist, explicit_scope, scope_reason.as_deref()),
        "url_population": if url_axis.as_object().is_some_and(|o| !o.is_empty()) {
            json!({
                "basis": url_axis.get("basis").cloned().unwrap_or(Value::Null),
                "numerator": url_axis.get("numerator").cloned().unwrap_or(Value::Null),
                "denominator": url_axis.get("denominator").cloned().unwrap_or(Value::Null),
                "state": url_axis.get("state").cloned().unwrap_or(Value::Null),
                "reason": url_axis.get("reason").cloned().unwrap_or(Value::Null),
                "population_kind": url_axis.get("population_kind").cloned().unwrap_or(Value::Null),
                "population_name": url_axis.get("population_name").cloned().unwrap_or(Value::Null),
            })
        } else {
            Value::Null
        },
        "audit_complete": checklist.get("complete").and_then(Value::as_bool).unwrap_or(false),
        "counts": {
            "total": counts.get("total").cloned().unwrap_or(Value::Null),
            "complete": counts.get("complete").cloned().unwrap_or(Value::Null),
            "remaining": counts.get("remaining").cloned().unwrap_or(Value::Null),
            "stale": counts.get("stale").cloned().unwrap_or(Value::Null),
            "excluded": counts.get("excluded").cloned().unwrap_or(Value::Null),
            "pending_exclusion": counts.get("pending_exclusion").cloned().unwrap_or(Value::Null),
            "not_agreed": counts.get("not_agreed").cloned().unwrap_or(Value::Null),
        },
        "state_counts": Value::Object(state_counts),
        "next_actions": next_actions[..next_actions.len().min(MAX_NEXT_ACTIONS)].to_vec(),
        "items": items,
        "pagination": {
            "limit": limit,
            "offset": offset,
            "total": total,
            "next_offset": next_offset,
        },
    }))
}

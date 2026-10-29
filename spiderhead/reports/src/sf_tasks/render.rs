//! `tasks.md` — the human-readable rendering of a task backlog.

use indexmap::IndexMap;
use serde_json::Value;

use super::PRIORITY_ORDER;
use crate::py;

/// `esc`: markdown-table-safe scalar text — `None` prints as "".
fn esc(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(v) => str_of(v)
            .replace('\\', "\\\\")
            .replace('|', "\\|")
            .replace('\n', " ")
            .trim()
            .to_owned(),
    }
}

/// Text for interpolation: `None` or absent renders as `"None"`.
fn py_str(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "None".to_owned(),
        Some(v) => str_of(v),
    }
}

fn str_of(value: &Value) -> String {
    match value {
        Value::Object(_) | Value::Array(_) => py::repr(value),
        other => py::str_value(other),
    }
}

/// Integer parsed for a display count.
fn py_int(value: Option<&Value>) -> i64 {
    value.and_then(py::to_i64).unwrap_or_default()
}

/// `render_tasks_md`: the developer-facing Markdown backlog.
pub fn render_tasks_md(backlog: &Value) -> String {
    let empty = serde_json::Map::new();
    let src = backlog
        .get("source")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let get = |key: &str| src.get(key);
    // Project name, then site name, then "site"; a stored null counts as missing.
    let site_name = [get("project"), get("site_name")]
        .into_iter()
        .flatten()
        .find(|v| py::truthy(Some(*v)))
        .map(py::str_value)
        .unwrap_or_else(|| "site".to_owned());
    let mut lines = vec![format!("# Audit Tasks — {site_name}"), String::new()];
    // A failed crawl says so before anything else: the tasks below describe
    // the failed run, not the site.
    if get("crawl_valid") == Some(&Value::Bool(false)) {
        let reason = get("crawl_invalid_reason")
            .filter(|v| py::truthy(Some(*v)))
            .map(str_of)
            .unwrap_or_else(|| "the crawl produced no usable data".to_owned());
        lines.push(format!("> **Crawl failed — no health score.** {reason}."));
        lines.push(String::new());
    }
    // A partial crawl still scores — a distinct warning from the failed one
    // A partial crawl still scores; this warning is distinct from the failed-crawl one above.
    if py::truthy(get("crawl_partial")) {
        let stop = get("crawl_finish_reason");
        let stop_text = match stop {
            Some(s) if py::truthy(Some(s)) => format!(" Stopped early: `{}`.", str_of(s)),
            _ => String::new(),
        };
        lines.push(format!(
            "> **Partial crawl — this is a sample, not the whole site.**{stop_text}"
        ));
        if let Some(scope) = get("health_score_scope").filter(|v| py::truthy(Some(*v))) {
            lines.push(format!("> {}", str_of(scope)));
        }
        lines.push(String::new());
    }
    // The health-score basis note appears whenever checks were skipped or disabled,
    // independent of a partial crawl.
    if let Some(basis) = get("health_score_basis").filter(|v| py::truthy(Some(*v))) {
        lines.push(format!("> {}", str_of(basis)));
        lines.push(String::new());
    }
    let exclusions = src
        .get("finding_exclusions")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    if py::truthy(exclusions.get("rules_configured")) {
        let findings = py_int(exclusions.get("suppressed_total"));
        let rules = py_int(exclusions.get("rules_configured"));
        let finding_label = if findings == 1 { "finding" } else { "findings" };
        let rule_label = if rules == 1 { "rule" } else { "rules" };
        lines.push(format!(
            "> Explicit URL-pattern exclusions: {findings} {finding_label} suppressed by {rules} {rule_label}. Suppressed findings are absent from this task list; see the audit JSON for their full records and reasons."
        ));
        lines.push(String::new());
    }
    let health_text = match get("health_score") {
        None | Some(Value::Null) => "n/a".to_owned(),
        Some(h) => str_of(h),
    };
    lines.push(format!(
        "- Source: audit generated at {} (health {health_text})",
        py_str(get("generated_at"))
    ));
    let summary = backlog
        .get("summary")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let by_priority = summary
        .get("by_priority")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let mut priority_keys: Vec<&String> = by_priority.keys().collect();
    priority_keys.sort();
    let counts = priority_keys
        .iter()
        .map(|k| format!("{k}: {}", py_str(by_priority.get(*k))))
        .collect::<Vec<_>>()
        .join(", ");
    lines.push(format!(
        "- Tasks: **{}** ({counts})",
        py_str(summary.get("tasks_total"))
    ));
    lines.push(String::new());

    let tasks = backlog
        .get("tasks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut by_prio: IndexMap<String, Vec<&Value>> = IndexMap::new();
    for task in &tasks {
        let prio = task
            .get("priority")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        by_prio.entry(prio).or_default().push(task);
    }
    let mut prio_order: Vec<(&String, &Vec<&Value>)> = by_prio.iter().collect();
    prio_order.sort_by_key(|(p, _)| {
        PRIORITY_ORDER
            .iter()
            .find(|(k, _)| *k == p.as_str())
            .map(|(_, v)| *v)
            .unwrap_or(9)
    });
    for (prio, group) in prio_order {
        lines.push(format!("## {prio} ({})", group.len()));
        lines.push(String::new());
        for task in group {
            lines.push(format!(
                "- [ ] **{}** · {} · effort: {}",
                esc(task.get("title")),
                py_str(task.get("severity")),
                py_str(task.get("effort"))
            ));
            if let Some(assignment) = task
                .get("assignment")
                .filter(|v| py::truthy(Some(*v)))
                .and_then(Value::as_object)
            {
                lines.push(format!(
                    "    - Assignment: {} ({}, {}) — {}",
                    esc(assignment.get("name")),
                    esc(assignment.get("kind")),
                    esc(assignment.get("state")),
                    esc(assignment.get("rationale"))
                ));
                let scope = task
                    .get("verification_scope")
                    .and_then(Value::as_object)
                    .unwrap_or(&empty);
                lines.push(format!(
                    "    - Verification scope: inspect a representative, then verify all {} affected URL(s); assignment does not prove one shared fix.",
                    py_str(scope.get("affected_urls_total"))
                ));
            }
            if let Some(hint) = task.get("fix_hint").filter(|v| py::truthy(Some(*v))) {
                lines.push(format!("    - _How to fix:_ {}", esc(Some(hint))));
            }
            let reproductions = task
                .get("reproductions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if !reproductions.is_empty() {
                for item in reproductions.iter().take(15) {
                    lines.push(format!("    - Reproduction: {}", esc(Some(item))));
                }
            } else {
                lines.push("    - Reproduction unavailable from the saved audit.".to_owned());
            }
            let broken_links = task
                .get("broken_links")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let urls = task
                .get("urls")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if !broken_links.is_empty() {
                lines.push(
                    "    - Broken links (destination ← source · position · XPath):".to_owned(),
                );
                let shown: Vec<&Value> = broken_links.iter().take(15).collect();
                for bl in &shown {
                    lines.push(format!(
                        "        - {} ({}) ← {} · {} · `{}`",
                        esc(bl.get("target_url")),
                        py_str(bl.get("status_code")),
                        esc(bl.get("source_url")),
                        esc(bl.get("link_position")),
                        esc(bl.get("link_path"))
                    ));
                }
                // Locations cut by the display slice and by the location cap are both real omissions.
                // Both are reported as omissions.
                let hidden = py_int(task.get("broken_links_truncated"))
                    + (broken_links.len() - shown.len()) as i64;
                if hidden > 0 {
                    lines.push(format!(
                        "        - … {hidden} more source location(s) omitted"
                    ));
                }
            } else if !urls.is_empty() {
                for url in urls.iter().take(15) {
                    lines.push(format!("        - {}", esc(Some(url))));
                }
                let shown = urls.len().min(15);
                let hidden = py_int(task.get("urls_truncated")) + (urls.len() - shown) as i64;
                if hidden > 0 {
                    lines.push(format!("        - … {hidden} more"));
                }
            }
            lines.push(String::new());
        }
    }
    format!("{}\n", lines.join("\n"))
}

//! Task grouping internals: one task per check, per issue, or per
//! check+assignment — plus the bounded SQLite aggregation used when the
//! findings arrive as an audit.v2 stream.

use std::collections::HashSet;
use std::path::Path;

use indexmap::IndexMap;
use serde_json::{json, Map, Value};

use super::{
    check_meta_fallback, evidence_references, reproductions, task_id, unassigned_assignment,
    TasksError, LINK_CHECKS, PRIORITY_ORDER,
};
use crate::py;

/// The `(priority, -affected_count)` sort key shared by both builders.
fn sort_key(task: &Value) -> (i64, i64) {
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
}

/// `_group_by_check`: one task per check id, first-seen order.
pub(super) fn group_by_check(
    issues: &[&Value],
    prio: &Value,
    effort: &Value,
    cap: usize,
    loc_cap: usize,
    min_occ: i64,
) -> Vec<Value> {
    let mut groups: IndexMap<String, Vec<&Value>> = IndexMap::new();
    for issue in issues {
        let check = issue
            .get("check")
            .and_then(Value::as_str)
            .unwrap_or_default();
        groups.entry(check.to_owned()).or_default().push(issue);
    }
    let mut tasks = Vec::new();
    for (check, group) in &groups {
        if let Some(task) = group_task(
            check, group, prio, effort, cap, loc_cap, min_occ, "all", None,
        ) {
            tasks.push(task);
        }
    }
    tasks
}

/// `_group_by_check_assignment`: one task per (check, assignment) pair.
pub(super) fn group_by_check_assignment(
    issues: &[&Value],
    assignments: &IndexMap<String, Value>,
    prio: &Value,
    effort: &Value,
    cap: usize,
    loc_cap: usize,
    min_occ: i64,
) -> Vec<Value> {
    let mut groups: IndexMap<(String, String), (Value, Vec<&Value>)> = IndexMap::new();
    for issue in issues {
        let assignment = issue
            .get("target_url")
            .and_then(Value::as_str)
            .and_then(|url| assignments.get(url))
            .cloned()
            .unwrap_or_else(|| {
                unassigned_assignment(
                    "No declared template/component assignment or validated segment matched this finding.",
                )
            });
        let check = issue
            .get("check")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let key = (
            check.to_owned(),
            assignment
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        );
        groups
            .entry(key)
            .or_insert_with(|| (assignment.clone(), Vec::new()))
            .1
            .push(issue);
    }
    let mut tasks = Vec::new();
    for ((check, assignment_id), (assignment, group)) in &groups {
        if let Some(task) = group_task(
            check,
            group,
            prio,
            effort,
            cap,
            loc_cap,
            min_occ,
            assignment_id,
            Some(assignment),
        ) {
            tasks.push(task);
        }
    }
    tasks
}

/// `_group_task`: build one grouped task; `None` when under `min_occurrences`.
#[allow(clippy::too_many_arguments)]
fn group_task(
    check: &str,
    group: &[&Value],
    prio: &Value,
    effort: &Value,
    cap: usize,
    loc_cap: usize,
    min_occ: i64,
    key: &str,
    assignment: Option<&Value>,
) -> Option<Value> {
    let urls: Vec<Value> = group
        .iter()
        .filter_map(|i| i.get("target_url"))
        .filter(|u| py::truthy(Some(*u)))
        .cloned()
        .collect();
    let mut unique_urls: Vec<Value> = Vec::new();
    for url in urls {
        if !unique_urls.contains(&url) {
            unique_urls.push(url);
        }
    }
    let occurrences: i64 = group
        .iter()
        .map(|i| i.get("occurrences_count").and_then(py::to_i64).unwrap_or(1))
        .sum();
    if occurrences < min_occ {
        return None;
    }
    let meta = check_meta_fallback(check);
    let severity = group
        .first()
        .and_then(|i| i.get("severity"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let label = if unique_urls.len() == 1 {
        "page"
    } else {
        "pages"
    };
    let title = if unique_urls.is_empty() {
        meta.message.clone()
    } else {
        format!("{} — {} {}", meta.message, unique_urls.len(), label)
    };
    let title = match assignment {
        Some(a) => format!(
            "{} — {} — {} {}",
            meta.message,
            a.get("name").and_then(Value::as_str).unwrap_or_default(),
            unique_urls.len(),
            label
        ),
        None => title,
    };
    let kept: Vec<Value> = unique_urls.iter().take(cap).cloned().collect();
    let urls_truncated = unique_urls.len().saturating_sub(cap);
    let (evidence, evidence_truncated) = evidence_references(group, cap);
    let affected = if unique_urls.is_empty() {
        group.len() as i64
    } else {
        unique_urls.len() as i64
    };
    let mut task = json!({
        "id": task_id(check, key),
        "check": check,
        "priority": prio.get(severity).cloned().unwrap_or_else(|| json!("P3")),
        "severity": severity,
        "effort": effort.get(severity).cloned().unwrap_or_else(|| json!("medium")),
        "title": title,
        "fix_hint": meta.fix.clone().map(Value::String).unwrap_or(Value::Null),
        "source": group.first().and_then(|i| i.get("source")).cloned().unwrap_or(Value::Null),
        "affected_count": affected,
        "occurrences": occurrences,
        "urls": kept,
        "urls_truncated": urls_truncated,
        "reproductions": reproductions(group, cap),
        "evidence_references": evidence,
        "evidence_references_truncated": evidence_truncated,
    });
    if let Some(a) = assignment {
        let obj = task.as_object_mut().unwrap_or_else(|| unreachable!());
        obj.insert("assignment".to_owned(), a.clone());
        obj.insert(
            "membership".to_owned(),
            json!({
                "findings_total": group.len(),
                "affected_urls_total": unique_urls.len(),
                "urls_returned": kept.len(),
                "urls_truncated": urls_truncated,
                "retrieval": "Filter the source audit by this task's check and assignment provenance; URL caps do not change the source findings.",
            }),
        );
        obj.insert(
            "verification_scope".to_owned(),
            json!({
                "state": a.get("state"),
                "affected_urls_total": unique_urls.len(),
                "representative_urls": kept,
                "representative_urls_truncated": urls_truncated,
                "guidance": "Inspect a representative page, then verify every affected URL after a change; assignment alone does not prove a shared cause.",
            }),
        );
    }
    if LINK_CHECKS.contains(&check) {
        let (links, total, truncated) = link_evidence(group, loc_cap);
        let obj = task.as_object_mut().unwrap_or_else(|| unreachable!());
        obj.insert("broken_links".to_owned(), json!(links));
        obj.insert("broken_links_total".to_owned(), json!(total));
        obj.insert("broken_links_truncated".to_owned(), json!(truncated));
    }
    Some(task)
}

/// `_per_issue`: one task per finding record.
pub(super) fn per_issue(
    issues: &[&Value],
    prio: &Value,
    effort: &Value,
    cap: usize,
    loc_cap: usize,
    min_occ: i64,
) -> Vec<Value> {
    let mut tasks = Vec::new();
    for issue in issues {
        // Same threshold semantics as _group_by_check (#224).
        let occurrences = issue
            .get("occurrences_count")
            .and_then(py::to_i64)
            .unwrap_or(1);
        if occurrences < min_occ {
            continue;
        }
        let check = issue
            .get("check")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let severity = issue
            .get("severity")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let meta = check_meta_fallback(check);
        let id = {
            let raw = issue
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .replace("ISSUE-", "");
            if raw.is_empty() {
                let target = match issue.get("target_url") {
                    None | Some(Value::Null) => "None".to_owned(),
                    Some(other) => py::str_value(other),
                };
                task_id(check, &target)
            } else {
                format!("TASK-{raw}")
            }
        };
        let title = format!(
            "{}: {}",
            meta.message,
            issue
                .get("target_url")
                .filter(|u| py::truthy(Some(*u)))
                .map(py::str_value)
                .unwrap_or_default()
        )
        .trim()
        .to_owned();
        let fix_hint = match issue.get("fix_hint").filter(|h| py::truthy(Some(*h))) {
            Some(hint) => hint.clone(),
            None => meta.fix.clone().map(Value::String).unwrap_or(Value::Null),
        };
        let urls: Vec<Value> = issue
            .get("target_url")
            .filter(|u| py::truthy(Some(*u)))
            .map(|u| vec![u.clone()])
            .unwrap_or_default();
        let (evidence, evidence_truncated) = evidence_references(&[*issue], cap);
        let mut task = json!({
            "id": id,
            "check": check,
            "priority": prio.get(severity).cloned().unwrap_or_else(|| json!("P3")),
            "severity": severity,
            "effort": effort.get(severity).cloned().unwrap_or_else(|| json!("medium")),
            "title": title,
            "fix_hint": fix_hint,
            "source": issue.get("source"),
            "affected_count": 1,
            "occurrences": occurrences,
            "urls": urls,
            "urls_truncated": 0,
            "reproductions": reproductions(&[*issue], cap),
            "evidence_references": evidence,
            "evidence_references_truncated": evidence_truncated,
        });
        if LINK_CHECKS.contains(&check) {
            let (links, total, truncated) = link_evidence(&[*issue], loc_cap);
            let obj = task.as_object_mut().unwrap_or_else(|| unreachable!());
            obj.insert("broken_links".to_owned(), json!(links));
            obj.insert("broken_links_total".to_owned(), json!(total));
            obj.insert("broken_links_truncated".to_owned(), json!(truncated));
        }
        tasks.push(task);
    }
    tasks
}

/// `_link_evidence`: (kept locations, total, omitted) under the location cap.
fn link_evidence(group: &[&Value], loc_cap: usize) -> (Vec<Value>, usize, usize) {
    let total: usize = group
        .iter()
        .map(|i| {
            i.get("locations")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or_default()
        })
        .sum();
    let mut out = Vec::new();
    'outer: for issue in group {
        if let Some(locations) = issue.get("locations").and_then(Value::as_array) {
            for loc in locations {
                if out.len() >= loc_cap {
                    break;
                }
                out.push(json!({
                    "target_url": issue.get("target_url"),
                    "status_code": issue.get("status_code"),
                    "source_url": loc.get("source_url"),
                    "anchor": loc.get("anchor"),
                    "link_position": loc.get("link_position"),
                    "link_path": loc.get("link_path"),
                }));
            }
        }
        if out.len() >= loc_cap {
            break 'outer;
        }
    }
    let truncated = total.saturating_sub(out.len());
    (out, total, truncated)
}

/// A grouped accumulator of the streamed aggregation.
struct StreamGroup<'a> {
    check: String,
    assignment: Option<Value>,
    first: &'a Value,
    issues: i64,
    occurrences: i64,
    reproductions: Vec<String>,
    reproduction_seen: HashSet<String>,
    links: Vec<Value>,
    links_total: usize,
}

/// `_stream_grouped_tasks`: the SQLite-backed aggregation — distinct URLs and
/// evidence are deduplicated in tables, only capped samples stay in RAM.
pub(super) fn stream_grouped_tasks(
    run: &Value,
    summary: &Value,
    rows: Vec<&Value>,
    cfg: &Value,
    declared: &IndexMap<String, Value>,
) -> Result<Value, TasksError> {
    let name = std::env::temp_dir().join(format!(
        "seohead-task-rows-{}.sqlite",
        uuid::Uuid::new_v4().simple()
    ));
    let result = stream_grouped_tasks_inner(run, summary, rows, cfg, declared, &name);
    let _ = std::fs::remove_file(&name);
    result
}

fn stream_grouped_tasks_inner(
    run: &Value,
    summary: &Value,
    rows: Vec<&Value>,
    cfg: &Value,
    declared: &IndexMap<String, Value>,
    db_path: &Path,
) -> Result<Value, TasksError> {
    let con = rusqlite::Connection::open(db_path).map_err(|e| TasksError(e.to_string()))?;
    con.execute(
        "CREATE TABLE urls (check_id TEXT, url TEXT, ordinal INTEGER, \
         PRIMARY KEY(check_id,url)) WITHOUT ROWID",
        [],
    )
    .map_err(|e| TasksError(e.to_string()))?;
    con.execute(
        "CREATE TABLE evidence (check_id TEXT, key TEXT, ordinal INTEGER, value_json TEXT, \
         PRIMARY KEY(check_id,key)) WITHOUT ROWID",
        [],
    )
    .map_err(|e| TasksError(e.to_string()))?;
    let severities = cfg
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
    let loc_cap = cfg
        .get("max_locations_per_task")
        .and_then(py::to_i64)
        .unwrap_or(25)
        .max(0) as usize;
    let group_by = cfg
        .get("group_by")
        .and_then(Value::as_str)
        .unwrap_or_default();

    let mut groups: IndexMap<String, StreamGroup> = IndexMap::new();
    let mut unassigned_findings: i64 = 0;
    for (ordinal, issue) in rows.into_iter().enumerate() {
        let Some(map) = issue.as_object() else {
            continue;
        };
        let severity = map
            .get("severity")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !severities.iter().any(|s| s.as_str() == Some(severity)) {
            continue;
        }
        let Some(check) = map.get("check").and_then(Value::as_str) else {
            continue;
        };
        if (!include.is_empty() && !include.iter().any(|c| c.as_str() == Some(check)))
            || exclude.iter().any(|c| c.as_str() == Some(check))
        {
            continue;
        }
        let url = map.get("target_url");
        let mut assignment: Option<Value> = None;
        let mut group_key = check.to_owned();
        if group_by == "check_assignment" {
            let a = url
                .and_then(Value::as_str)
                .and_then(|u| declared.get(u))
                .cloned()
                .unwrap_or_else(|| {
                    unassigned_assignment(
                        "No declared template/component assignment matched this finding.",
                    )
                });
            group_key = format!(
                "{check}\x00{}",
                a.get("id").and_then(Value::as_str).unwrap_or_default()
            );
            if a.get("id").and_then(Value::as_str) == Some("unassigned") {
                unassigned_findings += 1;
            }
            assignment = Some(a);
        }
        let group = groups
            .entry(group_key.clone())
            .or_insert_with(|| StreamGroup {
                check: check.to_owned(),
                assignment: assignment.clone(),
                first: issue,
                issues: 0,
                occurrences: 0,
                reproductions: Vec::new(),
                reproduction_seen: HashSet::new(),
                links: Vec::new(),
                links_total: 0,
            });
        group.issues += 1;
        group.occurrences += map
            .get("occurrences_count")
            .and_then(py::to_i64)
            .unwrap_or(1);
        if let Some(url_text) = url.and_then(Value::as_str).filter(|u| !u.is_empty()) {
            con.execute(
                "INSERT OR IGNORE INTO urls VALUES (?,?,?)",
                rusqlite::params![group_key, url_text, ordinal as i64],
            )
            .map_err(|e| TasksError(e.to_string()))?;
        }
        for value in reproductions(&[issue], 1) {
            if !group.reproduction_seen.contains(&value) && group.reproductions.len() < cap {
                group.reproduction_seen.insert(value.clone());
                group.reproductions.push(value);
            }
        }
        let (evidence, _unused) = evidence_references(&[issue], 1);
        for value in evidence {
            let key = spiderhead_store::discovery_ledger::canonical_json(&value);
            let stored = serde_json::to_string(&value).unwrap_or_default();
            con.execute(
                "INSERT OR IGNORE INTO evidence VALUES (?,?,?,?)",
                rusqlite::params![group_key, key, ordinal as i64, stored],
            )
            .map_err(|e| TasksError(e.to_string()))?;
        }
        let locations = map
            .get("locations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if LINK_CHECKS.contains(&check) {
            group.links_total += locations.len();
            if group.links.len() < loc_cap {
                for location in &locations {
                    if group.links.len() >= loc_cap {
                        break;
                    }
                    group.links.push(json!({
                        "target_url": url,
                        "status_code": map.get("status_code"),
                        "source_url": location.get("source_url"),
                        "anchor": location.get("anchor"),
                        "link_position": location.get("link_position"),
                        "link_path": location.get("link_path"),
                    }));
                }
            }
        }
    }
    let min_occ = cfg
        .get("min_occurrences")
        .and_then(py::to_i64)
        .unwrap_or_default();
    let prio_map = cfg
        .get("priority_map")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let effort_map = cfg.get("effort_map").cloned().unwrap_or_else(|| json!({}));
    let mut tasks = Vec::new();
    for (group_key, group) in &groups {
        if group.occurrences < min_occ {
            continue;
        }
        let check = group.check.as_str();
        let urls: Vec<String> = con
            .prepare("SELECT url FROM urls WHERE check_id=? ORDER BY ordinal LIMIT ?")
            .and_then(|mut stmt| {
                stmt.query_map(rusqlite::params![group_key, cap as i64], |row| {
                    row.get::<_, String>(0)
                })
                .map(|rows| rows.flatten().collect())
            })
            .map_err(|e| TasksError(e.to_string()))?;
        let url_total: i64 = con
            .query_row(
                "SELECT COUNT(*) FROM urls WHERE check_id=?",
                rusqlite::params![group_key],
                |row| row.get(0),
            )
            .map_err(|e| TasksError(e.to_string()))?;
        let evidence: Vec<Value> = con
            .prepare("SELECT value_json FROM evidence WHERE check_id=? ORDER BY ordinal LIMIT ?")
            .and_then(|mut stmt| {
                stmt.query_map(rusqlite::params![group_key, cap as i64], |row| {
                    row.get::<_, String>(0)
                })
                .map(|rows| {
                    rows.flatten()
                        .filter_map(|text| serde_json::from_str(&text).ok())
                        .collect()
                })
            })
            .map_err(|e| TasksError(e.to_string()))?;
        let evidence_total: i64 = con
            .query_row(
                "SELECT COUNT(*) FROM evidence WHERE check_id=?",
                rusqlite::params![group_key],
                |row| row.get(0),
            )
            .map_err(|e| TasksError(e.to_string()))?;
        let first = group.first;
        let meta = check_meta_fallback(check);
        let severity = first
            .get("severity")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let label = if url_total == 1 { "page" } else { "pages" };
        let title = if url_total > 0 {
            format!("{} — {} {}", meta.message, url_total, label)
        } else {
            meta.message.clone()
        };
        let (title, task_key) = match &group.assignment {
            Some(a) => (
                format!(
                    "{} — {} — {} {}",
                    meta.message,
                    a.get("name").and_then(Value::as_str).unwrap_or_default(),
                    url_total,
                    label
                ),
                a.get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            ),
            None => (title, "all".to_owned()),
        };
        let kept_urls: Vec<Value> = urls.into_iter().map(Value::String).collect();
        let urls_truncated = (url_total as usize).saturating_sub(kept_urls.len());
        let affected = if url_total > 0 {
            url_total
        } else {
            group.issues
        };
        let mut task = json!({
            "id": task_id(check, &task_key),
            "check": check,
            "priority": prio_map.get(severity).cloned().unwrap_or_else(|| json!("P3")),
            "severity": severity,
            "effort": effort_map.get(severity).cloned().unwrap_or_else(|| json!("medium")),
            "title": title,
            "fix_hint": meta.fix.clone().map(Value::String).unwrap_or(Value::Null),
            "source": first.get("source"),
            "affected_count": affected,
            "occurrences": group.occurrences,
            "urls": kept_urls,
            "urls_truncated": urls_truncated,
            "reproductions": group.reproductions,
            "evidence_references": evidence,
            "evidence_references_truncated": (evidence_total as usize).saturating_sub(evidence.len()),
        });
        if let Some(a) = &group.assignment {
            let obj = task.as_object_mut().unwrap_or_else(|| unreachable!());
            obj.insert("assignment".to_owned(), a.clone());
            obj.insert(
                "membership".to_owned(),
                json!({
                    "findings_total": group.issues,
                    "affected_urls_total": url_total,
                    "urls_returned": kept_urls.len(),
                    "urls_truncated": urls_truncated,
                    "retrieval": "Filter the source audit by this task's check and assignment provenance; URL caps do not change the source findings.",
                }),
            );
            obj.insert(
                "verification_scope".to_owned(),
                json!({
                    "state": a.get("state"),
                    "affected_urls_total": url_total,
                    "representative_urls": kept_urls,
                    "representative_urls_truncated": urls_truncated,
                    "guidance": "Inspect a representative page, then verify every affected URL after a change; assignment alone does not prove a shared cause.",
                }),
            );
        }
        if LINK_CHECKS.contains(&check) {
            let obj = task.as_object_mut().unwrap_or_else(|| unreachable!());
            obj.insert("broken_links".to_owned(), json!(group.links));
            obj.insert("broken_links_total".to_owned(), json!(group.links_total));
            obj.insert(
                "broken_links_truncated".to_owned(),
                json!(group.links_total.saturating_sub(group.links.len())),
            );
        }
        tasks.push(task);
    }
    drop(con);
    // Rebuild the envelope through the in-memory builder so the source block
    // and defaults stay in one place.
    let base_audit = json!({"run": run, "summary": summary, "issues": []});
    let wrapped = json!({"tasks_pipeline": cfg});
    let mut base = super::build_tasks(&base_audit, Some(&wrapped))?;
    tasks.sort_by_key(sort_key);
    let mut priorities: Vec<String> = tasks
        .iter()
        .filter_map(|t| t.get("priority").and_then(Value::as_str).map(str::to_owned))
        .collect();
    priorities.sort();
    priorities.dedup();
    let mut by_priority: Map<String, Value> = Map::new();
    for prio in &priorities {
        let count = tasks
            .iter()
            .filter(|t| t.get("priority").and_then(Value::as_str) == Some(prio.as_str()))
            .count();
        by_priority.insert(prio.clone(), Value::from(count as i64));
    }
    let obj = base.as_object_mut().unwrap_or_else(|| unreachable!());
    let tasks_total = tasks.len() as i64;
    obj.insert("tasks".to_owned(), Value::Array(tasks));
    obj.insert(
        "summary".to_owned(),
        json!({"tasks_total": tasks_total, "by_priority": by_priority}),
    );
    if group_by == "check_assignment" {
        let assignment_count = declared
            .values()
            .filter_map(|a| a.get("id").and_then(Value::as_str))
            .collect::<HashSet<_>>()
            .len();
        obj.insert(
            "grouping".to_owned(),
            json!({
                "mode": "operator_assignments",
                "assignment_count": assignment_count,
                "unassigned_findings": unassigned_findings,
            }),
        );
    }
    Ok(base)
}

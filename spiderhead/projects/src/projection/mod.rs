//! Bounded projections for finding pages, checklists and scan rows

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use rusqlite::types::ValueRef;
use serde_json::{Map, Value};

use crate::error::{value, ProjectsError, ProjectsResult};
use crate::pyjson;
use crate::schedule::falsy;
use crate::workspace;

mod findingspage;
pub use findingspage::*;

/// Default findings page size.
pub const FINDING_PAGE_SIZE: i64 = 50;
/// Maximum findings page size.
pub const MAX_FINDING_PAGE_SIZE: i64 = 100;
/// Fields a findings page may be sorted by.
pub const FINDING_SORTS: [&str; 4] = ["severity", "check", "target_url", "id"];
/// Fields a checklist page may be sorted by.
pub const CHECKLIST_SORTS: [&str; 4] = ["id", "priority", "state", "updated"];
/// Allowed checklist item states.
pub const CHECKLIST_STATES: [&str; 10] = [
    "excluded",
    "not_agreed",
    "stale",
    "running",
    "unavailable",
    "blocked",
    "completed",
    "review",
    "deliverable",
    "remaining",
];

pub(crate) fn severity_rank(name: &str) -> i64 {
    match name {
        "critical" => 0,
        "error" => 1,
        "warning" => 2,
        "notice" => 3,
        "info" => 4,
        _ => 99,
    }
}

/// Expected and recorded scenario/skill work, without inventing a percentage.
pub fn method_coverage(checklist: &Value) -> Value {
    let state = checklist.get("state");
    if state.and_then(Value::as_str) != Some("initialized") {
        let reason = checklist
            .get("reason")
            .cloned()
            .filter(|r| !falsy(r))
            .unwrap_or_else(|| Value::String("method coverage is unavailable".into()));
        let state_value = state
            .cloned()
            .filter(|s| !falsy(s))
            .unwrap_or_else(|| Value::String("unavailable".into()));
        return serde_json::json!({
            "state": state_value,
            "reason": reason,
            "kinds": {},
        });
    }
    let items = checklist
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let has_plan = checklist.get("plan").is_some_and(|p| !p.is_null());
    let mut kinds = Map::new();
    for kind in ["scenario", "skill"] {
        let rows: Vec<&Value> = items
            .iter()
            .filter(|item| {
                item.get("kind").and_then(Value::as_str) == Some(kind)
                    && !falsy(&item.get("enabled").cloned().unwrap_or(Value::Bool(true)))
                    && item.get("applicability").and_then(Value::as_str) == Some("applicable")
            })
            .collect();
        let mut states: BTreeMap<String, i64> = BTreeMap::new();
        for row in &rows {
            if let Some(name) = row.get("state").and_then(Value::as_str) {
                *states.entry(name.to_string()).or_insert(0) += 1;
            }
        }
        let completed = rows
            .iter()
            .filter(|row| row.get("complete") == Some(&Value::Bool(true)))
            .count() as i64;
        kinds.insert(
            kind.to_string(),
            serde_json::json!({
                "expected": if has_plan { Value::from(rows.len() as i64) } else { Value::Null },
                "applicable": rows.len() as i64,
                "basis": if has_plan { "agreed applicable methods" } else { "audit plan not recorded" },
                "completed": completed,
                "states": states,
            }),
        );
    }
    serde_json::json!({"state": "available", "reason": Value::Null, "kinds": kinds})
}

/// Bounded coverage projection preserving unknown and partial states
pub fn coverage_summary(checklist: &Value) -> Value {
    const KEYS: [&str; 9] = [
        "state",
        "reason",
        "numerator",
        "denominator",
        "measured_urls",
        "unfinished",
        "unverified_measurements",
        "population_kind",
        "population_name",
    ];
    let mut axes = Map::new();
    if let Some(coverage) = checklist.get("coverage").filter(|c| !falsy(c)) {
        if let Some(object) = coverage.as_object() {
            for (name, axis) in object {
                let Some(axis) = axis.as_object() else {
                    continue;
                };
                let mut projection = Map::new();
                for key in KEYS {
                    if let Some(item) = axis.get(key) {
                        projection.insert(key.to_string(), item.clone());
                    }
                }
                axes.insert(name.clone(), Value::Object(projection));
            }
        }
    }
    serde_json::json!({
        "state": checklist.get("state").cloned().filter(|s| !falsy(s))
            .unwrap_or_else(|| Value::String("unavailable".into())),
        "reason": checklist.get("reason").cloned().unwrap_or(Value::Null),
        "complete": checklist.get("complete").cloned().unwrap_or(Value::Null),
        "counts": checklist.get("counts").cloned().unwrap_or(Value::Null),
        "axes": axes,
    })
}

/// Resolve one retained scan through the project history, never a caller path
pub fn scan_row(directory: &Path, scan_uuid: Option<&str>) -> ProjectsResult<Map<String, Value>> {
    let (root, _project) = workspace::load(directory)?;
    let (rows, _errors) =
        spiderhead_store::history::catalog(&root.join("scans")).map_err(ProjectsError::Store)?;
    if rows.is_empty() {
        return Err(value("project has no retained scans"));
    }
    if scan_uuid.is_none() {
        return Ok(rows.into_iter().next().unwrap_or_default());
    }
    let wanted = scan_uuid.unwrap_or_default();
    rows.into_iter()
        .find(|row| row.get("uuid").and_then(Value::as_str) == Some(wanted))
        .ok_or_else(|| value("retained scan is not part of this project"))
}

/// Small finding page entry; full evidence stays in the retained artifact.
pub fn finding_summary(
    issue: &Value,
    ordinal: i64,
    affected_urls: Option<i64>,
    via_group: bool,
) -> ProjectsResult<Value> {
    let Some(object) = issue.as_object() else {
        return Err(value("retained finding is not an object"));
    };
    let get = |key: &str| object.get(key).cloned().unwrap_or(Value::Null);
    let target = if falsy(&get("target_url")) {
        get("url")
    } else {
        get("target_url")
    };
    let message = if falsy(&get("message")) {
        get("text")
    } else {
        get("message")
    };
    let mut result = Map::new();
    result.insert("ordinal".into(), Value::from(ordinal));
    result.insert("id".into(), get("id"));
    result.insert("check".into(), get("check"));
    result.insert("severity".into(), get("severity"));
    result.insert("target_url".into(), target);
    result.insert("message".into(), message);
    result.insert("fingerprint".into(), get("fingerprint"));
    if let Some(affected) = affected_urls {
        result.insert("affected_urls".into(), Value::from(affected));
    }
    if via_group {
        result.insert("via_group".into(), Value::Bool(true));
    }
    let mut truncated: Vec<String> = issue
        .get("truncated_fields")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    for (key, item) in result.clone().iter() {
        if let Some(text) = item.as_str() {
            if text.chars().count() > 4096 {
                let shortened: String = text.chars().take(4096).collect();
                result.insert(key.clone(), Value::String(format!("{shortened}…")));
                truncated.push(key.clone());
            }
        }
    }
    if !truncated.is_empty() {
        let mut unique: Vec<String> = truncated
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        unique.sort();
        result.insert(
            "truncated_fields".into(),
            Value::Array(unique.into_iter().map(Value::String).collect()),
        );
    }
    Ok(Value::Object(result))
}

fn truncate_chars(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

/// Retained finding evidence bounded so a terminal page never becomes a dump
pub fn bounded_value(item: &Value, depth: usize) -> Value {
    if depth >= 3 {
        return Value::String("[nested evidence omitted]".into());
    }
    match item {
        Value::Null | Value::Bool(_) | Value::Number(_) => item.clone(),
        Value::String(text) => {
            if text.chars().count() > 4096 {
                Value::String(format!("{}…", truncate_chars(text, 4096)))
            } else {
                item.clone()
            }
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .take(20)
                .map(|entry| bounded_value(entry, depth + 1))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .take(40)
                .map(|(key, entry)| (truncate_chars(key, 128), bounded_value(entry, depth + 1)))
                .collect(),
        ),
    }
}

/// Whether the bounded detail projection omits anything.
pub fn projection_truncated(item: &Value, depth: usize) -> bool {
    if depth >= 3 {
        return true;
    }
    match item {
        Value::String(text) => text.chars().count() > 4096,
        Value::Array(items) => {
            items.len() > 20
                || items
                    .iter()
                    .any(|entry| projection_truncated(entry, depth + 1))
        }
        Value::Object(map) => {
            map.len() > 40
                || map.iter().any(|(key, entry)| {
                    key.chars().count() > 128 || projection_truncated(entry, depth + 1)
                })
        }
        _ => false,
    }
}

/// Groups of the retained audit from its bounded legacy JSON slot
pub fn legacy_groups(row: &Map<String, Value>) -> Vec<Value> {
    let Some(path) = row.get("path").and_then(Value::as_str) else {
        return Vec::new();
    };
    match spiderhead_store::scan_artifact::read_audit(Path::new(path)) {
        Ok(document) => document
            .get("groups")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// Member counts for named legacy groups.
pub fn group_size_map(groups: &[Value], group_ids: &[Option<String>]) -> BTreeMap<String, i64> {
    let wanted: BTreeSet<&str> = group_ids.iter().filter_map(|id| id.as_deref()).collect();
    let mut sizes = BTreeMap::new();
    for group in groups {
        let Some(object) = group.as_object() else {
            continue;
        };
        let Some(group_id) = object.get("group_id").and_then(Value::as_str) else {
            continue;
        };
        if !wanted.contains(group_id) {
            continue;
        }
        let count = match object.get("count") {
            Some(Value::Number(number)) if number.is_i64() || number.is_u64() => number
                .as_i64()
                .or_else(|| number.as_u64().map(|u| u as i64))
                .unwrap_or(0),
            _ => object
                .get("urls")
                .and_then(Value::as_array)
                .map(|list| list.len() as i64)
                .unwrap_or(0),
        };
        sizes.insert(group_id.to_string(), count);
    }
    sizes
}

pub(crate) fn open_companion(path: &Path) -> ProjectsResult<rusqlite::Connection> {
    let con = spiderhead_core::sqlite::open_readonly(
        path,
        Duration::from_secs(5),
        &[
            "PRAGMA trusted_schema=OFF",
            "PRAGMA query_only=ON",
            "PRAGMA cache_size=-8192",
            "PRAGMA temp_store=FILE",
        ],
    )
    .map_err(|e| value(e.to_string()))?;
    con.create_scalar_function(
        "casefold",
        1,
        rusqlite::functions::FunctionFlags::default(),
        |ctx| {
            let text = match ctx.get_raw(0) {
                ValueRef::Null => String::new(),
                ValueRef::Integer(0) => String::new(),
                ValueRef::Integer(number) => number.to_string(),
                ValueRef::Real(number) => {
                    if number == 0.0 {
                        String::new()
                    } else {
                        pyjson::python_float_repr(number)
                    }
                }
                ValueRef::Text(bytes) => String::from_utf8_lossy(bytes).into_owned(),
                ValueRef::Blob(bytes) => format!("b'{:?}'", String::from_utf8_lossy(bytes)),
            };
            Ok(icu_casemap::CaseMapper::new()
                .fold_string(&text)
                .to_string())
        },
    )
    .map_err(|e| value(e.to_string()))?;
    Ok(con)
}

/// Member counts for named audit.v2 groups without their member arrays
pub fn v2_group_sizes(
    con: &rusqlite::Connection,
    group_ids: &[Option<String>],
) -> ProjectsResult<BTreeMap<String, i64>> {
    let mut wanted: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for id in group_ids.iter().filter_map(|id| id.as_deref()) {
        if seen.insert(id.to_string()) {
            wanted.push(id.to_string());
        }
    }
    if wanted.is_empty() {
        return Ok(BTreeMap::new());
    }
    let placeholders = vec!["?"; wanted.len()].join(",");
    let sql = format!(
        "SELECT json_extract(value_json,'$.group_id'), COALESCE(json_extract(value_json,'$.urls.count'), json_array_length(json_extract(value_json,'$.urls'))) FROM items WHERE pointer='/groups' AND json_extract(value_json,'$.group_id') IN ({placeholders})"
    );
    let mut statement = con.prepare(&sql).map_err(|e| value(e.to_string()))?;
    let params: Vec<&dyn rusqlite::types::ToSql> = wanted
        .iter()
        .map(|id| id as &dyn rusqlite::types::ToSql)
        .collect();
    let mut sizes = BTreeMap::new();
    let mut rows = statement
        .query(params.as_slice())
        .map_err(|e| value(e.to_string()))?;
    while let Some(row) = rows.next().map_err(|e| value(e.to_string()))? {
        let group_id = row
            .get::<_, rusqlite::types::Value>(0)
            .map_err(|e| value(e.to_string()))?;
        let size = row
            .get::<_, rusqlite::types::Value>(1)
            .map_err(|e| value(e.to_string()))?;
        if let (rusqlite::types::Value::Text(id), rusqlite::types::Value::Integer(number)) =
            (group_id, size)
        {
            sizes.insert(id, number);
        }
    }
    Ok(sizes)
}

/// Group ids whose member list contains the URL.
pub fn v2_group_ids_including(
    con: &rusqlite::Connection,
    storage_version: i64,
    url: &str,
) -> ProjectsResult<BTreeSet<String>> {
    let member = if storage_version == 2 {
        " EXISTS (SELECT 1 FROM json_each(json_extract(items.value_json,'$.urls')) member WHERE member.value=?)"
    } else {
        " EXISTS (SELECT 1 FROM group_members member WHERE member.group_ordinal=items.ordinal AND json_extract(member.value_json,'$')=?)"
    };
    let sql = format!(
        "SELECT DISTINCT json_extract(items.value_json,'$.group_id') FROM items WHERE pointer='/groups' AND{member}"
    );
    let mut statement = con.prepare(&sql).map_err(|e| value(e.to_string()))?;
    let mut rows = statement.query([url]).map_err(|e| value(e.to_string()))?;
    let mut ids = BTreeSet::new();
    while let Some(row) = rows.next().map_err(|e| value(e.to_string()))? {
        if let rusqlite::types::Value::Text(id) = row
            .get::<_, rusqlite::types::Value>(0)
            .map_err(|e| value(e.to_string()))?
        {
            ids.insert(id);
        }
    }
    Ok(ids)
}

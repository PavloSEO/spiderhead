//! Portable, declarative finding views stored with a local project (read side).
//!
//! The write side (`save_view`, `rename_view`, `delete_view`) is not in this module yet;
//! archive validation needs the read path only.

use std::path::Path;

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::error::{value, ProjectsResult};
use crate::runtime::read_document;
use crate::workspace::load as workspace_load;

mod save_view;
pub use save_view::*;

/// Document format.
pub const FORMAT: &str = "spiderhead.project-finding-views.v1";
/// Schema version.
pub const VERSION: i64 = 1;
/// Maximum stored views.
pub const MAX_VIEWS: usize = 100;
/// Maximum values per filter field.
pub const MAX_FILTER_VALUES: usize = 100;
/// Maximum `page_size`.
pub const MAX_PAGE_SIZE: i64 = 1000;
/// Maximum `offset`.
pub const MAX_OFFSET: i64 = 1_000_000;
const NAME_MAX: usize = 64;
const FIELDS: &[&str] = &[
    "severity",
    "check",
    "url",
    "text",
    "status_code",
    "occurrences_count",
    "fix_hint",
    "details",
    "locations",
    "segment",
];
const SORT_FIELDS: &[&str] = &[
    "severity",
    "check",
    "url",
    "occurrences_count",
    "status_code",
    "segment",
];
const DEFAULT_COLUMNS: &[&str] = &["severity", "check", "url", "text", "fix_hint"];
const SEVERITY_ORDER: &[&str] = &["notice", "warning", "critical"];

fn name(raw: &Value) -> ProjectsResult<String> {
    let Some(text) = raw.as_str() else {
        return Err(value(
            "view name must be a lowercase identifier of at most 64 characters",
        ));
    };
    let valid = !text.is_empty()
        && text.chars().count() <= NAME_MAX
        && text.starts_with(|c: char| c.is_ascii_lowercase())
        && text
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'));
    if !valid {
        return Err(value(
            "view name must be a lowercase identifier of at most 64 characters",
        ));
    }
    Ok(text.to_owned())
}

fn check_pattern(text: &str) -> bool {
    !text.is_empty()
        && text.chars().count() <= 128
        && text.starts_with(|c: char| c.is_ascii_alphanumeric())
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-'))
}

fn url(raw: &Value, field: &str) -> ProjectsResult<String> {
    let Some(text) = raw.as_str() else {
        return Err(value(format!(
            "{field} must be a bounded absolute HTTP(S) URL"
        )));
    };
    if text.chars().count() > 2048 {
        return Err(value(format!(
            "{field} must be a bounded absolute HTTP(S) URL"
        )));
    }
    let normalized = spiderhead_core::net::normalize_url(text);
    if normalized != text
        || !matches!(
            crate::urls::urlsplit(text).map(|p| p.scheme),
            Ok(s) if s == "http" || s == "https"
        )
    {
        return Err(value(format!(
            "{field} must be a normalized absolute HTTP(S) URL"
        )));
    }
    Ok(text.to_owned())
}

fn string_list(raw: &Value, field: &str, check: bool) -> ProjectsResult<Vec<String>> {
    let Some(list) = raw.as_array() else {
        return Err(value(format!(
            "{field} must be a list of at most {MAX_FILTER_VALUES} values"
        )));
    };
    if list.len() > MAX_FILTER_VALUES {
        return Err(value(format!(
            "{field} must be a list of at most {MAX_FILTER_VALUES} values"
        )));
    }
    let mut result = Vec::new();
    for item in list {
        let Some(text) = item.as_str() else {
            return Err(value(format!(
                "{field} values must be non-empty bounded strings"
            )));
        };
        if text.trim().is_empty() || text.chars().count() > 2048 {
            return Err(value(format!(
                "{field} values must be non-empty bounded strings"
            )));
        }
        if check && !check_pattern(text) {
            return Err(value(format!("{field} contains an invalid value")));
        }
        result.push(text.to_owned());
    }
    if result.len()
        != result
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    {
        return Err(value(format!("{field} values must be unique")));
    }
    Ok(result)
}

/// `validate_definition`: normalize a closed view definition; no SQL, regex, or
/// executable expression is accepted.
pub fn validate_definition(raw: &Value) -> ProjectsResult<Value> {
    let Some(obj) = raw.as_object() else {
        return Err(value("view definition has unsupported fields"));
    };
    let allowed: std::collections::BTreeSet<&str> =
        ["name", "filters", "sort", "columns", "page_size"]
            .into_iter()
            .collect();
    if obj.keys().any(|k| !allowed.contains(k.as_str())) {
        return Err(value("view definition has unsupported fields"));
    }
    let name = name(&raw["name"])?;
    let filters_obj = match raw.get("filters") {
        None => Map::new(),
        Some(f) => f
            .as_object()
            .cloned()
            .ok_or_else(|| value("view filters support severity, check, url, and segment only"))?,
    };
    if filters_obj
        .keys()
        .any(|k| !["severity", "check", "url", "segment"].contains(&k.as_str()))
    {
        return Err(value(
            "view filters support severity, check, url, and segment only",
        ));
    }
    let mut normalized_filters = Map::new();
    for (field, values) in &filters_obj {
        let list = match field.as_str() {
            "severity" => {
                let parsed = string_list(values, "severity", false)?;
                if parsed
                    .iter()
                    .any(|item| !SEVERITY_ORDER.contains(&item.as_str()))
                {
                    return Err(value(
                        "severity filters support critical, warning, and notice",
                    ));
                }
                let mut parsed = parsed;
                parsed
                    .sort_by_key(|item| SEVERITY_ORDER.iter().position(|s| s == item).unwrap_or(0));
                parsed
            }
            "check" => {
                let mut parsed = string_list(values, "check", true)?;
                parsed.sort();
                parsed
            }
            "url" => {
                let mut parsed = string_list(values, "url", false)?
                    .iter()
                    .map(|item| url(&json!(item), "URL filter"))
                    .collect::<ProjectsResult<Vec<_>>>()?;
                parsed.sort();
                parsed
            }
            _ => {
                let mut parsed = string_list(values, "segment", false)?;
                parsed.sort();
                parsed
            }
        };
        normalized_filters.insert(field.clone(), json!(list));
        if normalized_filters[field]
            .as_array()
            .is_some_and(Vec::is_empty)
        {
            return Err(value(format!(
                "{field} filter cannot be empty; omit it to leave the field unfiltered"
            )));
        }
    }
    let sort = match raw.get("sort") {
        None => json!({"field": "severity", "direction": "desc"}),
        Some(s) => s.clone(),
    };
    let sort_obj = sort
        .as_object()
        .ok_or_else(|| value("sort requires field and direction"))?;
    let sort_keys: std::collections::BTreeSet<&str> = sort_obj.keys().map(String::as_str).collect();
    if sort_keys != ["field", "direction"].into_iter().collect() {
        return Err(value("sort requires field and direction"));
    }
    if !sort["field"]
        .as_str()
        .is_some_and(|f| SORT_FIELDS.contains(&f))
    {
        return Err(value(
            "sort field must be severity, check, url, segment, occurrences_count, or status_code",
        ));
    }
    if !sort["direction"]
        .as_str()
        .is_some_and(|d| d == "asc" || d == "desc")
    {
        return Err(value("sort direction must be asc or desc"));
    }
    let columns = raw
        .get("columns")
        .cloned()
        .unwrap_or_else(|| json!(DEFAULT_COLUMNS));
    let column_list = columns.as_array().ok_or_else(|| {
        value(format!(
            "columns must select between 1 and {} finding fields",
            FIELDS.len()
        ))
    })?;
    if column_list.is_empty() || column_list.len() > FIELDS.len() {
        return Err(value(format!(
            "columns must select between 1 and {} finding fields",
            FIELDS.len()
        )));
    }
    if column_list
        .iter()
        .any(|c| !c.as_str().is_some_and(|s| FIELDS.contains(&s)))
    {
        return Err(value("columns contain an unsupported finding field"));
    }
    if column_list.len()
        != column_list
            .iter()
            .filter_map(|c| c.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    {
        return Err(value("columns must be unique"));
    }
    let page_size = raw.get("page_size").cloned().unwrap_or(json!(100));
    match page_size.as_i64() {
        Some(size) if (1..=MAX_PAGE_SIZE).contains(&size) && page_size.is_i64() => {}
        _ => {
            return Err(value(format!(
                "page_size must be within 1..{MAX_PAGE_SIZE}"
            )))
        }
    }
    Ok(json!({
        "name": name,
        "filters": Value::Object(normalized_filters),
        "sort": {"field": sort["field"], "direction": sort["direction"]},
        "columns": columns,
        "page_size": page_size,
    }))
}

fn identity(project_uuid: &str, name: &str) -> String {
    let digest = Sha256::digest(format!("{project_uuid}\0{name}").as_bytes());
    format!(
        "view-{}",
        digest[..12]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

fn validate_store(document: &Value, project_uuid: &str) -> ProjectsResult<()> {
    let Some(obj) = document.as_object() else {
        return Err(value("finding-views.json has an unsupported shape"));
    };
    let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
    if keys
        != ["format", "version", "project_uuid", "revision", "views"]
            .into_iter()
            .collect()
    {
        return Err(value("finding-views.json has an unsupported shape"));
    }
    if document["format"].as_str() != Some(FORMAT)
        || !document["version"].is_i64()
        || document["version"].as_i64() != Some(VERSION)
    {
        return Err(value("finding views use an unsupported schema version"));
    }
    if document["project_uuid"].as_str() != Some(project_uuid) {
        return Err(value("finding views belong to a different project"));
    }
    if !document["revision"].is_i64() || document["revision"].as_i64().is_some_and(|r| r < 1) {
        return Err(value("finding view store revision is invalid"));
    }
    let views = document["views"]
        .as_object()
        .ok_or_else(|| value("finding view catalogue is invalid or too large"))?;
    if views.len() > MAX_VIEWS {
        return Err(value("finding view catalogue is invalid or too large"));
    }
    for (key, entry) in views {
        let entry_keys: std::collections::BTreeSet<&str> = entry
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        if entry_keys
            != ["id", "schema_version", "revision", "definition"]
                .into_iter()
                .collect()
        {
            return Err(value("saved finding view has an unsupported shape"));
        }
        let name = name(&json!(key))?;
        if entry["id"].as_str() != Some(identity(project_uuid, &name).as_str()) {
            return Err(value("saved finding view identity is invalid"));
        }
        if !entry["schema_version"].is_i64() || entry["schema_version"].as_i64() != Some(1) {
            return Err(value("saved finding view schema version is unsupported"));
        }
        if !entry["revision"].is_i64() || entry["revision"].as_i64().is_some_and(|r| r < 1) {
            return Err(value("saved finding view revision is invalid"));
        }
        let definition = validate_definition(&entry["definition"])?;
        if definition["name"].as_str() != Some(name.as_str()) {
            return Err(value("saved finding view name does not match its key"));
        }
    }
    Ok(())
}

fn read(directory: &Path) -> ProjectsResult<(std::path::PathBuf, Value, Option<Value>)> {
    let (root, project) = workspace_load(directory)?;
    let document = read_document(&root, "finding-views.json")?;
    if let Some(doc) = &document {
        validate_store(doc, project["project_uuid"].as_str().unwrap_or_default())?;
    }
    Ok((root, project, document))
}

/// `list_views`: named project views without reading or changing audit evidence.
pub fn list_views(directory: &Path) -> ProjectsResult<Value> {
    let (_root, project, document) = read(directory)?;
    let views = document
        .as_ref()
        .and_then(|d| d["views"].as_object())
        .cloned()
        .unwrap_or_default();
    let mut items: Vec<Value> = views
        .iter()
        .map(|(name, entry)| {
            json!({
                "id": entry["id"],
                "name": name,
                "schema_version": entry["schema_version"],
                "revision": entry["revision"],
                "definition": entry["definition"],
            })
        })
        .collect();
    items.sort_by_key(|v| v["name"].as_str().unwrap_or_default().to_owned());
    Ok(json!({
        "ok": true,
        "schema_version": VERSION,
        "project_uuid": project["project_uuid"],
        "config_revision": document.as_ref().map(|d| d["revision"].clone()).unwrap_or(json!(0)),
        "views": items,
    }))
}

/// `show_view`: read one named view and its stable identity/config revisions.
pub fn show_view(directory: &Path, name: &str) -> ProjectsResult<Value> {
    let (_root, project, document) = read(directory)?;
    let view_name = self::name(&json!(name))?;
    let entry = document
        .as_ref()
        .and_then(|d| d["views"].as_object())
        .and_then(|views| views.get(&view_name));
    let Some(entry) = entry else {
        return Err(value(format!("finding view '{view_name}' does not exist")));
    };
    Ok(json!({
        "ok": true,
        "project_uuid": project["project_uuid"],
        "config_revision": document.as_ref().map(|d| d["revision"].clone()).unwrap_or(Value::Null),
        "view": {
            "id": entry["id"],
            "name": view_name,
            "schema_version": entry["schema_version"],
            "revision": entry["revision"],
            "definition": entry["definition"],
        },
    }))
}

fn checked_store(
    directory: &Path,
    expected_revision: i64,
) -> ProjectsResult<(std::path::PathBuf, Value, Option<Value>, i64)> {
    if expected_revision < 0 {
        return Err(value("expected_revision must be a non-negative integer"));
    }
    let (root, project, current) = read(directory)?;
    let revision = current
        .as_ref()
        .and_then(|d| d["revision"].as_i64())
        .unwrap_or(0);
    if expected_revision != revision {
        return Err(value(format!(
            "finding view revision conflict: current revision is {revision}"
        )));
    }
    Ok((root, project, current, revision))
}

fn write_views(
    root: &Path,
    project: &Value,
    views: &Map<String, Value>,
    revision: i64,
) -> ProjectsResult<()> {
    let document = json!({
        "format": FORMAT,
        "version": VERSION,
        "project_uuid": project["project_uuid"],
        "revision": revision + 1,
        "views": views,
    });
    crate::runtime::write_document(root, "finding-views.json", &document, Some(revision))
}

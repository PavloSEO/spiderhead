use super::*;

mod sort_key_cmp;
pub use sort_key_cmp::*;

/// `save_view`: create or update one named view under an expected config revision.
pub fn save_view(directory: &Path, view: &Value, expected_revision: i64) -> ProjectsResult<Value> {
    let definition = validate_definition(view)?;
    let (root, project, current, revision) = checked_store(directory, expected_revision)?;
    let mut views = current
        .as_ref()
        .and_then(|d| d["views"].as_object().cloned())
        .unwrap_or_default();
    let view_name = definition["name"].as_str().unwrap_or_default().to_string();
    let previous = views.get(&view_name);
    if previous.is_none() && views.len() >= MAX_VIEWS {
        return Err(value(format!(
            "a project can store at most {MAX_VIEWS} finding views"
        )));
    }
    let next = previous.and_then(|p| p["revision"].as_i64()).unwrap_or(0) + 1;
    views.insert(
        view_name.clone(),
        json!({
            "id": identity(project["project_uuid"].as_str().unwrap_or_default(), &view_name),
            "schema_version": 1,
            "revision": next,
            "definition": definition,
        }),
    );
    write_views(&root, &project, &views, revision)?;
    show_view(directory, &view_name)
}

/// `delete_view`: remove one named view guarded by the expected config revision.
pub fn delete_view(directory: &Path, name: &str, expected_revision: i64) -> ProjectsResult<Value> {
    let view_name = self::name(&json!(name))?;
    let (root, project, current, revision) = checked_store(directory, expected_revision)?;
    let mut views = current
        .as_ref()
        .and_then(|d| d["views"].as_object().cloned())
        .unwrap_or_default();
    if views.remove(&view_name).is_none() {
        return Err(value(format!("finding view '{view_name}' does not exist")));
    }
    write_views(&root, &project, &views, revision)?;
    Ok(json!({
        "ok": true,
        "project_uuid": project["project_uuid"],
        "config_revision": revision + 1,
        "deleted": view_name,
    }))
}

/// `rename_view`: rename one named view keeping its definition.
pub fn rename_view(
    directory: &Path,
    name: &str,
    new_name: &str,
    expected_revision: i64,
) -> ProjectsResult<Value> {
    let old_name = self::name(&json!(name))?;
    let target = self::name(&json!(new_name))?;
    let (root, project, current, revision) = checked_store(directory, expected_revision)?;
    let mut views = current
        .as_ref()
        .and_then(|d| d["views"].as_object().cloned())
        .unwrap_or_default();
    let Some(entry) = views.get(&old_name).cloned() else {
        return Err(value(format!("finding view '{old_name}' does not exist")));
    };
    if views.contains_key(&target) {
        return Err(value(format!("finding view {target:?} already exists")));
    }
    views.remove(&old_name);
    let mut definition = entry["definition"].as_object().cloned().unwrap_or_default();
    definition.insert("name".into(), json!(target));
    views.insert(
        target.clone(),
        json!({
            "id": identity(project["project_uuid"].as_str().unwrap_or_default(), &target),
            "schema_version": 1,
            "revision": entry["revision"].as_i64().unwrap_or(0) + 1,
            "definition": Value::Object(definition),
        }),
    );
    write_views(&root, &project, &views, revision)?;
    show_view(directory, &target)
}

pub(super) fn field(finding: &Value, field: &str) -> Value {
    match field {
        "url" => {
            let target = finding.get("target_url").cloned().unwrap_or(Value::Null);
            if crate::schedule::falsy(&target) {
                finding.get("url").cloned().unwrap_or(Value::Null)
            } else {
                target
            }
        }
        "text" => {
            let message = finding.get("message").cloned().unwrap_or(Value::Null);
            if crate::schedule::falsy(&message) {
                finding.get("text").cloned().unwrap_or(Value::Null)
            } else {
                message
            }
        }
        _ => finding.get(field).cloned().unwrap_or(Value::Null),
    }
}

pub(super) const SITE_AUDIT_SCHEMA: &str = "spiderhead.site-audit/1";
pub(super) const UNSEGMENTED: &str = "unsegmented";

pub(super) fn segment_definitions(document: &Value) -> Option<Vec<Value>> {
    let config = document
        .get("run")
        .filter(|r| !crate::schedule::falsy(r))
        .and_then(|r| r.get("crawl_config"))
        .filter(|c| !crate::schedule::falsy(c))?;
    if !config.is_object() {
        return None;
    }
    let analysis = config
        .get("analysis.segments")
        .filter(|a| !crate::schedule::falsy(a));
    if let Some(analysis) = analysis {
        return analysis.as_array().cloned();
    }
    if config
        .get("analysis.segments")
        .is_some_and(|a| a.is_array())
    {
        return None;
    }
    let scope = config
        .get("scope.segments")
        .filter(|s| !crate::schedule::falsy(s))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if config.get("scope.segments").is_some() && !config["scope.segments"].is_array() {
        return None;
    }
    if scope.len() > MAX_FILTER_VALUES {
        return None;
    }
    let mut definitions = Vec::new();
    for item in &scope {
        let name = item.get("name").and_then(Value::as_str);
        if !item.is_object() || name.is_none() {
            return None;
        }
        let mut rules = Vec::new();
        if !crate::schedule::falsy(item.get("prefix").unwrap_or(&Value::Null)) {
            rules.push(json!({"op": "prefix", "field": "path", "value": item["prefix"]}));
        }
        if !crate::schedule::falsy(item.get("host").unwrap_or(&Value::Null)) {
            rules.push(json!({"op": "eq", "field": "host", "value": item["host"]}));
        }
        if !crate::schedule::falsy(item.get("pattern").unwrap_or(&Value::Null)) {
            rules.push(json!({"op": "regex", "field": "url", "value": item["pattern"]}));
        }
        definitions.push(json!({"name": item["name"], "rules": rules}));
    }
    if definitions.is_empty() {
        None
    } else {
        Some(definitions)
    }
}

// `assign_segments` lives in `spiderhead_reports::segments`, the shared segment evaluator;
// this adapter keeps the `(order, url -> primary)` shape the view code consumes.

/// `(order, url -> primary segment)` returned by the segment evaluator.
pub(super) type SegmentAssignment = (
    Vec<String>,
    std::collections::BTreeMap<String, Option<String>>,
);

pub(super) fn assign_segments(
    pages: &[Value],
    definitions: &[Value],
) -> ProjectsResult<SegmentAssignment> {
    let assignment = spiderhead_reports::segments::assign_segments(pages, definitions)
        .map_err(|e| value(e.to_string()))?;
    let order: Vec<String> = assignment
        .get("order")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let primary: std::collections::BTreeMap<String, Option<String>> = assignment
        .get("primary")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .map(|(k, v)| (k.clone(), v.as_str().map(str::to_owned)))
                .collect()
        })
        .unwrap_or_default();
    Ok((order, primary))
}
pub(super) fn assign_view_segments(
    document: &Value,
    rows: &[Value],
) -> ProjectsResult<Option<(Vec<String>, std::collections::BTreeSet<String>)>> {
    let Some(definitions) = segment_definitions(document) else {
        return Ok(None);
    };
    let pages = document
        .get("pages")
        .filter(|p| !crate::schedule::falsy(p))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut page_records = Vec::new();
    for page in &pages {
        let Some(url) = page.get("url").and_then(Value::as_str) else {
            continue;
        };
        if url.is_empty() {
            continue;
        }
        let parts = crate::urls::urlsplit(url)?;
        let mut record = page.as_object().cloned().unwrap_or_default();
        record.insert("url".into(), json!(url));
        record.insert("path".into(), json!(parts.path));
        record.insert(
            "host".into(),
            json!(parts.hostname().unwrap_or_default().to_lowercase()),
        );
        page_records.push(Value::Object(record));
    }
    let (order, primary) = assign_segments(&page_records, &definitions)?;
    let mut known: std::collections::BTreeSet<String> = order.into_iter().collect();
    known.insert("default".to_string());
    let no_pages = document.get("pages").is_none_or(crate::schedule::falsy);
    let crawl_partial = document
        .get("run")
        .filter(|r| !crate::schedule::falsy(r))
        .and_then(|r| r.get("crawl_partial"))
        .is_some_and(|p| !crate::schedule::falsy(p));
    let mut result = Vec::new();
    for row in rows {
        let target = field(row, "url");
        let assigned = if !target.is_string() || target.as_str().is_some_and(str::is_empty) {
            "default".to_string()
        } else if let Some(segment) = primary.get(target.as_str().unwrap_or_default()) {
            match segment.as_deref() {
                None | Some(UNSEGMENTED) => "default".to_string(),
                Some(name) => name.to_string(),
            }
        } else if no_pages && !crawl_partial {
            String::new()
        } else {
            let url = target.as_str().unwrap_or_default();
            let parts = crate::urls::urlsplit(url)?;
            let isolated = assign_segments(
                &[json!({
                    "url": url,
                    "path": parts.path,
                    "host": parts.hostname().unwrap_or_default().to_lowercase(),
                })],
                &definitions,
            )?
            .1
            .get(url)
            .cloned()
            .flatten();
            match isolated.as_deref() {
                None | Some(UNSEGMENTED) => "default".to_string(),
                Some(name) => name.to_string(),
            }
        };
        result.push(assigned);
    }
    Ok(Some((result, known)))
}

pub(super) fn sort_key(item: &Value, field: &str) -> (bool, Value) {
    if item.is_null() || item.as_str().is_some_and(str::is_empty) {
        return (true, Value::Null);
    }
    match field {
        "severity" => {
            let rank = match crate::bindings::py_str(item).to_lowercase().as_str() {
                "notice" => Some(0),
                "warning" => Some(1),
                "critical" => Some(2),
                _ => None,
            };
            (rank.is_none(), rank.map(Value::from).unwrap_or(Value::Null))
        }
        "occurrences_count" | "status_code" => match item {
            Value::Number(n) => (false, json!(n.as_f64().unwrap_or(0.0))),
            _ => (true, Value::Null),
        },
        _ => (
            false,
            Value::String(
                icu_casemap::CaseMapper::new()
                    .fold_string(&crate::bindings::py_str(item))
                    .to_string(),
            ),
        ),
    }
}

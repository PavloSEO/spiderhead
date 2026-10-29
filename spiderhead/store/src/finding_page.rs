//! Bounded findings browse over one retained scan through its project history.
//!
//! Paged findings for one project, resolved through its scan catalog.
//! and its group and projection lookups.
//! project's `scans/` catalog (never a caller path), the audit.v2 companion answers the
//! page in SQL, and grouped findings answer for every member URL.

use std::path::Path;

use serde_json::{json, Map, Value};

use crate::audit_v2::{audit_v2_path, AuditV2Reader};
use crate::error::StoreError;
use crate::history::catalog;

/// Default page size (`FINDING_PAGE_SIZE`).
pub const FINDING_PAGE_SIZE: i64 = 50;
/// Page size ceiling (`MAX_FINDING_PAGE_SIZE`).
pub const MAX_FINDING_PAGE_SIZE: i64 = 100;
/// Sort fields a findings page accepts.
pub const FINDING_SORTS: [&str; 4] = ["severity", "check", "target_url", "id"];

fn invalid(message: impl Into<String>) -> StoreError {
    StoreError::Invalid(message.into())
}

/// `projects.workspace._directory`: an existing, non-symlink directory, resolved.
fn resolve_directory(path: &str, label: &str) -> Result<std::path::PathBuf, StoreError> {
    if path.is_empty() {
        return Err(invalid(format!("{label} is required")));
    }
    let value = Path::new(path);
    let meta = std::fs::symlink_metadata(value)
        .map_err(|_| invalid(format!("{label} must be an existing non-symlink directory")))?;
    if meta.is_symlink() || !meta.is_dir() {
        return Err(invalid(format!(
            "{label} must be an existing non-symlink directory"
        )));
    }
    value.canonicalize().map_err(|e| {
        invalid(format!(
            "{label} must be an existing non-symlink directory: {e}"
        ))
    })
}

/// Host and target check for a project site; accepts the public
/// hostnames a project manifest records, with the same bounds and scheme rules as ingest.
fn site_identity(site: &Value) -> Result<(), StoreError> {
    let Some(site) = site.as_object() else {
        return Err(invalid("project site identity is invalid"));
    };
    if site.len() != 3 || !site.contains_key("host") || !site.contains_key("label") {
        return Err(invalid("project site identity is invalid"));
    }
    let target = site["target"].as_str().unwrap_or("");
    if target.chars().count() > 2048
        || !(target.starts_with("http://") || target.starts_with("https://"))
    {
        return Err(invalid("project site identity is invalid"));
    }
    if let Some(label) = site["label"].as_str() {
        if label.trim().is_empty() || label.chars().count() > 128 {
            return Err(invalid("project site identity is invalid"));
        }
    } else if !site["label"].is_null() {
        return Err(invalid("project site identity is invalid"));
    }
    Ok(())
}

/// `projects.workspace._load`: validate one project directory and return its root.
fn load_project(directory: &str) -> Result<std::path::PathBuf, StoreError> {
    let root = resolve_directory(directory, "project directory")?;
    for name in ["scans", "reports"] {
        let child = root.join(name);
        let meta = std::fs::symlink_metadata(&child);
        let ok = meta.as_ref().is_ok_and(|m| m.is_dir() && !m.is_symlink());
        if !ok {
            return Err(invalid(format!(
                "project {name} directory is missing or unsafe"
            )));
        }
    }
    let log = root.join("log.md");
    let log_ok = std::fs::symlink_metadata(&log).is_ok_and(|m| m.is_file() && !m.is_symlink());
    if !log_ok {
        return Err(invalid("project log is missing or unsafe"));
    }
    let manifest = root.join("project.json");
    let manifest_ok =
        std::fs::symlink_metadata(&manifest).is_ok_and(|m| m.is_file() && !m.is_symlink());
    if !manifest_ok {
        return Err(invalid(
            "project.json is missing, unsafe, or exceeds its byte limit",
        ));
    }
    let text = std::fs::read_to_string(&manifest)
        .map_err(|_| invalid("project.json is not valid JSON"))?;
    let document: Value =
        serde_json::from_str(&text).map_err(|_| invalid("project.json is not valid JSON"))?;
    let Some(document) = document.as_object() else {
        return Err(invalid("project.json has an unsupported shape"));
    };
    let expected: [&str; 9] = [
        "format",
        "version",
        "project_uuid",
        "created_at",
        "site",
        "facts",
        "template_references",
        "profile_references",
        "artifact_directories",
    ];
    let own = document.len() - usize::from(document.contains_key("schema_version"));
    if own != expected.len() || !expected.iter().all(|key| document.contains_key(*key)) {
        return Err(invalid("project.json has an unsupported shape"));
    }
    if document["format"] != json!("spiderhead.project.v1") || document["version"] != json!(1) {
        return Err(invalid("project.json format or version is unsupported"));
    }
    site_identity(&document["site"])?;
    if !document["facts"].is_array()
        || !document["template_references"].is_array()
        || !document["profile_references"].is_array()
    {
        return Err(invalid("project.json has an unsupported shape"));
    }
    if document["artifact_directories"]
        != json!({"scans": "scans", "reports": "reports", "log": "log.md"})
    {
        return Err(invalid(
            "project artifact references must remain portable relative paths",
        ));
    }
    Ok(root)
}

/// Resolve one retained scan through the project catalog.
fn scan_row(directory: &Path, scan_uuid: Option<&str>) -> Result<Map<String, Value>, StoreError> {
    let (items, _errors) = catalog(&directory.join("scans"))?;
    if items.is_empty() {
        return Err(invalid("project has no retained scans"));
    }
    let Some(uuid) = scan_uuid else {
        return Ok(items[0].clone());
    };
    items
        .iter()
        .find(|item| item["uuid"] == json!(uuid))
        .cloned()
        .ok_or_else(|| invalid("retained scan is not part of this project"))
}

/// Group ids whose member list includes the URL.
fn group_ids_including(reader: &AuditV2Reader, url: &str) -> Vec<String> {
    let member = if reader.storage_version == 2 {
        " EXISTS (SELECT 1 FROM json_each(json_extract(items.value_json,'$.urls')) member\
         \n WHERE member.value=?)"
            .to_string()
    } else {
        " EXISTS (SELECT 1 FROM group_members member\
         \n WHERE member.group_ordinal=items.ordinal\
         \n AND json_extract(member.value_json,'$')=?)"
            .to_string()
    };
    let sql = format!(
        "SELECT DISTINCT json_extract(items.value_json,'$.group_id') FROM items\
         \n WHERE pointer='/groups' AND{member}"
    );
    reader
        .con
        .query_row(&sql, [url], |row| row.get::<_, String>(0))
        .ok()
        .into_iter()
        .collect()
}

/// Member counts for the named groups.
fn group_sizes(reader: &AuditV2Reader, wanted: &[String]) -> Map<String, Value> {
    let unique: Vec<&String> = {
        let mut seen = std::collections::BTreeSet::new();
        wanted
            .iter()
            .filter(|id| seen.insert((*id).clone()))
            .collect()
    };
    let mut out = Map::new();
    if unique.is_empty() {
        return out;
    }
    let placeholders: Vec<&str> = unique.iter().map(|_| "?").collect();
    let sql = format!(
        "SELECT json_extract(value_json,'$.group_id'),\
         \n COALESCE(json_extract(value_json,'$.urls.count'),\
         \n json_array_length(json_extract(value_json,'$.urls')))\
         \n FROM items WHERE pointer='/groups'\
         \n AND json_extract(value_json,'$.group_id') IN ({})",
        placeholders.join(",")
    );
    let params: Vec<String> = unique.iter().map(|id| (*id).clone()).collect();
    let Ok(mut rows) = reader.con.prepare(&sql) else {
        return out;
    };
    if let Ok(iter) = rows.query_map(rusqlite::params_from_iter(params), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    }) {
        for pair in iter.flatten() {
            out.insert(pair.0, json!(pair.1));
        }
    }
    out
}

/// What one audit.v2 findings page reads: source count, matched count, the
/// selected `(ordinal, finding)` rows, group sizes and the via-group ids.
type FindingsPage = Result<
    (
        i64,
        i64,
        Vec<(i64, Map<String, Value>)>,
        Map<String, Value>,
        Vec<String>,
    ),
    StoreError,
>;

/// One findings page over the audit v2 companion.
#[allow(clippy::too_many_arguments)]
fn v2_findings_page(
    reader: &AuditV2Reader,
    offset: i64,
    limit: i64,
    query: &str,
    url: Option<&str>,
    sort: &str,
    descending: bool,
) -> FindingsPage {
    let fields = [
        "id",
        "check",
        "severity",
        "target_url",
        "url",
        "message",
        "text",
    ];
    let expression = fields
        .iter()
        .map(|field| format!("COALESCE(json_extract(value_json,'$.{field}'),'')"))
        .collect::<Vec<_>>()
        .join(" || ' ' || ");
    let mut predicate = format!("pointer='/issues' AND instr(casefold({expression}),?)>0");
    let mut parameters: Vec<Value> = vec![json!(query.trim().to_lowercase())];
    let mut via_group_ids: Vec<String> = Vec::new();
    if let Some(url) = url {
        let target = "COALESCE(NULLIF(json_extract(value_json,'$.target_url'),''),\
                      \njson_extract(value_json,'$.url'))=?"
            .to_string();
        via_group_ids = group_ids_including(reader, url);
        if via_group_ids.is_empty() {
            predicate += &format!(" AND {target}");
        } else {
            let grouped = format!(
                "json_extract(value_json,'$.group_id') IN ({})",
                vec!["?"; via_group_ids.len()].join(",")
            );
            predicate += &format!(" AND ({target} OR {grouped})");
        }
        parameters.push(json!(url));
        parameters.extend(via_group_ids.iter().map(|id| json!(id.clone())));
    }
    let matched: i64 = reader
        .con
        .query_row(
            &format!("SELECT COUNT(*) FROM items WHERE {predicate}"),
            rusqlite::params_from_iter(parameters.iter().map(json_to_sql)),
            |row| row.get(0),
        )
        .map_err(StoreError::from)?;
    let raw_value = if sort == "target_url" {
        format!(
            "COALESCE(NULLIF(json_extract(value_json,'$.{sort}'),''),\
            \njson_extract(value_json,'$.url'))"
        )
    } else {
        format!("json_extract(value_json,'$.{sort}')")
    };
    let value = if sort == "severity" {
        let cases = ["critical", "error", "warning", "notice", "info"]
            .iter()
            .enumerate()
            .map(|(rank, name)| format!("WHEN '{name}' THEN {rank}"))
            .collect::<Vec<_>>()
            .join(" ");
        format!("CASE casefold({raw_value}) {cases} ELSE 99 END")
    } else {
        format!("casefold({raw_value})")
    };
    let direction = if descending { " DESC" } else { " ASC" };
    let order = format!(
        "({raw_value} IS NULL OR {raw_value}='') ASC,{value}{direction},ordinal{direction}"
    );
    let sql = format!(
        "SELECT ordinal,value_json FROM items WHERE {predicate} ORDER BY {order} LIMIT ? OFFSET ?"
    );
    parameters.push(json!(limit));
    parameters.push(json!(offset));
    let mut statement = reader.con.prepare(&sql).map_err(StoreError::from)?;
    let selected = statement
        .query_map(
            rusqlite::params_from_iter(parameters.iter().map(json_to_sql)),
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .map_err(StoreError::from)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)?;
    let selected: Vec<(i64, Map<String, Value>)> = selected
        .into_iter()
        .map(|(ordinal, value_json)| {
            let issue: Value = serde_json::from_str(&value_json)
                .map_err(|e| scan_issue("retained finding is not an object", &e))?;
            let Some(map) = issue.as_object().cloned() else {
                return Err(scan_issue("retained finding is not an object", &""));
            };
            Ok((ordinal, map))
        })
        .collect::<Result<_, _>>()?;
    let wanted: Vec<String> = selected
        .iter()
        .filter_map(|(_, issue)| issue["group_id"].as_str().map(str::to_owned))
        .collect();
    let sizes = group_sizes(reader, &wanted);
    Ok((
        reader.count("/issues")?,
        matched,
        selected,
        sizes,
        via_group_ids,
    ))
}

fn scan_issue(message: &str, cause: &impl std::fmt::Display) -> StoreError {
    StoreError::Invalid(format!("{message}: {cause}"))
}

/// JSON value → SQLite parameter (numbers/text/null; containers as their JSON text).
fn json_to_sql(value: &Value) -> rusqlite::types::Value {
    use rusqlite::types::Value as Sql;
    match value {
        Value::Null => Sql::Null,
        Value::Bool(flag) => Sql::Integer(*flag as i64),
        Value::Number(number) => number
            .as_i64()
            .map(Sql::Integer)
            .unwrap_or_else(|| Sql::Real(number.as_f64().unwrap_or(0.0))),
        Value::String(text) => Sql::Text(text.clone()),
        other => Sql::Text(serde_json::to_string(other).unwrap_or_default()),
    }
}

/// The bounded finding row a page returns.
fn finding_summary(
    issue: &Map<String, Value>,
    ordinal: i64,
    affected_urls: Option<&Value>,
    via_group: bool,
) -> Map<String, Value> {
    let mut result = Map::new();
    let mut truncated: Vec<String> = Vec::new();
    let mut put = |result: &mut Map<String, Value>, key: &str, value: Value| {
        if let Some(text) = value.as_str() {
            if text.chars().count() > 4096 {
                let cut: String = text.chars().take(4096).collect();
                result.insert(key.into(), json!(format!("{cut}\u{2026}")));
                truncated.push(key.to_owned());
                return;
            }
        }
        result.insert(key.into(), value);
    };
    put(&mut result, "ordinal", json!(ordinal));
    put(
        &mut result,
        "id",
        issue.get("id").cloned().unwrap_or(Value::Null),
    );
    put(
        &mut result,
        "check",
        issue.get("check").cloned().unwrap_or(Value::Null),
    );
    put(
        &mut result,
        "severity",
        issue.get("severity").cloned().unwrap_or(Value::Null),
    );
    let target = issue
        .get("target_url")
        .cloned()
        .filter(|v| !v.is_null())
        .or_else(|| issue.get("url").cloned())
        .unwrap_or(Value::Null);
    put(&mut result, "target_url", target);
    let message = issue
        .get("message")
        .cloned()
        .filter(|v| !v.is_null())
        .or_else(|| issue.get("text").cloned())
        .unwrap_or(Value::Null);
    put(&mut result, "message", message);
    put(
        &mut result,
        "fingerprint",
        issue.get("fingerprint").cloned().unwrap_or(Value::Null),
    );
    if let Some(affected) = affected_urls {
        result.insert("affected_urls".into(), affected.clone());
    }
    if via_group {
        result.insert("via_group".into(), json!(true));
    }
    if !truncated.is_empty() {
        truncated.sort();
        truncated.dedup();
        result.insert(
            "truncated_fields".into(),
            Value::Array(truncated.into_iter().map(Value::String).collect()),
        );
    }
    result
}

/// Browse one retained scan's findings with bounded filter, sort and pagination
/// (`projects.observer.findings_page`). Read-only: the scan comes from the project's
/// retained history, never a caller path.
#[allow(clippy::too_many_arguments)]
pub fn findings_page(
    directory: &str,
    scan_uuid: Option<&str>,
    offset: i64,
    limit: i64,
    query: &str,
    url: Option<&str>,
    sort: &str,
    descending: bool,
) -> Result<Map<String, Value>, StoreError> {
    if offset < 0 {
        return Err(invalid("finding offset must be non-negative"));
    }
    if !(1..=MAX_FINDING_PAGE_SIZE).contains(&limit) {
        return Err(invalid("finding limit must be from 1 to 100"));
    }
    if query.chars().count() > 256 {
        return Err(invalid(
            "finding query must be text of at most 256 characters",
        ));
    }
    if let Some(url) = url {
        if url.is_empty() || url.chars().count() > 8192 {
            return Err(invalid(
                "finding URL must be exact nonempty text of at most 8192 characters",
            ));
        }
    }
    if !FINDING_SORTS.contains(&sort) {
        return Err(invalid(
            "finding sort must be severity, check, target_url or id",
        ));
    }
    let root = load_project(directory)?;
    let row = scan_row(&root, scan_uuid)?;
    let path = Path::new(row["path"].as_str().unwrap_or_default());
    if !audit_v2_path(path).exists() {
        return Err(invalid("retained scan audit has no readable finding list"));
    }
    let reader = AuditV2Reader::new(path, true, true, None)?;
    let (source_count, matched_count, selected, sizes, via) =
        v2_findings_page(&reader, offset, limit, query, url, sort, descending)?;
    reader.close();
    let items: Vec<Value> = selected
        .iter()
        .map(|(ordinal, issue)| {
            let group_id = issue["group_id"].as_str();
            let affected = group_id.and_then(|id| sizes.get(id));
            let via_flag = group_id.is_some_and(|id| via.iter().any(|v| v == id));
            Value::Object(finding_summary(issue, *ordinal, affected, via_flag))
        })
        .collect();
    let returned = items.len() as i64;
    let next = if offset + returned < matched_count {
        Value::from(offset + returned)
    } else {
        Value::Null
    };
    Ok(json!({
        "ok": true,
        "scan": {
            "uuid": row.get("uuid").cloned().unwrap_or(Value::Null),
            "source_kind": row.get("source_kind").cloned().unwrap_or(Value::Null),
            "lifecycle": row.get("lifecycle").cloned().unwrap_or(Value::Null),
            "finish_reason": row.get("finish_reason").cloned().unwrap_or(Value::Null),
            "path": row.get("path").cloned().unwrap_or(Value::Null),
        },
        "query": query,
        "sort": {"field": sort, "direction": if descending { "desc" } else { "asc" }},
        "counts": {"source": source_count, "matched": matched_count, "returned": returned},
        "pagination": {
            "offset": offset,
            "limit": limit,
            "next_offset": next,
            "previous_offset": if offset > 0 { Value::from(offset.saturating_sub(limit)) } else { Value::Null },
            "truncated": offset + returned < matched_count,
        },
        "items": items,
    })
    .as_object()
    .cloned()
    .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_scans_directory_is_named() {
        let base = std::env::temp_dir().join(format!("sh-finding-page-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let error = load_project(base.to_str().unwrap())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("project scans directory is missing or unsafe"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn page_bounds_reject_bad_offset_and_sort() {
        let error = findings_page("x", None, -1, 50, "", None, "severity", false)
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "invalid artifact: finding offset must be non-negative"
        );
        let error = findings_page("x", None, 0, 50, "", None, "bogus", false)
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "invalid artifact: finding sort must be severity, check, target_url or id"
        );
    }
}

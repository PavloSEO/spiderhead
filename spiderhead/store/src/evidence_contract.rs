//! Additive, body-free evidence references for saved audit documents.
//!
//! The audit JSON stays at `2.0` on purpose.  This module adds an optional contract around
//! it rather than changing the historical wire format: new native scans can name the saved SQLite
//! `audit` observation that supports each finding, while export-only and older documents
//! explicitly say that such a reference was not retained.  No helper here opens a page body or
//! contacts a target.
//!
//! Documents are `serde_json::Map` values (insertion ordered), the scan handle is a borrowed
//! `rusqlite::Connection`.

use crate::corpus_derivations::{self, CorpusSources};
use crate::error::StoreError;
use regex::Regex;
use rusqlite::types::Value as SqlValue;
use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use spiderhead_formats::check_registry::{self, CheckRegistry};
use spiderhead_formats::issue_map;
use spiderhead_formats::models::Rows;
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, OnceLock};

/// A JSON object with insertion order preserved.
pub type Obj = Map<String, Value>;

pub const CONTRACT_VERSION: &str = "audit_evidence_contract.v1";

pub const SAVED_CORPUS_VERSION: &str = "saved_corpus_derivations.v2";

pub const AUDIT_SCHEMA_VERSION: &str = "2.0";

/// Regex source.
pub const ISSUE_ID: &str = "ISSUE-[0-9]{6}";

/// Regex source.
pub const TABLE: &str = "[a-z][a-z0-9_]{0,63}";

fn fullmatch(source: &'static str, cell: &'static OnceLock<Regex>, text: &str) -> bool {
    cell.get_or_init(|| Regex::new(&format!("^(?:{source})$")).unwrap_or_else(|_| unreachable_re()))
        .is_match(text)
}

/// The two literal patterns above are valid; this only exists to avoid an `unwrap`.
fn unreachable_re() -> Regex {
    Regex::new("^$").unwrap_or_else(|_| std::process::abort())
}

fn issue_id_ok(text: &str) -> bool {
    static CELL: OnceLock<Regex> = OnceLock::new();
    fullmatch(ISSUE_ID, &CELL, text)
}

fn table_ok(text: &str) -> bool {
    static CELL: OnceLock<Regex> = OnceLock::new();
    fullmatch(TABLE, &CELL, text)
}

fn registry() -> Result<&'static CheckRegistry, StoreError> {
    check_registry::builtin().map_err(|error| StoreError::Invalid(error.to_string()))
}

fn bad(message: &str) -> StoreError {
    StoreError::Invalid(message.to_string())
}

/// Parse a UUID and return it lower-case hyphenated, or `None` when invalid.
pub(crate) fn parse_uuid(text: &str) -> Option<String> {
    let cleaned = text.replace("urn:", "").replace("uuid:", "");
    let cleaned = cleaned
        .trim_matches(|c| c == '{' || c == '}')
        .replace('-', "");
    if cleaned.len() != 32 || !cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    uuid::Uuid::parse_str(&cleaned)
        .ok()
        .map(|value| value.hyphenated().to_string())
}

/// JSON string encoding, with non-ASCII escaped as `\uXXXX`.
fn ascii_json_string(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(ch),
            _ => {
                let mut units = [0u16; 2];
                for unit in ch.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
}

/// Return an opaque stable identifier for one retained typed observation.
///
/// `source_table` and `observation_id` remain separate fields in the contract for a human or
/// reader to resolve.  The compact hash avoids turning arbitrary source identifiers into a
/// report-facing path or payload.
pub fn stable_evidence_id(
    scan_uuid: &str,
    source_table: &str,
    observation_id: &str,
) -> Result<String, StoreError> {
    let normalized = parse_uuid(scan_uuid)
        .ok_or_else(|| bad("evidence reference requires a UUID scan identity"))?;
    if !table_ok(source_table) {
        return Err(bad("evidence reference has an invalid typed table"));
    }
    let length = observation_id.chars().count();
    if length == 0 || length > 256 {
        return Err(bad(
            "evidence reference has an invalid observation identity",
        ));
    }
    let mut text = String::from("[");
    for (index, part) in [CONTRACT_VERSION, &normalized, source_table, observation_id]
        .iter()
        .enumerate()
    {
        if index > 0 {
            text.push(',');
        }
        ascii_json_string(part, &mut text);
    }
    text.push(']');
    let digest = hex_lower(&Sha256::digest(text.as_bytes()));
    Ok(format!(
        "evidence:v1:{normalized}:{source_table}:{}",
        &digest[..24]
    ))
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn scan_uuid(value: &Value) -> Option<String> {
    value.as_str().and_then(parse_uuid)
}

fn as_obj(value: Option<&Value>) -> Option<&Obj> {
    value.and_then(Value::as_object)
}

/// Truthiness of a JSON value.
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|n| n != 0.0),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(map)) => !map.is_empty(),
    }
}

fn string(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str)
}

/// Return only closed skip/disable records from an audit document, as `(id, reason)` pairs.
pub fn items(value: &Value) -> Result<Vec<(String, String)>, StoreError> {
    let registry = registry()?;
    let Some(list) = value.as_array() else {
        return Ok(Vec::new());
    };
    Ok(list
        .iter()
        .filter_map(|item| {
            let item = item.as_object()?;
            let id = string(item.get("id"))?;
            let reason = string(item.get("reason"))?;
            registry.get(id)?;
            Some((id.to_string(), reason.to_string()))
        })
        .collect())
}

pub fn population(document: &Obj) -> Obj {
    let run = as_obj(document.get("run"));
    let summary = as_obj(document.get("summary"));
    let totals = summary.and_then(|map| as_obj(map.get("totals")));
    let urls = totals.and_then(|map| map.get("urls_crawled"));
    let urls = match urls {
        Some(Value::Number(number))
            if number.is_u64() || number.as_i64().is_some_and(|n| n >= 0) =>
        {
            urls.cloned().unwrap_or(Value::Null)
        }
        _ => Value::Null,
    };
    let representations = totals
        .and_then(|map| as_obj(map.get("pages_by_representation")))
        .cloned()
        .unwrap_or_default();
    let mut result = Obj::new();
    result.insert("urls_crawled".into(), urls);
    result.insert(
        "crawl_valid".into(),
        Value::Bool(run.and_then(|map| map.get("crawl_valid")) != Some(&Value::Bool(false))),
    );
    result.insert(
        "crawl_partial".into(),
        Value::Bool(truthy(run.and_then(|map| map.get("crawl_partial")))),
    );
    result.insert(
        "scope_reason".into(),
        Value::String(
            string(summary.and_then(|map| map.get("health_score_scope")))
                .unwrap_or("")
                .to_string(),
        ),
    );
    result.insert("representations".into(), Value::Object(representations));
    result
}

/// Return only issue-map rows that explicitly name this registry check.
pub fn issue_map_refs(check_id: &str) -> Vec<Obj> {
    issue_map::entries()
        .into_iter()
        .filter(|(_, entry)| entry.refs.contains(&check_id))
        .map(|(category, entry)| {
            let mut row = Obj::new();
            row.insert("category".into(), category.into());
            row.insert("name".into(), entry.name.into());
            row.insert("status".into(), entry.status.as_str().into());
            row
        })
        .collect()
}

fn state(state: &str, value: Value, reason: &str) -> Value {
    json!({"state": state, "value": value, "reason": reason})
}

fn evidence(kind: &str, source: &str, reason: &str) -> Value {
    json!({"state": "derived", "items": [{"kind": kind, "source": source}], "reason": reason})
}

/// Derive prerequisite labels from declared source metadata, never from check names.
pub fn prerequisites(check_id: &str, source: &str) -> Obj {
    let unknown = state(
        "unknown",
        Value::Null,
        "registry source does not declare this prerequisite",
    );
    let mut required = json!({
        "state": "unknown",
        "items": [],
        "reason": "registry source has no typed evidence declaration",
    });
    let mut population = unknown.clone();
    let mut representation = unknown;
    if source.starts_with("SF:") {
        // Source and label texts are fixed strings in the audit format.
        required = evidence("screaming_frog_export", source, "");
        population = state("derived", "Screaming Frog export rows".into(), "");
    } else if source.starts_with("inlinks:") {
        // "screaming_frog_inlinks_export" is a data-format compatibility string.
        required = evidence("screaming_frog_inlinks_export", source, "");
        population = state("derived", "inlink relation rows".into(), "");
    } else if source == "sitemap" {
        required = evidence("sitemap_observations", source, "");
        population = state("derived", "sitemap declarations".into(), "");
        representation = state(
            "not_applicable",
            Value::Null,
            "sitemap declarations have no raw/rendered page representation",
        );
    } else if source.starts_with("crawl:") {
        required = evidence("native_crawl_evidence", source, "");
        population = state("derived", "native crawl evidence records".into(), "");
    } else if source.starts_with("provider:") {
        required = evidence(
            "retained_project_provider_history",
            source,
            "provider findings require an explicitly bound local property and retained coverage",
        );
        population = state(
            "derived",
            "audited crawl URLs joined to the source's retained reporting interval".into(),
            "the exact native provider interval and coverage are attached to the finding",
        );
        representation = state(
            "not_applicable",
            Value::Null,
            "provider observations are not page-body representation checks",
        );
    }
    let mut result = Obj::new();
    result.insert("source_tag".into(), source.into());
    result.insert("required_evidence".into(), required);
    result.insert("population".into(), population);
    result.insert("representation".into(), representation);
    result.insert(
        "issue_map_refs".into(),
        Value::Array(
            issue_map_refs(check_id)
                .into_iter()
                .map(Value::Object)
                .collect(),
        ),
    );
    result
}

fn id_list(value: Option<&Value>, registry: &CheckRegistry) -> Option<Vec<String>> {
    let list = value?.as_array()?;
    Some(
        list.iter()
            .filter_map(Value::as_str)
            .filter(|id| registry.get(id).is_some())
            .map(str::to_string)
            .collect(),
    )
}

/// Describe every registered check without treating missing data as clean.
///
/// `check_coverage` and the run's named skipped/disabled declarations are the authoritative
/// completion facts.  A check that has no finding and is not listed in the silent inventory is
/// *unmeasured*, not a silent success.
pub fn capability_rows(document: &Obj) -> Result<Vec<Obj>, StoreError> {
    let registry = registry()?;
    let run = as_obj(document.get("run"));
    let summary = as_obj(document.get("summary"));
    let coverage = summary.and_then(|map| as_obj(map.get("check_coverage")));
    let empty = Vec::new();
    let issues = document
        .get("issues")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let fired: BTreeSet<&str> = issues
        .iter()
        .filter_map(|item| string(item.as_object()?.get("check")))
        .filter(|id| registry.get(id).is_some())
        .collect();
    let named = |key: &str| -> Result<HashMap<String, String>, StoreError> {
        let value = run
            .and_then(|map| map.get(key))
            .cloned()
            .unwrap_or(Value::Null);
        Ok(items(&value)?.into_iter().collect())
    };
    let skipped = named("checks_skipped")?;
    let mut disabled = named("checks_disabled")?;
    let mut silent: BTreeSet<String> = BTreeSet::new();
    if let Some(coverage) = coverage {
        if let Some(ids) = id_list(coverage.get("checks_disabled_ids"), registry) {
            for id in ids {
                disabled
                    .entry(id)
                    .or_insert_with(|| "disabled by the saved audit configuration".into());
            }
        }
        if let Some(ids) = id_list(coverage.get("checks_silent_ids"), registry) {
            silent.extend(ids);
        }
    }
    let partial = truthy(run.and_then(|map| map.get("crawl_partial")));
    let mut ids: Vec<&str> = registry
        .checks()
        .iter()
        .map(|meta| meta.id.as_str())
        .collect();
    ids.sort_unstable();
    let mut rows = Vec::with_capacity(ids.len());
    for check_id in ids {
        let source = registry
            .get(check_id)
            .map(|meta| meta.source.as_str())
            .unwrap_or("");
        let mut row = Obj::new();
        row.insert("check".into(), check_id.into());
        row.insert("source".into(), source.into());
        row.insert(
            "prerequisites".into(),
            Value::Object(prerequisites(check_id, source)),
        );
        let (state, capability, reason): (&str, &str, String) = if fired.contains(check_id) {
            ("measured", "finding_recorded", String::new())
        } else if let Some(reason) = disabled.get(check_id) {
            ("unmeasured", "disabled", reason.clone())
        } else if let Some(reason) = skipped.get(check_id) {
            ("unmeasured", "skipped", reason.clone())
        } else if silent.contains(check_id) {
            if partial {
                (
                    "partial",
                    "ran_without_finding",
                    "crawl was partial; this clean result covers only the saved scope".into(),
                )
            } else {
                ("measured", "ran_without_finding", String::new())
            }
        } else {
            (
                "unmeasured",
                "unaccounted",
                "the saved audit does not account for this check".into(),
            )
        };
        row.insert("state".into(), state.into());
        row.insert("capability".into(), capability.into());
        row.insert("reason".into(), reason.into());
        rows.push(row);
    }
    Ok(rows)
}

/// Name the saved audit issue separately from its underlying observations.
pub fn finding_projection(issue: &Obj, scan_uuid: Option<&str>) -> Result<Obj, StoreError> {
    let unavailable = |reason: &str| -> Obj {
        let mut map = Obj::new();
        map.insert("state".into(), "unavailable".into());
        map.insert("reason".into(), reason.into());
        map
    };
    let Some(scan_uuid) = scan_uuid else {
        return Ok(unavailable(
            "legacy or export-only audit has no retained scan UUID",
        ));
    };
    let Some(issue_id) = string(issue.get("id")).filter(|id| issue_id_ok(id)) else {
        return Ok(unavailable(
            "saved audit finding has no stable ISSUE identifier",
        ));
    };
    let observation_id = format!("issue:{issue_id}");
    reference(
        "measured",
        "finding_projection",
        scan_uuid,
        "audit",
        &observation_id,
        None,
    )
}

/// One evidence reference in the contract's field order.
fn reference(
    state: &str,
    role: &str,
    scan_uuid: &str,
    table: &str,
    observation_id: &str,
    representation: Option<Value>,
) -> Result<Obj, StoreError> {
    let mut map = Obj::new();
    map.insert("state".into(), state.into());
    map.insert("role".into(), role.into());
    map.insert(
        "id".into(),
        stable_evidence_id(scan_uuid, table, observation_id)?.into(),
    );
    map.insert("scan_uuid".into(), scan_uuid.into());
    map.insert("source_table".into(), table.into());
    map.insert("observation_id".into(), observation_id.into());
    if let Some(representation) = representation {
        map.insert("representation".into(), representation);
    }
    Ok(map)
}

fn sql_value(row: &rusqlite::Row<'_>, index: usize) -> SqlValue {
    row.get::<_, SqlValue>(index).unwrap_or(SqlValue::Null)
}

/// Return the retained scan source kind, or an explicit binding failure reason.
pub fn saved_scan(
    con: Option<&Connection>,
    scan_uuid: Option<&str>,
) -> Result<String, &'static str> {
    let Some(con) = con else {
        return Err("underlying saved observations were not supplied");
    };
    let Some(scan_uuid) = scan_uuid else {
        return Err("saved scan has no valid UUID binding");
    };
    let row = con.query_row(
        "SELECT scan_uuid,source_kind FROM scan WHERE singleton=1",
        [],
        |row| Ok((sql_value(row, 0), sql_value(row, 1))),
    );
    let (stored, kind) = match row {
        Ok(pair) => pair,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err("saved scan header is unavailable")
        }
        Err(_) => return Err("saved scan cannot resolve retained observations"),
    };
    let stored = match stored {
        SqlValue::Text(text) => parse_uuid(&text),
        _ => None,
    };
    if stored.as_deref() != Some(scan_uuid) {
        return Err("saved scan identity does not match the audit binding");
    }
    match kind {
        SqlValue::Text(kind)
            if matches!(kind.as_str(), "native" | "reanalysis" | "legacy_import") =>
        {
            Ok(kind)
        }
        _ => Err("saved scan source kind is unsupported"),
    }
}

fn as_positive_int(value: &SqlValue) -> Option<i64> {
    match value {
        SqlValue::Integer(number) if *number > 0 => Some(*number),
        _ => None,
    }
}

fn text_or_empty(value: &SqlValue) -> Value {
    match value {
        SqlValue::Text(text) => Value::String(text.clone()),
        _ => Value::String(String::new()),
    }
}

/// Resolve one audit URL to actual page/document/response records only.
pub fn stored_page_observations(
    con: &Connection,
    scan_uuid: &str,
    url: &str,
    role: &str,
    source_kind: &str,
) -> Result<Vec<Obj>, StoreError> {
    let row = con.query_row(
        "SELECT p.url_id,p.document_id,p.representation,d.source_response_id,r.response_id \
         FROM urls u JOIN pages p ON p.url_id=u.url_id \
         LEFT JOIN documents d ON d.document_id=p.document_id \
         LEFT JOIN responses r ON r.response_id=d.source_response_id WHERE u.url=?1",
        params![url],
        |row| {
            Ok([
                sql_value(row, 0),
                sql_value(row, 1),
                sql_value(row, 2),
                sql_value(row, 3),
                sql_value(row, 4),
            ])
        },
    );
    let Ok([url_id, document_id, representation, source_response_id, response_id]) = row else {
        return Ok(Vec::new());
    };
    let Some(url_id) = as_positive_int(&url_id) else {
        return Ok(Vec::new());
    };
    let state = if source_kind == "legacy_import" {
        "imported_projection"
    } else {
        "measured"
    };
    let representation = text_or_empty(&representation);
    let make = |table: &str, id: String, representation: Value| {
        reference(state, role, scan_uuid, table, &id, Some(representation))
    };
    let mut output = vec![
        make("pages", format!("url_id:{url_id}"), representation.clone())?,
        make(
            "urls",
            format!("url_id:{url_id}"),
            Value::String(String::new()),
        )?,
    ];
    if let Some(document_id) = as_positive_int(&document_id) {
        output.push(make(
            "documents",
            format!("document_id:{document_id}"),
            representation.clone(),
        )?);
    }
    if let Some(source) = as_positive_int(&source_response_id) {
        if matches!(response_id, SqlValue::Integer(found) if found == source) {
            output.push(make(
                "responses",
                format!("response_id:{source}"),
                representation,
            )?);
        }
    }
    Ok(output)
}

/// Return saved hreflang declaration observations for already-resolved documents.
pub fn language_observations(
    con: &Connection,
    scan_uuid: &str,
    observations: &[Obj],
    source_kind: &str,
) -> Result<Vec<Obj>, StoreError> {
    if source_kind == "legacy_import" {
        return Ok(Vec::new());
    }
    // Fetch every language_evidence item once (ordered by item_key, so the first entry per
    // document matches the old per-document `ORDER BY item_key LIMIT 1` lookup).
    let mut evidence_rows = con.prepare(
        "SELECT item_key,payload_json FROM context_items WHERE kind='language_evidence' \
         ORDER BY item_key",
    )?;
    let mut evidence: std::collections::BTreeMap<i64, (String, String)> =
        std::collections::BTreeMap::new();
    {
        let mut rows = evidence_rows.query([])?;
        while let Some(row) = rows.next()? {
            let (SqlValue::Text(item_key), SqlValue::Text(payload_text)) =
                (sql_value(row, 0), sql_value(row, 1))
            else {
                continue;
            };
            let Some(document_id) = item_key
                .split(":document:")
                .nth(1)
                .and_then(|tail| tail.split(":representation:").next())
                .and_then(|digits| digits.trim().parse::<i64>().ok())
            else {
                continue;
            };
            evidence
                .entry(document_id)
                .or_insert((item_key, payload_text));
        }
    }
    let mut output = Vec::new();
    for document in observations.iter().filter(|item| {
        string(item.get("source_table")) == Some("documents")
            && string(item.get("role")) == Some("target")
    }) {
        let Some(observation_id) = string(document.get("observation_id")) else {
            continue;
        };
        let Some(tail) = observation_id.strip_prefix("document_id:") else {
            continue;
        };
        let Ok(document_id) = tail.trim().parse::<i64>() else {
            continue;
        };
        let representation = match document.get("representation") {
            Some(value @ Value::String(_)) => value.clone(),
            _ => Value::String(String::new()),
        };
        let Some((item_key, payload_text)) = evidence.get(&document_id) else {
            continue;
        };
        let Ok(payload) = serde_json::from_str::<Value>(payload_text) else {
            continue;
        };
        let declarations: Vec<Value> = match payload.as_object() {
            Some(map) => match map.get("declarations") {
                Some(Value::Array(list)) => list.clone(),
                Some(_) => continue,
                None => Vec::new(),
            },
            None => Vec::new(),
        };
        if declarations.is_empty() {
            let state = string(payload.get("state")).unwrap_or("unknown");
            let id = format!("language_evidence:{item_key}:state:{state}");
            output.push(reference(
                "measured",
                "declaration",
                scan_uuid,
                "context_items",
                &id,
                Some(representation.clone()),
            )?);
        }
        for declaration in declarations.iter().take(100) {
            let Some(ordinal) = declaration
                .as_object()
                .and_then(|map| map.get("ordinal"))
                .and_then(|value| {
                    value
                        .as_i64()
                        .map(|n| n.to_string())
                        .or_else(|| value.as_u64().map(|n| n.to_string()))
                })
            else {
                continue;
            };
            let id = format!("language_evidence:{item_key}:ordinal:{ordinal}");
            output.push(reference(
                "measured",
                "declaration",
                scan_uuid,
                "context_items",
                &id,
                Some(representation.clone()),
            )?);
        }
    }
    Ok(output)
}

/// Reference actual retained graph rows, not a source page's response.
pub fn image_resource_observations(
    con: &Connection,
    issue: &Obj,
    scan_uuid: &str,
) -> Result<Vec<Obj>, StoreError> {
    let check = match issue.get("check") {
        Some(Value::String(text)) => text.clone(),
        None => String::new(),
        Some(other) => other.to_string(),
    };
    if !check.starts_with("IMG_") {
        return Ok(Vec::new());
    }
    let empty = Obj::new();
    let details = as_obj(issue.get("details")).unwrap_or(&empty);
    let resource = if details.contains_key("resource_url") {
        details.get("resource_url")
    } else {
        issue.get("target_url")
    };
    let Some(resource) = string(resource).filter(|text| !text.is_empty()) else {
        return Ok(Vec::new());
    };
    let mut output = Vec::new();
    // Older artifacts may have no graph or declaration binding columns; any failure ends the
    // lookup and keeps what was already found.
    let _ = image_lookup(con, details, resource, scan_uuid, &mut output);
    Ok(output)
}

fn image_lookup(
    con: &Connection,
    details: &Obj,
    resource: &str,
    scan_uuid: &str,
    output: &mut Vec<Obj>,
) -> Result<(), StoreError> {
    let fetch = con.query_row(
        "SELECT status_code,bytes_received FROM resource_graph_fetches WHERE resolved_url=?1",
        params![resource],
        |row| Ok((sql_value(row, 0), sql_value(row, 1))),
    );
    match fetch {
        Ok((status, bytes)) => {
            let found = if !matches!(status, SqlValue::Null) {
                true
            } else {
                match bytes {
                    SqlValue::Integer(count) => count > 0,
                    // An absent count cannot be compared, so the lookup stops here.
                    _ => return Err(bad("bytes_received is not comparable")),
                }
            };
            if found {
                let key = format!(
                    "url_sha256:{}",
                    hex_lower(&Sha256::digest(resource.as_bytes()))
                );
                output.push(reference(
                    "measured",
                    "target_resource",
                    scan_uuid,
                    "resource_graph_fetches",
                    &key,
                    Some(Value::String(String::new())),
                )?);
            }
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => {}
        Err(error) => return Err(error.into()),
    }
    let Some(source) = string(details.get("source_page")).filter(|text| !text.is_empty()) else {
        return Ok(());
    };
    let document_id = details.get("document_id").and_then(Value::as_i64);
    let mut sql = String::from(
        "SELECT o.occurrence_id,o.representation FROM resource_graph_occurrences o \
         JOIN urls u ON u.url_id=o.page_url_id WHERE o.resolved_url=?1 AND u.url=?2",
    );
    if document_id.is_some() {
        sql.push_str(" AND o.source_document_id=?3");
    }
    sql.push_str(" ORDER BY o.occurrence_id LIMIT 1");
    let map = |row: &rusqlite::Row<'_>| Ok((sql_value(row, 0), sql_value(row, 1)));
    let found = match document_id {
        Some(id) => con.query_row(&sql, params![resource, source, id], map),
        None => con.query_row(&sql, params![resource, source], map),
    };
    match found {
        Ok((occurrence, representation)) => {
            let occurrence = match occurrence {
                SqlValue::Integer(number) => number.to_string(),
                _ => "None".to_string(),
            };
            let representation = match representation {
                SqlValue::Text(text) => Value::String(text),
                _ => Value::Null,
            };
            output.push(reference(
                "measured",
                "source_declaration",
                scan_uuid,
                "resource_graph_occurrences",
                &format!("occurrence_id:{occurrence}"),
                Some(representation),
            )?);
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// Resolve target/source/declaration rows without making a network request.
pub fn underlying_references(
    issue: &Obj,
    con: Option<&Connection>,
    scan_uuid: Option<&str>,
) -> Result<(Vec<Obj>, String), StoreError> {
    let source_kind = match saved_scan(con, scan_uuid) {
        Ok(kind) => kind,
        Err(reason) => return Ok((Vec::new(), reason.to_string())),
    };
    let (Some(con), Some(scan_uuid)) = (con, scan_uuid) else {
        return Ok((Vec::new(), "saved scan has no valid UUID binding".into()));
    };
    let mut observations = image_resource_observations(con, issue, scan_uuid)?;
    if let Some(target) = string(issue.get("target_url")).filter(|text| !text.is_empty()) {
        observations.extend(stored_page_observations(
            con,
            scan_uuid,
            target,
            "target",
            &source_kind,
        )?);
    }
    let mut sources: BTreeSet<&str> = BTreeSet::new();
    if let Some(page) = as_obj(issue.get("details"))
        .and_then(|details| string(details.get("source_page")))
        .filter(|text| !text.is_empty())
    {
        sources.insert(page);
    }
    if let Some(Value::Array(locations)) = issue.get("locations") {
        for location in locations.iter().take(100) {
            if let Some(url) = location
                .as_object()
                .and_then(|map| string(map.get("source_url")))
                .filter(|text| !text.is_empty())
            {
                sources.insert(url);
            }
        }
    }
    for source in sources {
        observations.extend(stored_page_observations(
            con,
            scan_uuid,
            source,
            "source",
            &source_kind,
        )?);
    }
    if string(issue.get("check")).is_some_and(|check| check.starts_with("HREFLANG_")) {
        let languages = language_observations(con, scan_uuid, &observations, &source_kind)?;
        observations.extend(languages);
    }
    if !observations.is_empty() {
        return Ok((observations, String::new()));
    }
    if source_kind == "legacy_import" {
        return Ok((
            Vec::new(),
            "legacy import has no captured page/document/response observations".into(),
        ));
    }
    Ok((
        Vec::new(),
        "no retained page/document/response observation matches this finding".into(),
    ))
}

/// Return underlying observations first; the audit ordinal stays a projection.
pub fn issue_reference(
    issue: &Obj,
    con: Option<&Connection>,
    scan_uuid: Option<&str>,
) -> Result<Obj, StoreError> {
    let finding = finding_projection(issue, scan_uuid)?;
    let (observations, reason) = underlying_references(issue, con, scan_uuid)?;
    let Some(first) = observations.first() else {
        let mut map = Obj::new();
        map.insert("state".into(), "unavailable".into());
        map.insert("reason".into(), reason.into());
        map.insert("finding".into(), Value::Object(finding));
        map.insert("observations".into(), Value::Array(Vec::new()));
        return Ok(map);
    };
    let mut primary = first.clone();
    primary.insert("finding".into(), Value::Object(finding));
    primary.insert(
        "observations".into(),
        Value::Array(observations.into_iter().map(Value::Object).collect()),
    );
    Ok(primary)
}

fn require_schema(document: &Obj, message: &str) -> Result<(), StoreError> {
    if string(document.get("schema_version")) != Some(AUDIT_SCHEMA_VERSION) {
        return Err(bad(message));
    }
    Ok(())
}

/// Make sure `key` holds an object in `document` and return a clone of it for editing.
fn object_slot(document: &mut Obj, key: &str) -> Obj {
    let current = as_obj(document.get(key)).cloned().unwrap_or_default();
    document.insert(key.into(), Value::Object(current.clone()));
    current
}

fn contract_summary(projected: &Obj, identity: &Option<String>, rows: Vec<Obj>) -> Value {
    json!({
        "schema_version": CONTRACT_VERSION,
        "audit_schema_version": projected.get("schema_version").cloned().unwrap_or(Value::Null),
        "scan_uuid": identity,
        "scan_identity_state": if identity.is_some() { "measured" } else { "unavailable" },
        "scan_identity_reason": if identity.is_some() { "" } else { "no retained scan UUID in this audit document" },
        "population": Value::Object(population(projected)),
        "capability_rows": rows.into_iter().map(Value::Object).collect::<Vec<_>>(),
    })
}

fn identity_of(scan_uuid: Option<&str>, run: &Obj) -> Option<String> {
    scan_uuid
        .and_then(parse_uuid)
        .or_else(|| run.get("scan_uuid").and_then(scan_uuid_value))
}

fn scan_uuid_value(value: &Value) -> Option<String> {
    scan_uuid(value)
}

/// Return a copied audit document with additive capability and evidence IDs.
///
/// The function deliberately keeps `schema_version` at `2.0`; callers can use it before
/// reports/tasks are built without breaking existing audit JSON readers.  A UUID supplied by a
/// scan header wins over any run field so an integration can bind an exported document to its
/// retained scan explicitly.  `con` is an already-open scan connection; when supplied,
/// references bind to stored pages/documents/responses and (where present) language contexts.
pub fn attach_contract(
    document: &Obj,
    scan_uuid: Option<&str>,
    con: Option<&Connection>,
) -> Result<Obj, StoreError> {
    let mut projected = document.clone();
    require_schema(
        &projected,
        "evidence contract supports only audit.json schema_version 2.0",
    )?;
    let run = object_slot(&mut projected, "run");
    let mut summary = object_slot(&mut projected, "summary");
    let identity = identity_of(scan_uuid, &run);
    let rows = capability_rows(&projected)?;
    summary.insert(
        "evidence_contract".into(),
        contract_summary(&projected, &identity, rows),
    );
    projected.insert("summary".into(), Value::Object(summary));
    if let Some(Value::Array(issues)) = projected.get_mut("issues") {
        for issue in issues.iter_mut() {
            let Some(issue) = issue.as_object_mut() else {
                continue;
            };
            let mut evidence = as_obj(issue.get("evidence")).cloned().unwrap_or_default();
            let contract = issue_reference(issue, con, identity.as_deref())?;
            evidence.insert("contract".into(), Value::Object(contract));
            issue.insert("evidence".into(), Value::Object(evidence));
        }
    }
    Ok(projected)
}

/// Attach contract metadata while preserving a re-iterable issue collection.
///
/// The returned rows are produced lazily, one issue per step.  A reference that cannot be
/// built is reported as an unavailable reference carrying the
/// error text, so one bad row never aborts the stream.
pub fn attach_contract_parts<'a, 'b: 'a>(
    header: &Obj,
    issues: &'a Rows<'b>,
    scan_uuid: Option<&str>,
    con: Option<&'a Connection>,
) -> Result<(Obj, Rows<'a>), StoreError> {
    let mut projected = header.clone();
    require_schema(
        &projected,
        "evidence contract supports only audit.json schema_version 2.0",
    )?;
    let run = object_slot(&mut projected, "run");
    let mut summary = object_slot(&mut projected, "summary");
    let identity = identity_of(scan_uuid, &run);
    let fired: BTreeSet<String> = issues
        .iter()
        .filter_map(|item| string(item.as_object()?.get("check")).map(str::to_string))
        .collect();
    let mut shell = projected.clone();
    shell.insert(
        "issues".into(),
        Value::Array(
            fired
                .iter()
                .map(|check| json!({ "check": check }))
                .collect(),
        ),
    );
    let rows = capability_rows(&shell)?;
    summary.insert(
        "evidence_contract".into(),
        contract_summary(&projected, &identity, rows),
    );
    projected.insert("summary".into(), Value::Object(summary));
    let stream = Rows::new(move || {
        let identity = identity.clone();
        Box::new(issues.iter().map(move |issue| {
            let Value::Object(mut item) = issue else {
                return issue;
            };
            let mut evidence = match item.get("evidence") {
                Some(Value::Object(map)) => map.clone(),
                _ => Obj::new(),
            };
            let contract =
                issue_reference(&item, con, identity.as_deref()).unwrap_or_else(|error| {
                    let mut map = Obj::new();
                    map.insert("state".into(), "unavailable".into());
                    map.insert("reason".into(), error.to_string().into());
                    map.insert("finding".into(), Value::Null);
                    map.insert("observations".into(), Value::Array(Vec::new()));
                    map
                });
            evidence.insert("contract".into(), Value::Object(contract));
            item.insert("evidence".into(), Value::Object(evidence));
            Value::Object(item)
        }))
    });
    Ok((projected, stream))
}

/// Attach already-stored corpus derivations at the audit boundary.
///
/// `con` is the caller's already-open scan connection.  The only consumer is
/// [`corpus_derivations::derive`], which reads typed context items and does not fetch, render,
/// or retain bodies.  This is intentionally a separate pure hook so legacy and export-only
/// callers stay explicit.
pub fn attach_saved_corpus(
    document: &Obj,
    con: &Connection,
    duplicate_threshold: f64,
    derived: Option<&Obj>,
    sources: &dyn CorpusSources,
) -> Result<Obj, StoreError> {
    let mut projected = document.clone();
    require_schema(
        &projected,
        "saved corpus attachment supports only audit.json schema_version 2.0",
    )?;
    let mut summary = object_slot(&mut projected, "summary");
    let derived = match derived {
        Some(value) => value.clone(),
        None => corpus_derivations::derive_with(con, duplicate_threshold, false, sources)?,
    };
    if string(derived.get("schema_version")) != Some(SAVED_CORPUS_VERSION) {
        return Err(bad(
            "saved corpus derivation has an unsupported schema version",
        ));
    }
    summary.insert("saved_corpus_derivations".into(), Value::Object(derived));
    projected.insert("summary".into(), Value::Object(summary));
    Ok(projected)
}

/// Sections whose row lists are lifted out of the header, with the lists' names.
const LIFTED: [(&str, &[&str]); 3] = [
    (
        "duplicates",
        &["exact_groups", "near_witnesses", "excluded"],
    ),
    ("structured", &["items"]),
    ("internationalization", &["items", "declarations"]),
];

/// Attach corpus derivations to an audit header without reading collections.
///
/// When `collections` is given, each lifted row list is registered there under its JSON pointer
/// as a re-iterable [`Rows`] and replaced by `[]` in the header.
pub fn attach_saved_corpus_header<'a>(
    header: &Obj,
    con: &Connection,
    derived: Option<&Obj>,
    collections: Option<&mut Vec<(String, Rows<'a>)>>,
    sources: &dyn CorpusSources,
) -> Result<Obj, StoreError> {
    let mut projected = header.clone();
    require_schema(
        &projected,
        "saved corpus attachment supports only audit.json schema_version 2.0",
    )?;
    let mut summary = object_slot(&mut projected, "summary");
    let mut payload = match derived {
        Some(value) => value.clone(),
        None => corpus_derivations::derive_with(con, 0.92, false, sources)?,
    };
    if string(payload.get("schema_version")) != Some(SAVED_CORPUS_VERSION) {
        return Err(bad(
            "saved corpus derivation has an unsupported schema version",
        ));
    }
    if let Some(collections) = collections {
        for (section, names) in LIFTED {
            let mut block = as_obj(payload.get(section))
                .cloned()
                .ok_or_else(|| bad(&format!("saved corpus derivation has no {section} block")))?;
            for name in names {
                let rows = match block.get(*name) {
                    Some(Value::Array(list)) => list.clone(),
                    _ => return Err(bad(&format!("saved corpus {section} has no {name} list"))),
                };
                let pointer = format!("/summary/saved_corpus_derivations/{section}/{name}");
                let rows = Arc::new(rows);
                collections.push((
                    pointer,
                    Rows::new(move || {
                        let rows = Arc::clone(&rows);
                        Box::new((0..rows.len()).map(move |index| rows[index].clone()))
                    }),
                ));
                block.insert((*name).into(), Value::Array(Vec::new()));
            }
            payload.insert(section.into(), Value::Object(block));
        }
    }
    summary.insert("saved_corpus_derivations".into(), Value::Object(payload));
    projected.insert("summary".into(), Value::Object(summary));
    Ok(projected)
}

/// One comparison side for `basis`, or `None` when the document does not declare it.
fn side(document: &Obj, key: &str) -> Option<Value> {
    let run = as_obj(document.get("run"));
    let summary = as_obj(document.get("summary"));
    let totals = summary.and_then(|map| as_obj(map.get("totals")));
    let contract = summary.and_then(|map| as_obj(map.get("evidence_contract")));
    let corpus = summary.and_then(|map| map.get("saved_corpus_derivations"));
    let run_get = |name: &str| -> Value {
        run.and_then(|map| map.get(name))
            .cloned()
            .unwrap_or(Value::Null)
    };
    match key {
        "scope" => {
            let run = run?;
            if !run.contains_key("crawl_valid") || !run.contains_key("crawl_partial") {
                return None;
            }
            Some(json!({
                "crawl_valid": run_get("crawl_valid"),
                "crawl_partial": run_get("crawl_partial"),
                "scope": summary.and_then(|map| map.get("health_score_scope")).cloned().unwrap_or(Value::Null),
            }))
        }
        "configuration" => {
            run.and_then(|map| as_obj(map.get("crawl_config")))?;
            Some(json!({
                "crawl_config": run_get("crawl_config"),
                "profile": run_get("profile"),
                "config_fingerprint": run_get("config_fingerprint"),
            }))
        }
        "representation" => totals
            .and_then(|map| as_obj(map.get("pages_by_representation")))
            .map(|map| Value::Object(map.clone())),
        "corpus" => {
            let corpus = corpus.and_then(Value::as_object)?;
            Some(json!({
                "corpus_partial": run_get("corpus_partial"),
                "schema_version": corpus.get("schema_version").cloned().unwrap_or(Value::Null),
                "scan_identity": contract.and_then(|map| map.get("scan_identity_state")).cloned().unwrap_or(Value::Null),
            }))
        }
        _ => {
            if !run.is_some_and(|map| map.get("input_mode").is_some_and(Value::is_string)) {
                return None;
            }
            Some(json!({
                "input_mode": run_get("input_mode"),
                "collector": run_get("collector"),
                "provider": run_get("provider"),
                "source_kind": run_get("source_kind"),
            }))
        }
    }
}

/// A basis dictionary whose values are all absent says nothing.
fn all_absent(value: &Option<Value>) -> bool {
    value
        .as_ref()
        .and_then(Value::as_object)
        .is_some_and(|map| !map.values().any(|item| !item.is_null()))
}

/// Return named comparison bases without inferring equality from absence.
pub fn comparison_compatibility(before: &Obj, after: &Obj) -> Vec<Obj> {
    let mut result = Vec::new();
    for key in [
        "scope",
        "configuration",
        "representation",
        "corpus",
        "provider",
    ] {
        let (left, right) = (side(before, key), side(after, key));
        let mut unknown =
            left.is_none() || right.is_none() || all_absent(&left) || all_absent(&right);
        if key == "corpus" {
            let measured = |value: &Option<Value>| {
                value
                    .as_ref()
                    .and_then(Value::as_object)
                    .is_some_and(|map| map.get("corpus_partial").is_some_and(Value::is_boolean))
            };
            if !measured(&left) || !measured(&right) {
                unknown = true;
            }
        }
        let state = if unknown {
            "unknown"
        } else if left == right {
            "compatible"
        } else {
            "incompatible"
        };
        let mut row = Obj::new();
        row.insert("basis".into(), key.into());
        row.insert("state".into(), state.into());
        row.insert("before".into(), left.unwrap_or(Value::Null));
        row.insert("after".into(), right.unwrap_or(Value::Null));
        result.push(row);
    }
    result
}

/// Render comparison bases as concise warnings for a caller's preflight.
pub fn comparison_warnings(before: &Obj, after: &Obj) -> Vec<String> {
    let mut warnings = Vec::new();
    for row in comparison_compatibility(before, after) {
        let basis = string(row.get("basis")).unwrap_or("");
        if basis == "corpus" {
            for name in ["before", "after"] {
                let partial = as_obj(row.get(name)).and_then(|map| map.get("corpus_partial"))
                    == Some(&Value::Bool(true));
                if partial {
                    warnings.push(format!(
                        "{name} saved corpus is partial; missing evidence is not a verified fix"
                    ));
                }
            }
        }
        match string(row.get("state")) {
            Some("compatible") => {}
            Some("unknown") => warnings.push(format!(
                "comparison {basis} basis is unavailable; comparability is unknown"
            )),
            _ => warnings.push(format!(
                "comparison {basis} basis differs between saved audits"
            )),
        }
    }
    warnings
}

#[cfg(test)]
#[path = "evidence_contract_tests.rs"]
mod tests;

//! Versioned normalized-evidence join, and the compatibility decision over two normalized
//! documents.

use indexmap::IndexMap;
use serde_json::{json, Map, Value};

use crate::cloudflare::CfError;
use crate::external_join::normalize_join_key;
use crate::pystr;
use crate::pytime;

mod policy;
use policy::*;
mod compatibility;
pub use compatibility::*;

/// Format identifier of a join result document.
pub const JOIN_FORMAT: &str = "spiderhead.evidence-join.v1";
/// Format identifier of a compatibility report.
pub const COMPATIBILITY_FORMAT: &str = "spiderhead.evidence-compatibility.v1";
/// Normalized document format; must match the import module.
pub const NORMALIZED_FORMAT: &str = "spiderhead.normalized-evidence.v1";

const URL_POLICY_KEYS: &[&str] = &["ignore_query", "ignore_scheme", "casefold_path"];
const BOUNDARY_POLICIES: &[&str] = &["strict", "local_calendar"];
const POLICY_KEYS: &[&str] = &["boundary_policy", "cross_source", "quadrant"];
const PRIVATE_AXES: &[&str] = &["reporting_identity", "site_origin"];

fn import_error(message: impl Into<String>) -> CfError {
    CfError::Python("EvidenceImportError", message.into())
}

/// Field lookup; a non-object value is an error.
fn get(value: &Value, name: &str) -> Result<Option<Value>, CfError> {
    if value.is_object() {
        Ok(value.get(name).cloned())
    } else {
        Err(crate::cloudflare::attr_get_error(value))
    }
}

/// Join-key function for a URL policy: non-text URLs cannot be keyed.
/// Also used by `evidence_join_store`, with the same policy-keyed closure.
pub(crate) fn key_fn_for_store(
    policy: &Map<String, Value>,
) -> impl Fn(Option<&Value>) -> Option<String> + '_ {
    key_fn(policy)
}

fn key_fn(policy: &Map<String, Value>) -> impl Fn(Option<&Value>) -> Option<String> + '_ {
    move |value| {
        let text = value.and_then(Value::as_str)?;
        normalize_join_key(
            Some(&json!(text)),
            policy["ignore_query"].as_bool().unwrap_or(false),
            policy["ignore_scheme"].as_bool().unwrap_or(false),
            policy["casefold_path"].as_bool().unwrap_or(false),
        )
        .ok()
        .flatten()
    }
}

pub(crate) fn row_join_key(
    row: &Value,
    key_fn: &dyn Fn(Option<&Value>) -> Option<String>,
) -> Result<Option<String>, CfError> {
    let url = get(row, "url")?
        .filter(|v| pystr::truthy(Some(v)))
        .unwrap_or(json!({}));
    if get(&url, "state")? == Some(json!("unkeyable")) {
        return Ok(None);
    }
    let candidate = get(&url, "resolved")?
        .filter(|v| pystr::truthy(Some(v)))
        .or(get(&url, "raw")?);
    Ok(key_fn(candidate.as_ref()))
}

pub(crate) fn evidence_header_pub(document: &Value) -> Result<Value, CfError> {
    evidence_header(document)
}

fn evidence_header(document: &Value) -> Result<Value, CfError> {
    let mapping = get(document, "mapping")?
        .filter(|v| pystr::truthy(Some(v)))
        .unwrap_or(json!({}));
    let source = get(&mapping, "source")?
        .filter(|v| pystr::truthy(Some(v)))
        .unwrap_or(json!({}));
    let provenance = get(document, "provenance")?
        .filter(|v| pystr::truthy(Some(v)))
        .unwrap_or(json!({}));
    let fields = get(&provenance, "fields")?
        .filter(|v| pystr::truthy(Some(v)))
        .unwrap_or(json!({}));
    let mut field_origins = Map::new();
    if let Value::Object(entries) = &fields {
        for (name, entry) in entries {
            field_origins.insert(name.clone(), get(entry, "origin")?.unwrap_or(Value::Null));
        }
    }
    Ok(json!({
        "provider": get(&source, "provider")?,
        "operation": get(&source, "operation")?,
        "reporting_identity": get(&source, "reporting_identity")?,
        "privacy": get(&source, "privacy")?,
        "search_engine": get(&source, "search_engine")?,
        "search_type": get(&source, "search_type")?,
        "attribution": get(&source, "attribution")?,
        "site_origin": get(&source, "site_origin")?,
        "period": get(&mapping, "period")?,
        "timezone": get(&source, "timezone")?,
        "dimensions": get(&mapping, "dimensions")?.filter(|v| pystr::truthy(Some(v))).unwrap_or(json!([])),
        "metrics": get(&mapping, "metrics")?.filter(|v| pystr::truthy(Some(v))).unwrap_or(json!([])),
        "collection": get(&mapping, "collection")?.filter(|v| pystr::truthy(Some(v))).unwrap_or(json!({})),
        "file": get(&provenance, "file")?,
        "artifact_reference": get(&provenance, "artifact_reference")?,
        "field_origins": field_origins,
    }))
}

/// Joins crawled pages with a normalized evidence document by URL key, under the given URL policy and optional crawl settings.
pub fn join_evidence(
    pages: &[Value],
    document: &Value,
    url_policy: Option<&Value>,
    crawl: Option<&Value>,
) -> Result<Value, CfError> {
    if !document.is_object() || document.get("format") != Some(&json!(NORMALIZED_FORMAT)) {
        return Err(import_error(format!(
            "expected a {NORMALIZED_FORMAT} document"
        )));
    }
    let mut policy = Map::from_iter(
        URL_POLICY_KEYS
            .iter()
            .map(|k| (k.to_string(), json!(false))),
    );
    if let Some(spec) = url_policy.filter(|v| pystr::truthy(Some(v))) {
        let entries = spec
            .as_object()
            .ok_or_else(|| CfError::Python("TypeError", "object is not iterable".to_owned()))?;
        for (name, value) in entries {
            if !URL_POLICY_KEYS.contains(&name.as_str()) {
                let mut unknown: Vec<String> = entries
                    .keys()
                    .filter(|k| !URL_POLICY_KEYS.contains(&k.as_str()))
                    .cloned()
                    .collect();
                unknown.sort();
                return Err(import_error(format!(
                    "url_policy has unsupported keys: {}",
                    pystr::repr(&json!(unknown))
                )));
            }
            if !value.is_boolean() {
                return Err(import_error(format!("url_policy {name} must be a boolean")));
            }
            policy.insert(name.clone(), value.clone());
        }
    }
    let key_fn = key_fn(&policy);
    let rows = document
        .get("rows")
        .filter(|v| pystr::truthy(Some(v)))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut page_by_key: IndexMap<String, Vec<Value>> = IndexMap::new();
    let mut page_raw_by_key: IndexMap<String, Vec<String>> = IndexMap::new();
    let mut unkeyable_pages: Vec<Value> = Vec::new();
    for (position, page) in pages.iter().enumerate() {
        let raw = get(page, "url")?;
        match key_fn(raw.as_ref()) {
            None => unkeyable_pages.push(json!({
                "position": position,
                "url": raw,
                "reason": "unkeyable_url",
            })),
            Some(key) => {
                page_by_key
                    .entry(key.clone())
                    .or_default()
                    .push(page.clone());
                let raws = page_raw_by_key.entry(key).or_default();
                if let Some(text) = raw.and_then(|v| v.as_str().map(str::to_owned)) {
                    if !raws.contains(&text) {
                        raws.push(text);
                    }
                }
            }
        }
    }

    let mut row_by_key: IndexMap<String, Vec<Value>> = IndexMap::new();
    let mut row_raw_by_key: IndexMap<String, Vec<String>> = IndexMap::new();
    let mut unkeyable_rows: Vec<Value> = Vec::new();
    for row in &rows {
        match row_join_key(row, &key_fn)? {
            None => unkeyable_rows.push(row.clone()),
            Some(key) => {
                row_by_key.entry(key.clone()).or_default().push(row.clone());
                let url = get(row, "url")?
                    .filter(|v| pystr::truthy(Some(v)))
                    .unwrap_or(json!({}));
                let candidate = get(&url, "resolved")?
                    .filter(|v| pystr::truthy(Some(v)))
                    .or(get(&url, "raw")?);
                let raws = row_raw_by_key.entry(key).or_default();
                if let Some(text) = candidate.and_then(|v| v.as_str().map(str::to_owned)) {
                    if !raws.contains(&text) {
                        raws.push(text);
                    }
                }
            }
        }
    }

    let mut shared_keys: Vec<String> = page_by_key
        .keys()
        .filter(|k| row_by_key.contains_key(*k))
        .cloned()
        .collect();
    shared_keys.sort();
    let mut matched = Vec::new();
    for key in &shared_keys {
        for page in &page_by_key[key] {
            matched.push(json!({
                "key": key,
                "url": get(page, "url")?,
                "page": page,
                "rows": row_by_key[key],
                "candidate_multiplicity": page_by_key[key].len() * row_by_key[key].len(),
            }));
        }
    }
    let mut crawl_keys: Vec<String> = page_by_key
        .keys()
        .filter(|k| !row_by_key.contains_key(*k))
        .cloned()
        .collect();
    crawl_keys.sort();
    let mut crawl_only = Vec::new();
    for key in &crawl_keys {
        for page in &page_by_key[key] {
            crawl_only.push(json!({"key": key, "url": get(page, "url")?, "page": page}));
        }
    }
    let mut external_keys: Vec<String> = row_by_key
        .keys()
        .filter(|k| !page_by_key.contains_key(*k))
        .cloned()
        .collect();
    external_keys.sort();
    let mut external_only = Vec::new();
    for key in &external_keys {
        for row in &row_by_key[key] {
            external_only.push(json!({"key": key, "row": row}));
        }
    }

    let sorted_collisions = |raws: &IndexMap<String, Vec<String>>| -> Vec<Value> {
        let mut items: Vec<(&String, &Vec<String>)> =
            raws.iter().filter(|(_, raw)| raw.len() > 1).collect();
        items.sort_by(|a, b| a.0.cmp(b.0));
        items
            .iter()
            .map(|(key, raw)| {
                let mut values = (*raw).clone();
                values.sort();
                json!({"key": key, "raw_values": values})
            })
            .collect()
    };
    let collisions = json!({
        "pages": sorted_collisions(&page_raw_by_key),
        "rows": sorted_collisions(&row_raw_by_key),
        "multiplicity": shared_keys
            .iter()
            .filter(|key| page_by_key[*key].len() > 1 || row_by_key[*key].len() > 1)
            .map(|key| json!({
                "key": key,
                "pages": page_by_key[key].len(),
                "rows": row_by_key[key].len(),
            }))
            .collect::<Vec<_>>(),
    });
    let summary = json!({
        "pages": pages.len(),
        "rows": rows.len(),
        "matched": matched.len(),
        "matched_pages": shared_keys.iter().map(|k| page_by_key[k].len()).sum::<usize>(),
        "matched_rows": shared_keys.iter().map(|k| row_by_key[k].len()).sum::<usize>(),
        "candidate_pairs": shared_keys
            .iter()
            .map(|k| page_by_key[k].len() * row_by_key[k].len())
            .sum::<usize>(),
        "crawl_only": crawl_only.len(),
        "external_only": external_only.len(),
        "unkeyable_pages": unkeyable_pages.len(),
        "unkeyable_rows": unkeyable_rows.len(),
        "page_key_collisions": collisions["pages"].as_array().map(Vec::len).unwrap_or_default(),
        "row_key_collisions": collisions["rows"].as_array().map(Vec::len).unwrap_or_default(),
        "multi_match_keys": collisions["multiplicity"].as_array().map(Vec::len).unwrap_or_default(),
        "ambiguous_rows": rows.iter().filter(|row| {
            row.get("ambiguous").map(|v| pystr::truthy(Some(v))).unwrap_or(false)
        }).count(),
    });
    let crawl_doc = match crawl.filter(|v| pystr::truthy(Some(v))) {
        None => json!({}),
        Some(value) if value.is_object() => value.clone(),
        Some(other) => {
            return Err(CfError::Python(
                "TypeError",
                format!("'{}' object is not iterable", pystr::type_name(other)),
            ))
        }
    };
    Ok(json!({
        "format": JOIN_FORMAT,
        "url_policy": {"version": "external_join.v1", "ignore_query": policy["ignore_query"], "ignore_scheme": policy["ignore_scheme"], "casefold_path": policy["casefold_path"]},
        "crawl": crawl_doc,
        "evidence": evidence_header(document)?,
        "matched": matched,
        "crawl_only": crawl_only,
        "external_only": external_only,
        "unkeyable_pages": unkeyable_pages,
        "unkeyable_rows": unkeyable_rows,
        "collisions": collisions,
        "summary": summary,
    }))
}

/// Compatibility result for public output, with client-identifying axes redacted.
pub fn public_compatibility(result: &Value) -> Value {
    let mut axes = Map::new();
    if let Some(entries) = result.get("axes").and_then(Value::as_object) {
        for (side, axis) in entries {
            let mut redacted = Map::new();
            if let Some(fields) = axis.as_object() {
                for (name, value) in fields {
                    redacted.insert(
                        name.clone(),
                        if PRIVATE_AXES.contains(&name.as_str()) && !value.is_null() {
                            json!("redacted")
                        } else {
                            value.clone()
                        },
                    );
                }
            }
            axes.insert(side.clone(), Value::Object(redacted));
        }
    }
    let mut out = result.clone();
    out["axes"] = Value::Object(axes);
    out
}

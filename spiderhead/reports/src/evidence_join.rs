//! `join_evidence`: joins crawl pages to a `seohead.normalized-evidence.v1` document,
//! keeping every population. Compatibility checks live in `bi/compatibility.rs`.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Map, Value};
use thiserror::Error;

use crate::join::normalize_join_key_opts;
use crate::py;

/// Join failure; shares the import error type.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct EvidenceJoinError(pub String);

fn err(msg: impl Into<String>) -> EvidenceJoinError {
    EvidenceJoinError(msg.into())
}

pub const JOIN_FORMAT: &str = "spiderhead.evidence-join.v1";
const NORMALIZED_FORMAT: &str = "spiderhead.normalized-evidence.v1";
const URL_POLICY_KEYS: [&str; 3] = ["ignore_query", "ignore_scheme", "casefold_path"];

fn key_fn(policy: &Map<String, Value>, value: &Value) -> Option<String> {
    let text = value.as_str()?;
    normalize_join_key_opts(
        Some(text),
        policy["ignore_query"].as_bool().unwrap_or(false),
        policy["ignore_scheme"].as_bool().unwrap_or(false),
        policy["casefold_path"].as_bool().unwrap_or(false),
    )
}

/// `row_join_key`.
fn row_join_key(row: &Value, policy: &Map<String, Value>) -> Option<String> {
    let url = row.get("url").cloned().unwrap_or(json!({}));
    if url.get("state").and_then(Value::as_str) == Some("unkeyable") {
        return None;
    }
    let candidate = url
        .get("resolved")
        .filter(|v| py::truthy(Some(*v)))
        .or_else(|| url.get("raw"))
        .cloned();
    key_fn(policy, &candidate.unwrap_or(Value::Null))
}

/// `evidence_header` — the dataset-level provenance a joined population
/// carries with it.
fn evidence_header(document: &Value) -> Value {
    let mapping = document.get("mapping").cloned().unwrap_or(json!({}));
    let source = mapping.get("source").cloned().unwrap_or(json!({}));
    let provenance = document.get("provenance").cloned().unwrap_or(json!({}));
    let fields = provenance.get("fields").cloned().unwrap_or(json!({}));
    let mut field_origins = Map::new();
    if let Some(obj) = fields.as_object() {
        for (name, entry) in obj {
            field_origins.insert(
                name.clone(),
                entry.get("origin").cloned().unwrap_or(Value::Null),
            );
        }
    }
    json!({
        "provider": source.get("provider").cloned().unwrap_or(Value::Null),
        "operation": source.get("operation").cloned().unwrap_or(Value::Null),
        "reporting_identity": source.get("reporting_identity").cloned().unwrap_or(Value::Null),
        "privacy": source.get("privacy").cloned().unwrap_or(Value::Null),
        "search_engine": source.get("search_engine").cloned().unwrap_or(Value::Null),
        "search_type": source.get("search_type").cloned().unwrap_or(Value::Null),
        "attribution": source.get("attribution").cloned().unwrap_or(Value::Null),
        "site_origin": source.get("site_origin").cloned().unwrap_or(Value::Null),
        "period": mapping.get("period").cloned().unwrap_or(Value::Null),
        "timezone": source.get("timezone").cloned().unwrap_or(Value::Null),
        "dimensions": mapping.get("dimensions").cloned().unwrap_or(json!([])),
        "metrics": mapping.get("metrics").cloned().unwrap_or(json!([])),
        "collection": mapping.get("collection").cloned().unwrap_or(json!({})),
        "file": provenance.get("file").cloned().unwrap_or(Value::Null),
        "artifact_reference": provenance.get("artifact_reference").cloned().unwrap_or(Value::Null),
        "field_origins": Value::Object(field_origins),
    })
}

/// `join_evidence(pages, document, url_policy=None, crawl=None)`.
pub fn join_evidence(
    pages: &[Value],
    document: &Value,
    url_policy: Option<&Value>,
    crawl: Option<&Value>,
) -> Result<Value, EvidenceJoinError> {
    if !document.is_object() || document.get("format") != Some(&json!(NORMALIZED_FORMAT)) {
        return Err(err(format!("expected a {NORMALIZED_FORMAT} document")));
    }
    let mut policy = Map::new();
    for name in URL_POLICY_KEYS {
        policy.insert(name.to_string(), json!(false));
    }
    if let Some(up) = url_policy {
        let unknown: Vec<String> = up
            .as_object()
            .map(|m| {
                m.keys()
                    .filter(|k| !URL_POLICY_KEYS.contains(&k.as_str()))
                    .cloned()
                    .collect()
            })
            .unwrap_or_else(|| vec!["<not-a-dict>".to_string()]);
        if !unknown.is_empty() {
            let mut sorted = unknown;
            sorted.sort();
            return Err(err(format!("url_policy has unsupported keys: {sorted:?}")));
        }
        for (name, value) in up.as_object().unwrap() {
            if !value.is_boolean() {
                return Err(err(format!("url_policy {name} must be a boolean")));
            }
            policy.insert(name.clone(), value.clone());
        }
    }
    let rows = document
        .get("rows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut page_by_key: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut page_raw_by_key: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut unkeyable_pages: Vec<Value> = Vec::new();
    for (position, page) in pages.iter().enumerate() {
        let raw = page.get("url").cloned().unwrap_or(Value::Null);
        match key_fn(&policy, &raw) {
            None => unkeyable_pages.push(json!({
                "position": position,
                "url": raw,
                "reason": "unkeyable_url",
            })),
            Some(key) => {
                page_by_key.entry(key.clone()).or_default().push(position);
                page_raw_by_key
                    .entry(key)
                    .or_default()
                    .insert(raw.as_str().unwrap_or_default().to_string());
            }
        }
    }

    let mut row_by_key: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut row_raw_by_key: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut unkeyable_rows: Vec<Value> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        match row_join_key(row, &policy) {
            None => unkeyable_rows.push(row.clone()),
            Some(key) => {
                row_by_key.entry(key.clone()).or_default().push(index);
                let url = row.get("url").cloned().unwrap_or(json!({}));
                let raw = url
                    .get("resolved")
                    .filter(|v| py::truthy(Some(*v)))
                    .or_else(|| url.get("raw"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                row_raw_by_key.entry(key).or_default().insert(raw);
            }
        }
    }

    let shared_keys: Vec<String> = page_by_key
        .keys()
        .filter(|k| row_by_key.contains_key(*k))
        .cloned()
        .collect();
    let mut matched: Vec<Value> = Vec::new();
    for key in &shared_keys {
        for &position in &page_by_key[key] {
            let page = &pages[position];
            matched.push(json!({
                "key": key,
                "url": page.get("url").cloned().unwrap_or(Value::Null),
                "page": page.clone(),
                "rows": row_by_key[key]
                    .iter()
                    .map(|&i| rows[i].clone())
                    .collect::<Vec<_>>(),
                "candidate_multiplicity": page_by_key[key].len() * row_by_key[key].len(),
            }));
        }
    }
    let mut crawl_only: Vec<Value> = Vec::new();
    for key in page_by_key.keys() {
        if !row_by_key.contains_key(key) {
            for &position in &page_by_key[key] {
                let page = &pages[position];
                crawl_only.push(json!({
                    "key": key,
                    "url": page.get("url").cloned().unwrap_or(Value::Null),
                    "page": page.clone(),
                }));
            }
        }
    }
    let mut external_only: Vec<Value> = Vec::new();
    for key in row_by_key.keys() {
        if !page_by_key.contains_key(key) {
            for &index in &row_by_key[key] {
                external_only.push(json!({"key": key, "row": rows[index].clone()}));
            }
        }
    }
    let collisions = json!({
        "pages": page_raw_by_key
            .iter()
            .filter(|(_, raw)| raw.len() > 1)
            .map(|(key, raw)| json!({"key": key, "raw_values": raw.iter().collect::<Vec<_>>()}))
            .collect::<Vec<_>>(),
        "rows": row_raw_by_key
            .iter()
            .filter(|(_, raw)| raw.len() > 1)
            .map(|(key, raw)| json!({"key": key, "raw_values": raw.iter().collect::<Vec<_>>()}))
            .collect::<Vec<_>>(),
        "multiplicity": shared_keys
            .iter()
            .filter(|key| page_by_key[*key].len() > 1 || row_by_key[*key].len() > 1)
            .map(|key| json!({"key": key, "pages": page_by_key[key].len(), "rows": row_by_key[key].len()}))
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
        "page_key_collisions": collisions["pages"].as_array().unwrap().len(),
        "row_key_collisions": collisions["rows"].as_array().unwrap().len(),
        "multi_match_keys": collisions["multiplicity"].as_array().unwrap().len(),
        "ambiguous_rows": rows
            .iter()
            .filter(|row| row.get("ambiguous").and_then(Value::as_bool).unwrap_or(false))
            .count(),
    });
    let mut url_policy_out = Map::new();
    url_policy_out.insert("version".into(), json!("external_join.v1"));
    for (k, v) in &policy {
        url_policy_out.insert(k.clone(), v.clone());
    }
    Ok(json!({
        "format": JOIN_FORMAT,
        "url_policy": Value::Object(url_policy_out),
        "crawl": crawl.cloned().unwrap_or(json!({})),
        "evidence": evidence_header(document),
        "matched": matched,
        "crawl_only": crawl_only,
        "external_only": external_only,
        "unkeyable_pages": unkeyable_pages,
        "unkeyable_rows": unkeyable_rows,
        "collisions": collisions,
        "summary": summary,
    }))
}

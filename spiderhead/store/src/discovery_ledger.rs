//! Typed, bounded discovery occurrences for explicit ``scan.v2`` artifacts.
#![allow(clippy::too_many_arguments, clippy::type_complexity)]

use crate::error::StoreError;
use regex::Regex;
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::sync::OnceLock;

pub const VERSION: &str = "discovery_ledger.v1";

pub const MAX_OCCURRENCES_PER_DOCUMENT: i64 = 2000;

/// Sorted so the output order is stable.
pub const RELATIONS: &[&str] = &[
    "alternate",
    "canonical",
    "form_action",
    "hreflang",
    "http_link",
    "hyperlink",
    "next",
    "prev",
    "redirect",
    "refresh",
    "seed",
    "x_default",
];

/// Sorted so the output order is stable.
pub const OUTCOMES: &[&str] = &[
    "blocked",
    "excluded",
    "fetched",
    "queued",
    "unmeasured",
    "unresolved",
];

/// Regex source.
pub const HEADER_LINK: &str = r#"(?i)\s*<([^>]*)>\s*(?:;\s*rel="?([^;,"]+)"?)?"#;

const HEADER_ATTR: &str = r#"(?i);\s*([A-Za-z][A-Za-z0-9_-]*)\s*=\s*(?:"([^"]*)"|([^;]+))"#;
const REFRESH_URL: &str = r"(?i)url\s*=\s*(.+)$";

fn header_link_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(&format!(r"^{HEADER_LINK}")).expect("static regex"))
}
fn header_attr_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(HEADER_ATTR).expect("static regex"))
}
fn refresh_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(REFRESH_URL).expect("static regex"))
}

fn invalid(msg: &str) -> StoreError {
    StoreError::Invalid(msg.to_string())
}

/// Sorted-key JSON text; `compact` picks `(",", ":")` separators over
/// the default `(", ": ")`, and non-ASCII is escaped as `\uXXXX`.
pub fn py_dumps(value: &Value, compact: bool, ensure_ascii: bool) -> String {
    fn encode(v: &Value, compact: bool, ascii: bool, out: &mut String) {
        match v {
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                out.push('{');
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push_str(if compact { "," } else { ", " });
                    }
                    encode(&Value::String((*k).clone()), compact, ascii, out);
                    out.push_str(if compact { ":" } else { ": " });
                    encode(&map[*k], compact, ascii, out);
                }
                out.push('}');
            }
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(if compact { "," } else { ", " });
                    }
                    encode(item, compact, ascii, out);
                }
                out.push(']');
            }
            Value::String(s) => {
                let text = serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into());
                for ch in text.chars() {
                    if ascii && (ch as u32) >= 0x80 {
                        let mut buf = [0u16; 2];
                        for unit in ch.encode_utf16(&mut buf) {
                            out.push_str(&format!("\\u{unit:04x}"));
                        }
                    } else {
                        out.push(ch);
                    }
                }
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut out = String::new();
    encode(value, compact, ensure_ascii, &mut out);
    out
}

/// Sorted-key compact JSON with non-ASCII escaped as `\uXXXX`.
pub fn canonical_json(value: &Value) -> String {
    py_dumps(value, true, true)
}

/// Sorted keys, default separators, and no ASCII escaping.
pub fn dump(value: &Value) -> String {
    py_dumps(value, false, false)
}

/// Truthiness of a JSON value.
pub(crate) fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// Text of an optional JSON value; empty when absent.
pub(crate) fn py_text(v: Option<&Value>) -> String {
    if !truthy(v) {
        return String::new();
    }
    py_str(v.expect("truthy implies present"))
}

/// Text of a JSON value: `True`/`False` for booleans, JSON text for containers.
pub(crate) fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

/// First truthy value of two keys on a JSON object.
fn get_either<'a>(map: &'a Map<String, Value>, first: &str, second: &str) -> Option<&'a Value> {
    let a = map.get(first);
    if truthy(a) {
        a
    } else {
        map.get(second)
    }
}

pub(crate) fn is_py_int(v: &Value) -> bool {
    v.is_i64() || v.is_u64()
}

/// Resolve a relative reference against a base URL, for the cases the ledger uses.
pub(crate) fn urljoin(base: &str, raw: &str) -> String {
    match url::Url::parse(base) {
        Ok(b) => b
            .join(raw)
            .map(|u| u.to_string())
            .unwrap_or_else(|_| raw.to_string()),
        Err(_) => raw.to_string(),
    }
}

/// Document base URL: the first `<base href>` outside `<template>`, resolved against `final_url`.
/// Simplified base-URL lookup; the parse crate's version is the fuller one.
pub(crate) fn document_base_url(html: &str, final_url: &str) -> String {
    let doc = scraper::Html::parse_document(html);
    let sel = scraper::Selector::parse("base[href]").expect("static selector");
    for el in doc.select(&sel) {
        let inert = el
            .ancestors()
            .filter_map(scraper::ElementRef::wrap)
            .any(|a| a.value().name() == "template");
        if inert {
            continue;
        }
        if let Some(href) = el.value().attr("href") {
            return urljoin(final_url, href);
        }
    }
    final_url.to_string()
}

/// Relation from a set of lowercased rel tokens (HTML branch: no `http_link` fallback).
fn relation_from_rels(rels: &BTreeSet<String>) -> &'static str {
    if rels.contains("canonical") {
        "canonical"
    } else if rels.contains("alternate") {
        "alternate"
    } else if rels.contains("next") {
        "next"
    } else if rels.contains("prev") {
        "prev"
    } else {
        ""
    }
}

pub fn ensure_schema(con: &Connection) -> Result<(), StoreError> {
    con.execute_batch(
        "CREATE TABLE IF NOT EXISTS discovery_occurrences (\
         occurrence_key TEXT PRIMARY KEY,source_kind TEXT NOT NULL,relation TEXT NOT NULL,\
         source_url_id INTEGER,source_document_id INTEGER,source_response_id INTEGER,representation TEXT NOT NULL,\
         carrier TEXT NOT NULL,raw_value TEXT NOT NULL,resolved_value TEXT NOT NULL,\
         target_url_id INTEGER,depth INTEGER,outcome TEXT NOT NULL,reason TEXT NOT NULL,attributes_json TEXT NOT NULL);\
         CREATE INDEX IF NOT EXISTS discovery_occurrences_target ON discovery_occurrences(target_url_id,outcome);",
    )?;
    let mut columns = BTreeSet::new();
    {
        let mut st = con.prepare("PRAGMA table_info(discovery_occurrences)")?;
        let rows = st.query_map([], |r| r.get::<_, String>(1))?;
        for name in rows {
            columns.insert(name?);
        }
    }
    for (name, definition) in [
        ("source_response_id", "INTEGER"),
        ("attributes_json", "TEXT NOT NULL DEFAULT '{}'"),
    ] {
        if !columns.contains(name) {
            con.execute_batch(&format!(
                "ALTER TABLE discovery_occurrences ADD COLUMN {name} {definition}"
            ))?;
        }
    }
    con.execute_batch(
        "CREATE TABLE IF NOT EXISTS discovery_ledger_coverage (\
         source_document_id INTEGER NOT NULL,representation TEXT NOT NULL,captured INTEGER NOT NULL,\
         omitted INTEGER NOT NULL,state TEXT NOT NULL,reason TEXT NOT NULL,\
         PRIMARY KEY(source_document_id,representation));",
    )?;
    Ok(())
}

pub fn url_id(con: &Connection, value: &str) -> Result<Option<i64>, StoreError> {
    if value.is_empty() {
        return Ok(None);
    }
    Ok(con
        .query_row("SELECT url_id FROM urls WHERE url=?1", [value], |r| {
            r.get(0)
        })
        .optional()?)
}

pub fn outcome(con: &Connection, value: &str) -> Result<(String, String), StoreError> {
    let Some(target_id) = url_id(con, value)? else {
        return Ok((
            "unresolved".into(),
            "target URL was not retained in this scan".into(),
        ));
    };
    let state: Option<String> = con
        .query_row(
            "SELECT state FROM frontier WHERE url_id=?1",
            [target_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(state) = state else {
        return Ok((
            "unresolved".into(),
            "target URL has no frontier outcome".into(),
        ));
    };
    Ok(match state.as_str() {
        "done" => ("fetched".into(), String::new()),
        "queued" => ("queued".into(), String::new()),
        "excluded" => {
            let reason: Option<String> = con
                .query_row(
                    "SELECT reason FROM decisions WHERE url=?1 ORDER BY decision_id DESC LIMIT 1",
                    [value],
                    |r| r.get(0),
                )
                .optional()?;
            let detail = reason.unwrap_or_else(|| "frontier excluded target".into());
            let kind = if detail.to_lowercase().contains("robot") {
                "blocked"
            } else {
                "excluded"
            };
            (kind.into(), detail)
        }
        other => ("unresolved".into(), format!("frontier state is {other}")),
    })
}

/// Insert one occurrence, or verify an identical retry.
pub fn put(con: &Connection, occurrence: &Map<String, Value>) -> Result<(), StoreError> {
    const REQUIRED: [&str; 14] = [
        "occurrence_key",
        "source_kind",
        "relation",
        "source_url_id",
        "source_document_id",
        "source_response_id",
        "representation",
        "carrier",
        "raw_value",
        "resolved_value",
        "depth",
        "outcome",
        "reason",
        "attributes",
    ];
    if occurrence.len() != REQUIRED.len() || REQUIRED.iter().any(|k| !occurrence.contains_key(*k)) {
        return Err(invalid("discovery occurrence has unsupported fields"));
    }
    let text = |k: &str| occurrence[k].as_str();
    let relation = text("relation").unwrap_or("");
    let out_kind = text("outcome").unwrap_or("");
    if !RELATIONS.contains(&relation) || !OUTCOMES.contains(&out_kind) {
        return Err(invalid(
            "discovery occurrence relation or outcome is invalid",
        ));
    }
    for k in [
        "occurrence_key",
        "source_kind",
        "representation",
        "carrier",
        "raw_value",
        "resolved_value",
        "reason",
    ] {
        if text(k).is_none() {
            return Err(invalid("discovery occurrence text is invalid"));
        }
    }
    let len = |k: &str| text(k).map(|s| s.chars().count()).unwrap_or(0);
    if len("occurrence_key") > 512
        || len("carrier") > 512
        || ["raw_value", "resolved_value", "reason"]
            .iter()
            .any(|k| len(k) > 8192)
    {
        return Err(invalid(
            "discovery occurrence exceeds its bounded text budget",
        ));
    }
    let depth = &occurrence["depth"];
    if !depth.is_null() && (!is_py_int(depth) || depth.as_i64().unwrap_or(-1) < 0) {
        return Err(invalid("discovery occurrence depth is invalid"));
    }
    let resp = &occurrence["source_response_id"];
    if !resp.is_null() && (!is_py_int(resp) || resp.as_i64().unwrap_or(0) < 1) {
        return Err(invalid("discovery occurrence source response is invalid"));
    }
    let Value::Object(attributes) = &occurrence["attributes"] else {
        return Err(invalid("discovery occurrence attributes are invalid"));
    };
    let attributes_json = canonical_json(&Value::Object(attributes.clone()));
    let resolved = text("resolved_value").unwrap_or("");
    let target_id = url_id(con, resolved)?;
    let opt_int = |k: &str| -> SqlValue {
        match occurrence[k].as_i64() {
            Some(i) => SqlValue::Integer(i),
            None => SqlValue::Null,
        }
    };
    let txt = |k: &str| SqlValue::Text(text(k).unwrap_or("").to_string());
    let values: Vec<SqlValue> = vec![
        txt("occurrence_key"),
        txt("source_kind"),
        txt("relation"),
        opt_int("source_url_id"),
        opt_int("source_document_id"),
        opt_int("source_response_id"),
        txt("representation"),
        txt("carrier"),
        txt("raw_value"),
        txt("resolved_value"),
        target_id.map_or(SqlValue::Null, SqlValue::Integer),
        opt_int("depth"),
        txt("outcome"),
        txt("reason"),
        SqlValue::Text(attributes_json),
    ];
    let existing: Option<Vec<SqlValue>> = con
        .query_row(
            "SELECT occurrence_key,source_kind,relation,source_url_id,source_document_id,source_response_id,\
             representation,carrier,raw_value,resolved_value,target_url_id,depth,outcome,reason,attributes_json \
             FROM discovery_occurrences WHERE occurrence_key=?1",
            [text("occurrence_key").unwrap_or("")],
            |r| (0..15).map(|i| r.get::<_, SqlValue>(i)).collect(),
        )
        .optional()?;
    if let Some(existing) = existing {
        if existing != values {
            return Err(invalid(
                "discovery occurrence retry differs from immutable saved evidence",
            ));
        }
        return Ok(());
    }
    con.execute(
        "INSERT INTO discovery_occurrences VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
        rusqlite::params_from_iter(values.iter()),
    )?;
    Ok(())
}

/// Split one Link header without treating quoted or angle-bracket commas as separators.
pub fn split_header_values(value: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut start = 0usize;
    let (mut quoted, mut angled, mut escaped) = (false, false, false);
    for (index, ch) in value.char_indices() {
        if escaped {
            escaped = false;
        } else if ch == '\\' && quoted {
            escaped = true;
        } else if ch == '"' && !angled {
            quoted = !quoted;
        } else if ch == '<' && !quoted {
            angled = true;
        } else if ch == '>' && !quoted {
            angled = false;
        } else if ch == ',' && !quoted && !angled {
            values.push(value[start..index].to_string());
            start = index + 1;
        }
    }
    values.push(value[start..].to_string());
    values
}

/// Parse one Link header part into its target and lowercased attributes.
pub fn header_link(part: &str) -> Option<(String, BTreeMapStr)> {
    let caps = header_link_re().captures(part)?;
    let raw = caps.get(1)?.as_str().to_string();
    let mut attributes = std::collections::BTreeMap::new();
    for c in header_attr_re().captures_iter(part) {
        let name = c.get(1).map(|m| m.as_str()).unwrap_or("").to_lowercase();
        let value = match (c.get(2), c.get(3)) {
            (Some(q), _) => q.as_str().to_string(),
            (None, Some(b)) => b.as_str().trim().to_string(),
            _ => String::new(),
        };
        attributes.insert(name, value);
    }
    Some((raw, attributes))
}

/// Attribute map of one Link header part (lowercased names, sorted for stable iteration).
pub type BTreeMapStr = std::collections::BTreeMap<String, String>;

/// Relation of a Link header from its attributes (`rel` and `hreflang`).
fn header_relation(attributes: &BTreeMapStr) -> &'static str {
    let rels: BTreeSet<String> = attributes
        .get("rel")
        .map(|r| {
            r.to_lowercase()
                .split_whitespace()
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    let mut relation = match relation_from_rels(&rels) {
        "" => "http_link",
        other => other,
    };
    if relation == "alternate" {
        if let Some(h) = attributes.get("hreflang") {
            relation = if h.to_lowercase() == "x-default" {
                "x_default"
            } else {
                "hreflang"
            };
        }
    }
    relation
}

/// One relation item: (relation, carrier, raw, resolved, attributes).
pub type RelationItem = (String, String, String, String, BTreeMapStr);

/// Return relation, carrier, raw and resolved values without fetching.
pub fn relation_items(
    html: Option<&str>,
    source_url: &str,
    headers: Option<&Map<String, Value>>,
) -> Vec<RelationItem> {
    let mut items: Vec<RelationItem> = Vec::new();
    if let Some(html) = html {
        let base_url = document_base_url(html, source_url);
        let doc = scraper::Html::parse_document(html);
        let link_sel = scraper::Selector::parse("link").expect("static selector");
        for (ordinal, tag) in doc.select(&link_sel).enumerate() {
            let el = tag.value();
            let rels: BTreeSet<String> = el
                .attr("rel")
                .unwrap_or("")
                .split_whitespace()
                .map(|s| s.to_lowercase())
                .collect();
            let raw = el.attr("href").unwrap_or("").trim().to_string();
            if raw.is_empty() {
                continue;
            }
            let mut relation = relation_from_rels(&rels).to_string();
            if relation.is_empty() {
                continue;
            }
            let hreflang = el.attr("hreflang").filter(|h| !h.is_empty());
            if relation == "alternate" {
                if let Some(h) = hreflang {
                    relation = if h.to_lowercase() == "x-default" {
                        "x_default".into()
                    } else {
                        "hreflang".into()
                    };
                }
            }
            let mut attributes = BTreeMapStr::new();
            if let Some(h) = hreflang {
                attributes.insert("hreflang".into(), h.to_string());
            }
            items.push((
                relation,
                format!("link[{ordinal}]"),
                raw.clone(),
                urljoin(&base_url, &raw),
                attributes,
            ));
        }
        let meta_sel = scraper::Selector::parse("meta").expect("static selector");
        let meta = doc.select(&meta_sel).find(|m| {
            m.value()
                .attr("http-equiv")
                .map(|v| v.eq_ignore_ascii_case("refresh"))
                .unwrap_or(false)
        });
        if let Some(meta) = meta {
            let raw = meta.value().attr("content").unwrap_or("").to_string();
            if let Some(c) = refresh_re().captures(&raw) {
                let target = c[1].trim_matches(|ch| ch == ' ' || ch == '\'' || ch == '"');
                items.push((
                    "refresh".into(),
                    "meta[http-equiv=refresh]".into(),
                    raw.clone(),
                    urljoin(&base_url, target),
                    BTreeMapStr::new(),
                ));
            }
        }
    }
    if let Some(headers) = headers {
        if let Some(Value::String(location)) = get_either(headers, "location", "Location") {
            if !location.is_empty() {
                items.push((
                    "redirect".into(),
                    "http-location".into(),
                    location.clone(),
                    urljoin(source_url, location),
                    BTreeMapStr::new(),
                ));
            }
        }
        if let Some(Value::String(raw_header)) = get_either(headers, "link", "Link") {
            for (ordinal, part) in split_header_values(raw_header).iter().enumerate() {
                if let Some((raw, attributes)) = header_link(part) {
                    let relation = header_relation(&attributes).to_string();
                    items.push((
                        relation,
                        format!("http-link[{ordinal}]"),
                        raw.clone(),
                        urljoin(source_url, &raw),
                        attributes,
                    ));
                }
            }
        }
        if let Some(Value::String(refresh)) = get_either(headers, "refresh", "Refresh") {
            if let Some(c) = refresh_re().captures(refresh) {
                let target = c[1].trim_matches(|ch| ch == ' ' || ch == '\'' || ch == '"');
                items.push((
                    "refresh".into(),
                    "http-refresh".into(),
                    refresh.clone(),
                    urljoin(source_url, target),
                    BTreeMapStr::new(),
                ));
            }
        }
    }
    items
}

fn occurrence(
    occurrence_key: String,
    source_kind: &str,
    relation: &str,
    source_url_id: Option<i64>,
    source_document_id: Option<i64>,
    source_response_id: Option<i64>,
    representation: &str,
    carrier: &str,
    raw: &str,
    resolved: &str,
    depth: Value,
    outcome: &str,
    reason: &str,
    attributes: Value,
) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("occurrence_key".into(), Value::String(occurrence_key));
    m.insert("source_kind".into(), Value::String(source_kind.into()));
    m.insert("relation".into(), Value::String(relation.into()));
    m.insert(
        "source_url_id".into(),
        source_url_id.map_or(Value::Null, Value::from),
    );
    m.insert(
        "source_document_id".into(),
        source_document_id.map_or(Value::Null, Value::from),
    );
    m.insert(
        "source_response_id".into(),
        source_response_id.map_or(Value::Null, Value::from),
    );
    m.insert(
        "representation".into(),
        Value::String(representation.into()),
    );
    m.insert("carrier".into(), Value::String(carrier.into()));
    m.insert("raw_value".into(), Value::String(raw.into()));
    m.insert("resolved_value".into(), Value::String(resolved.into()));
    m.insert("depth".into(), depth);
    m.insert("outcome".into(), Value::String(outcome.into()));
    m.insert("reason".into(), Value::String(reason.into()));
    m.insert("attributes".into(), attributes);
    m
}

fn attrs_value(attributes: &BTreeMapStr) -> Value {
    Value::Object(
        attributes
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect(),
    )
}

/// Store bounded document relations and candidate outcomes in the active transaction.
pub fn store_document_relations(
    con: &Connection,
    source_url_id: i64,
    source_document_id: Option<i64>,
    source_response_id: Option<i64>,
    representation: &str,
    source_url: &str,
    depth: i64,
    html: Option<&str>,
    headers: Option<&Map<String, Value>>,
    links: &[Map<String, Value>],
    candidates: &[Map<String, Value>],
    decisions: &[Map<String, Value>],
    forms: &[Map<String, Value>],
) -> Result<(), StoreError> {
    let Some(doc_id) = source_document_id else {
        return Ok(());
    };
    let depth_v = Value::from(depth);
    let mut rows: Vec<Map<String, Value>> = Vec::new();
    for (ordinal, (relation, carrier, raw, resolved, attributes)) in
        relation_items(html, source_url, headers)
            .into_iter()
            .enumerate()
    {
        let (out, reason) = outcome(con, &resolved)?;
        let kind = if carrier.starts_with("http-") {
            "http_header"
        } else {
            "html"
        };
        rows.push(occurrence(
            format!("document:{doc_id}:{relation}:{ordinal}"),
            kind,
            &relation,
            Some(source_url_id),
            Some(doc_id),
            source_response_id,
            representation,
            &carrier,
            &raw,
            &resolved,
            depth_v.clone(),
            &out,
            &reason,
            attrs_value(&attributes),
        ));
    }
    let empty = Map::new();
    let headers = headers.unwrap_or(&empty);
    for (ordinal, item) in links.iter().enumerate() {
        let raw = py_text(item.get("raw_href"));
        let resolved = py_text(item.get("destination"));
        let (out, reason) = outcome(con, &resolved)?;
        let carrier = py_text_or(item.get("position"), "a[href]");
        rows.push(occurrence(
            format!("document:{doc_id}:link:{ordinal}"),
            "html",
            "hyperlink",
            Some(source_url_id),
            Some(doc_id),
            source_response_id,
            representation,
            &carrier,
            &raw,
            &resolved,
            depth_v.clone(),
            &out,
            &reason,
            Value::Object(Map::new()),
        ));
    }
    let redirect_raw = py_text(get_either(headers, "location", "Location"));
    let redirect_target = if redirect_raw.is_empty() {
        String::new()
    } else {
        urljoin(source_url, &redirect_raw)
    };
    for (ordinal, item) in candidates.iter().enumerate() {
        let raw = py_text(item.get("requested_url"));
        let resolved = py_text(item.get("frontier_url"));
        let (out, reason) = outcome(con, &resolved)?;
        let relation = if resolved == redirect_target {
            "redirect"
        } else {
            "hyperlink"
        };
        rows.push(occurrence(
            format!("document:{doc_id}:candidate:{ordinal}"),
            "document",
            relation,
            Some(source_url_id),
            Some(doc_id),
            source_response_id,
            representation,
            "frontier_candidate",
            &raw,
            &resolved,
            item.get("depth").cloned().unwrap_or(Value::Null),
            &out,
            &reason,
            Value::Object(Map::new()),
        ));
    }
    for (ordinal, item) in decisions.iter().enumerate() {
        let raw = py_text(item.get("url"));
        let carrier = py_text_or(item.get("source"), "decision");
        let reason = py_text_or(item.get("reason"), "decision unavailable");
        rows.push(occurrence(
            format!("document:{doc_id}:decision:{ordinal}"),
            "decision",
            "hyperlink",
            Some(source_url_id),
            Some(doc_id),
            source_response_id,
            representation,
            &carrier,
            &raw,
            &raw,
            item.get("depth").cloned().unwrap_or(Value::Null),
            "excluded",
            &reason,
            Value::Object(Map::new()),
        ));
    }
    for (ordinal, item) in forms.iter().enumerate() {
        let raw = py_text(item.get("action"));
        let resolved = if raw.is_empty() {
            String::new()
        } else {
            urljoin(source_url, &raw)
        };
        let (out, reason) = outcome(con, &resolved)?;
        let (out, reason) = if resolved.is_empty() {
            (
                "unmeasured".to_string(),
                "form action is absent".to_string(),
            )
        } else {
            (out, reason)
        };
        rows.push(occurrence(
            format!("document:{doc_id}:form:{ordinal}"),
            "document",
            "form_action",
            Some(source_url_id),
            Some(doc_id),
            source_response_id,
            representation,
            "form[action]",
            &raw,
            &resolved,
            depth_v.clone(),
            &out,
            &reason,
            Value::Object(Map::new()),
        ));
    }
    let cap = MAX_OCCURRENCES_PER_DOCUMENT as usize;
    let omitted = rows.len().saturating_sub(cap);
    let captured_count = rows.len().min(cap);
    let state = if omitted > 0 { "partial" } else { "complete" };
    let reason = if omitted > 0 {
        "occurrence cap reached"
    } else {
        ""
    };
    let coverage = (
        doc_id,
        representation.to_string(),
        captured_count as i64,
        omitted as i64,
        state.to_string(),
        reason.to_string(),
    );
    let existing: Option<(i64, String, i64, i64, String, String)> = con
        .query_row(
            "SELECT source_document_id,representation,captured,omitted,state,reason \
             FROM discovery_ledger_coverage WHERE source_document_id=?1 AND representation=?2",
            rusqlite::params![doc_id, representation],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .optional()?;
    match existing {
        Some(existing) => {
            if existing != coverage {
                return Err(invalid(
                    "discovery coverage retry differs from immutable saved evidence",
                ));
            }
        }
        None => {
            con.execute(
                "INSERT INTO discovery_ledger_coverage VALUES(?1,?2,?3,?4,?5,?6)",
                rusqlite::params![
                    coverage.0, coverage.1, coverage.2, coverage.3, coverage.4, coverage.5
                ],
            )?;
        }
    }
    for row in rows.iter().take(captured_count) {
        put(con, row)?;
    }
    Ok(())
}

/// Text of an optional JSON value, or the default when absent.
fn py_text_or(v: Option<&Value>, default: &str) -> String {
    if truthy(v) {
        py_text(v)
    } else {
        default.to_string()
    }
}

pub fn store_seeds(con: &Connection, entries: &[Map<String, Value>]) -> Result<(), StoreError> {
    for (ordinal, item) in entries.iter().enumerate() {
        let raw = py_text(item.get("requested_url"));
        let resolved = py_text(item.get("frontier_url"));
        let (mut out, mut reason) = outcome(con, &resolved)?;
        if truthy(item.get("reason")) {
            out = "excluded".into();
            reason = py_text(item.get("reason"));
        }
        let digest = hex(Sha256::digest(raw.as_bytes()).as_slice());
        let source = py_text_or(item.get("source"), "seed");
        let source_digest = hex(Sha256::digest(source.as_bytes()).as_slice());
        let source_digest = &source_digest[..16];
        put(
            con,
            &occurrence(
                format!("seed:{source_digest}:{ordinal}:{digest}"),
                "seed",
                "seed",
                None,
                None,
                None,
                "unmeasured",
                &source,
                &raw,
                &resolved,
                item.get("depth").cloned().unwrap_or(Value::Null),
                &out,
                &reason,
                Value::Object(Map::new()),
            ),
        )?;
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn query_maps(
    con: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> Result<Vec<Map<String, Value>>, StoreError> {
    let mut out = Vec::new();
    for_each_row(con, sql, params, &mut |m| {
        out.push(m);
        Ok(true)
    })?;
    Ok(out)
}

/// Stream rows of `sql` as column-name maps one at a time, without buffering all rows;
/// `each` returns `false` to stop early.
pub(crate) fn for_each_row(
    con: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
    each: &mut dyn FnMut(Map<String, Value>) -> Result<bool, StoreError>,
) -> Result<(), StoreError> {
    let mut st = con.prepare(sql)?;
    let cols: Vec<String> = st.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = st.query(params)?;
    while let Some(row) = rows.next()? {
        let mut m = Map::new();
        for (i, name) in cols.iter().enumerate() {
            let v = match row.get_ref(i)? {
                rusqlite::types::ValueRef::Null => Value::Null,
                rusqlite::types::ValueRef::Integer(n) => Value::from(n),
                rusqlite::types::ValueRef::Real(f) => {
                    serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)
                }
                rusqlite::types::ValueRef::Text(t) => {
                    Value::String(String::from_utf8_lossy(t).into_owned())
                }
                rusqlite::types::ValueRef::Blob(_) => Value::Null,
            };
            m.insert(name.clone(), v);
        }
        if !each(m)? {
            break;
        }
    }
    Ok(())
}

pub fn read(con: &Connection, limit: i64, offset: i64) -> Result<Map<String, Value>, StoreError> {
    if !(1..=10_000).contains(&limit) || offset < 0 {
        return Err(invalid("limit must be 1..10000 and offset nonnegative"));
    }
    let names: BTreeSet<String> =
        query_maps(con, "SELECT name FROM sqlite_master WHERE type='table'", [])?
            .into_iter()
            .filter_map(|m| m.get("name").and_then(|v| v.as_str()).map(String::from))
            .collect();
    let mut out = Map::new();
    if !names.contains("discovery_occurrences") {
        out.insert("schema_version".into(), Value::from(VERSION));
        out.insert("state".into(), Value::from("unmeasured"));
        out.insert(
            "reason".into(),
            Value::from("discovery ledger is unavailable in this scan format"),
        );
        out.insert("items".into(), Value::Array(vec![]));
        return Ok(out);
    }
    let coverage = if names.contains("discovery_ledger_coverage") {
        query_maps(con, "SELECT * FROM discovery_ledger_coverage", [])?
    } else {
        Vec::new()
    };
    let rows = query_maps(
        con,
        "SELECT * FROM discovery_occurrences ORDER BY occurrence_key LIMIT ?1 OFFSET ?2",
        rusqlite::params![limit + 1, offset],
    )?;
    let truncated = rows.len() as i64 > limit;
    let mut items = Vec::new();
    for mut row in rows.into_iter().take(limit as usize) {
        let observed = row.remove("outcome").unwrap_or(Value::Null);
        let resolved = py_text(row.get("resolved_value"));
        let (current, current_reason) = outcome(con, &resolved)?;
        row.insert("observed_outcome".into(), observed);
        row.insert("current_outcome".into(), Value::from(current));
        row.insert("current_outcome_reason".into(), Value::from(current_reason));
        items.push(Value::Object(row));
    }
    let partial = truncated
        || coverage
            .iter()
            .any(|r| r.get("state").and_then(|v| v.as_str()) == Some("partial"));
    let (state, reason) = if coverage.is_empty() {
        (
            "unmeasured",
            "no document discovery population was retained".to_string(),
        )
    } else if partial {
        (
            "partial",
            if truncated {
                "page limit reached".to_string()
            } else {
                "one or more document occurrence caps were reached".to_string()
            },
        )
    } else {
        ("complete", String::new())
    };
    out.insert("schema_version".into(), Value::from(VERSION));
    out.insert("state".into(), Value::from(state));
    out.insert("reason".into(), Value::from(reason));
    out.insert(
        "coverage".into(),
        Value::Array(coverage.into_iter().map(Value::Object).collect()),
    );
    out.insert("items".into(), Value::Array(items));
    out.insert("truncated".into(), Value::Bool(truncated));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn link_header_keeps_quoted_comma_and_hreflang_metadata() {
        let mut headers = Map::new();
        headers.insert(
            "link".into(),
            Value::from(
                r#"<alt>; title="one,two"; hreflang=fr; rel=alternate, <next>; rel="next""#,
            ),
        );
        let items = relation_items(
            Some(
                r#"<base href="https://example.test/base/"><link rel="canonical" href="preferred">"#,
            ),
            "https://example.test/page",
            Some(&headers),
        );
        assert!(items
            .iter()
            .any(|(rel, carrier, raw, resolved, attrs)| rel == "canonical"
                && carrier == "link[0]"
                && raw == "preferred"
                && resolved == "https://example.test/base/preferred"
                && attrs.is_empty()));
        assert!(items.iter().any(|(rel, _, _, _, attrs)| rel == "hreflang"
            && attrs.get("hreflang").map(String::as_str) == Some("fr")));
        assert!(items.iter().any(|(rel, ..)| rel == "next"));
    }

    #[test]
    fn split_header_values_ignores_quoted_and_angled_commas() {
        let parts = split_header_values(r#"<a,b>; title="x,y", <c>"#);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].trim(), r#"<a,b>; title="x,y""#);
    }

    #[test]
    fn canonical_json_sorts_keys_and_escapes_non_ascii() {
        let v = json!({"b": 1, "a": "é"});
        assert_eq!(canonical_json(&v), r#"{"a":"\u00e9","b":1}"#);
        assert_eq!(dump(&json!({"a": "é"})), r#"{"a": "é"}"#);
    }

    #[test]
    fn occurrence_retry_is_idempotent_but_mismatch_is_refused() {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(crate::schema_sql::SCAN_V1_SQL).unwrap();
        ensure_schema(&con).unwrap();
        let mut occ = Map::new();
        for (k, v) in [
            ("occurrence_key", json!("document:1:link:0")),
            ("source_kind", json!("html")),
            ("relation", json!("hyperlink")),
            ("source_url_id", json!(1)),
            ("source_document_id", json!(1)),
            ("source_response_id", Value::Null),
            ("representation", json!("static")),
            ("carrier", json!("a[href]")),
            ("raw_value", json!("/x")),
            ("resolved_value", json!("https://example.test/x")),
            ("depth", json!(0)),
            ("outcome", json!("unresolved")),
            ("reason", json!("target URL was not retained in this scan")),
            ("attributes", json!({})),
        ] {
            occ.insert(k.into(), v);
        }
        put(&con, &occ).unwrap();
        put(&con, &occ).unwrap();
        occ.insert("raw_value".into(), json!("/y"));
        assert!(put(&con, &occ).is_err());
    }

    #[test]
    fn invalid_relation_is_refused() {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(crate::schema_sql::SCAN_V1_SQL).unwrap();
        ensure_schema(&con).unwrap();
        let mut occ = Map::new();
        for k in [
            "occurrence_key",
            "source_kind",
            "relation",
            "source_url_id",
            "source_document_id",
            "source_response_id",
            "representation",
            "carrier",
            "raw_value",
            "resolved_value",
            "depth",
            "outcome",
            "reason",
            "attributes",
        ] {
            occ.insert(k.into(), Value::Null);
        }
        occ.insert("relation".into(), json!("bogus"));
        occ.insert("outcome".into(), json!("queued"));
        assert!(put(&con, &occ).is_err());
    }
}

//! Versioned structured-data and language-declaration evidence from one document.

use crate::content_evidence::ContextRows;
use crate::discovery_ledger::{canonical_json, py_dumps};
use crate::error::StoreError;
use crate::scan_artifact::ScanError;
use regex::Regex;
use rusqlite::{params, Connection};
use scraper::{ElementRef, Html, Selector};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use spiderhead_parse::dom::{has_ancestor_named, parse_document};
use spiderhead_parse::schema_org::{check_schema, SchemaSource, MAX_HTML_BYTES, MAX_MARKUP_NODES};
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// Outer envelope version of every context item.
pub const OUTER_VERSION: &str = "scan_context.v1";
/// Context kind of structured-data evidence.
pub const STRUCTURED_KIND: &str = "structured_evidence";
/// Context kind of language evidence.
pub const LANGUAGE_KIND: &str = "language_evidence";
/// Current structured payload version.
pub const STRUCTURED_VERSION: &str = "structured_evidence.v2";
/// Legacy structured payload version, still readable.
pub const STRUCTURED_LEGACY_VERSION: &str = "structured_evidence.v1";
/// Language payload version.
pub const LANGUAGE_VERSION: &str = "language_evidence.v1";
/// Valid representations, sorted for a stable order.
pub const REPRESENTATIONS: &[&str] = &["legacy_fragment", "rendered", "static"];

fn scan_err(message: &str) -> StoreError {
    StoreError::ScanError(ScanError {
        message: message.into(),
    })
}

/// SHA-256 of the compact, key-sorted JSON of a value.
pub fn hash(value: &Value) -> String {
    let digest = Sha256::digest(py_dumps(value, true, false).as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Every object of a JSON value in pre-order, using an explicit stack.
pub fn nodes(value: &Value) -> Vec<Map<String, Value>> {
    let mut stack = vec![value];
    let mut result = Vec::new();
    while let Some(current) = stack.pop() {
        match current {
            Value::Object(map) => {
                result.push(map.clone());
                stack.extend(map.values().rev());
            }
            Value::Array(items) => stack.extend(items.iter().rev()),
            _ => {}
        }
    }
    result
}

fn schema_namespace(value: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?i)^https?://(?:www\.)?schema\.org(?:/|$)").expect("static pattern")
    });
    re.is_match(value.trim())
}

fn inert(el: &ElementRef<'_>) -> bool {
    has_ancestor_named(el, &["template"])
}

fn attr<'a>(el: &'a ElementRef<'_>, name: &str) -> Option<&'a str> {
    el.value().attr(name)
}

/// Recognize Schema.org RDFa properties as retained structured markup.
pub fn has_schema_rdfa(soup: &Html) -> bool {
    static PREFIX: OnceLock<Regex> = OnceLock::new();
    let prefix_re =
        PREFIX.get_or_init(|| Regex::new(r"(\w[\w-]*):\s+(\S+)").expect("static pattern"));
    for node in soup
        .tree
        .nodes()
        .filter_map(ElementRef::wrap)
        .take(MAX_MARKUP_NODES)
    {
        if !["typeof", "property", "rel", "rev"]
            .iter()
            .any(|a| attr(&node, a).is_some())
        {
            continue;
        }
        if inert(&node) {
            continue;
        }
        if attr(&node, "typeof").is_some() {
            return true;
        }
        let mut terms: Vec<&str> = Vec::new();
        for name in ["property", "rel", "rev"] {
            if let Some(value) = attr(&node, name) {
                terms.extend(value.split_whitespace());
            }
        }
        if terms
            .iter()
            .any(|t| t.starts_with("schema:") || schema_namespace(t))
        {
            return true;
        }
        let ancestors: Vec<ElementRef<'_>> = std::iter::once(node)
            .chain(node.ancestors().filter_map(ElementRef::wrap))
            .collect();
        if ancestors
            .iter()
            .any(|a| attr(a, "vocab").is_some_and(schema_namespace))
        {
            return true;
        }
        let prefix_text = ancestors
            .iter()
            .map(|a| attr(a, "prefix").unwrap_or(""))
            .collect::<Vec<_>>()
            .join(" ");
        let mut prefix_map: BTreeMap<&str, &str> = BTreeMap::new();
        for caps in prefix_re.captures_iter(&prefix_text) {
            prefix_map.insert(
                caps.get(1).map_or("", |m| m.as_str()),
                caps.get(2).map_or("", |m| m.as_str()),
            );
        }
        if terms.iter().any(|t| {
            t.split_once(':').is_some_and(|(prefix, _)| {
                schema_namespace(prefix_map.get(prefix).copied().unwrap_or(""))
            })
        }) {
            return true;
        }
    }
    false
}

fn unavailable_validation() -> Map<String, Value> {
    let mut base = Map::new();
    base.insert("syntax_state".into(), json!("unavailable"));
    base.insert("structural_state".into(), json!("unavailable"));
    base.insert("feature_eligibility_state".into(), json!("unknown"));
    base.insert("unsupported_context_normalization".into(), json!(false));
    base.insert("entity_errors".into(), json!(0));
    base.insert("entity_warnings".into(), json!(0));
    base.insert("feature_checks".into(), json!([]));
    base
}

fn count_list(item: &Value, key: &str) -> i64 {
    item.get(key)
        .and_then(Value::as_array)
        .map_or(0, |a| a.len() as i64)
}

fn text_of(item: &Value, key: &str) -> String {
    match item.get(key) {
        Some(Value::String(s)) => s.clone(),
        None | Some(Value::Null) => String::new(),
        Some(other) => other.to_string(),
    }
}

/// Run only bundled, offline graph checks; do not promise Google display.
/// `base_url` defaults to unset.
pub fn validation(
    html: Option<&str>,
    base_url: Option<&str>,
) -> Result<Map<String, Value>, StoreError> {
    let Some(html) = html else {
        return Ok(unavailable_validation());
    };
    let checked = check_schema(SchemaSource::Html(html), None, 0.0, base_url)
        .map_err(|e| StoreError::Invalid(e.to_string()))?;
    let parse_errors = checked.parse_errors.unwrap_or_default();
    let entities = checked.entities.unwrap_or_default();
    let vocabularies = checked.vocabularies.unwrap_or_default();
    let unsupported = vocabularies.iter().any(|v| !v.supported);
    let partial_markup = checked
        .markup_coverage
        .unwrap_or_default()
        .iter()
        .any(|m| m.status == "partial");
    let mut entity_errors = 0;
    let mut entity_warnings = 0;
    for item in entities.iter().filter(|i| i.is_object()) {
        entity_errors += count_list(item, "errors");
        entity_warnings += count_list(item, "warnings");
    }
    let mut feature_checks = Vec::new();
    for item in checked
        .rich_results
        .unwrap_or_default()
        .iter()
        .filter(|i| i.is_object())
    {
        let eligible = item.get("eligible").is_some_and(|v| match v {
            Value::Bool(b) => *b,
            Value::Null => false,
            Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
            Value::String(s) => !s.is_empty(),
            Value::Array(a) => !a.is_empty(),
            Value::Object(o) => !o.is_empty(),
        });
        feature_checks.push(json!({
            "type": text_of(item, "type"),
            "feature": text_of(item, "google_feature"),
            "local_requirements_state": if eligible { "met" } else { "missing_required_properties" },
            "missing_required": item.get("missing_required").cloned().unwrap_or(json!([])),
            "missing_required_any_of": item.get("missing_required_any_of").cloned().unwrap_or(json!([])),
            "rules_version": text_of(item, "rules_version"),
        }));
    }
    let structural_state = if !parse_errors.is_empty() {
        "malformed"
    } else if unsupported {
        "unsupported_context"
    } else if partial_markup {
        "unavailable"
    } else if entities.is_empty() || entity_errors > 0 {
        "structurally_invalid"
    } else {
        "valid"
    };
    let mut out = Map::new();
    out.insert(
        "syntax_state".into(),
        json!(if parse_errors.is_empty() {
            "parsed"
        } else {
            "malformed"
        }),
    );
    out.insert("structural_state".into(), json!(structural_state));
    out.insert(
        "feature_eligibility_state".into(),
        json!(if feature_checks.is_empty() {
            "unknown"
        } else {
            "locally_checked"
        }),
    );
    out.insert(
        "unsupported_context_normalization".into(),
        json!(unsupported),
    );
    out.insert("entity_errors".into(), json!(entity_errors));
    out.insert("entity_warnings".into(), json!(entity_warnings));
    out.insert("feature_checks".into(), Value::Array(feature_checks));
    Ok(out)
}

fn check_identity(
    page_url_id: i64,
    source_document_id: i64,
    representation: &str,
    what: &str,
) -> Result<(), StoreError> {
    let _ = (page_url_id, source_document_id);
    if !REPRESENTATIONS.contains(&representation) {
        return Err(StoreError::Invalid(format!("{what} identity is invalid")));
    }
    Ok(())
}

fn envelope(
    version: &str,
    page_url_id: i64,
    source_document_id: i64,
    representation: &str,
) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("schema_version".into(), json!(version));
    out.insert("page_url_id".into(), json!(page_url_id));
    out.insert("source_document_id".into(), json!(source_document_id));
    out.insert("representation".into(), json!(representation));
    out
}

fn list_of(parsed: &Map<String, Value>, key: &str) -> Vec<Value> {
    parsed
        .get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Capture syntax, graph and local feature-check states without body excerpts.
/// `html` defaults to unset.
pub fn structured_payload(
    page_url_id: i64,
    source_document_id: i64,
    representation: &str,
    parsed: Option<&Map<String, Value>>,
    html: Option<&str>,
) -> Result<Map<String, Value>, StoreError> {
    check_identity(
        page_url_id,
        source_document_id,
        representation,
        "structured evidence",
    )?;
    let mut out = envelope(
        STRUCTURED_VERSION,
        page_url_id,
        source_document_id,
        representation,
    );
    let Some(parsed) = parsed else {
        out.insert("state".into(), json!("unavailable"));
        out.insert("reason".into(), json!("captured document was not parsed"));
        out.insert("blocks".into(), json!([]));
        out.insert("nodes".into(), json!([]));
        out.insert("edges".into(), json!([]));
        out.insert("validation".into(), Value::Object(unavailable_validation()));
        return Ok(out);
    };
    let valid = list_of(parsed, "jsonld");
    let invalid = list_of(parsed, "jsonld_invalid");
    let (mut blocks, mut node_rows, mut edges) = (Vec::new(), Vec::new(), Vec::new());
    for (ordinal, block) in valid.iter().enumerate() {
        blocks.push(json!({
            "ordinal": ordinal, "parser_ordinal": ordinal, "state": "parsed", "hash": hash(block),
        }));
        for (node_ordinal, node) in nodes(block).iter().enumerate() {
            let node_id = node.get("@id").and_then(Value::as_str);
            let types = match node.get("@type") {
                Some(Value::String(t)) => vec![t.clone()],
                Some(Value::Array(items)) => {
                    let mut t: Vec<String> = items
                        .iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect();
                    t.sort();
                    t
                }
                _ => Vec::new(),
            };
            node_rows.push(json!({
                "block_ordinal": ordinal, "ordinal": node_ordinal, "id": node_id, "types": types,
            }));
            for (property, value) in node {
                let values: Vec<&Value> = match value {
                    Value::Array(items) => items.iter().collect(),
                    other => vec![other],
                };
                for target in values {
                    if let Some(target_id) = target
                        .as_object()
                        .and_then(|t| t.get("@id"))
                        .and_then(Value::as_str)
                    {
                        edges.push(json!({
                            "block_ordinal": ordinal, "source_id": node_id,
                            "property": property, "target_id": target_id,
                        }));
                    }
                }
            }
        }
    }
    for (offset, item) in invalid.iter().enumerate() {
        let source_ordinal = item
            .as_object()
            .and_then(|o| o.get("index"))
            .and_then(Value::as_i64);
        blocks.push(json!({
            "ordinal": valid.len() + offset, "parser_ordinal": source_ordinal,
            "state": "malformed", "hash": hash(item),
        }));
    }
    let base = parsed.get("document_base_url").and_then(Value::as_str);
    let mut checked = validation(html, base)?;
    if !invalid.is_empty() {
        checked.insert("syntax_state".into(), json!("malformed"));
    }
    // Non-JSON-LD formats are validated by the same bundled vocabulary; they must not read
    // as absent merely because the JSON-LD block list is empty.
    let mut non_json_markup = false;
    if let Some(html) = html.filter(|h| h.len() <= MAX_HTML_BYTES) {
        let soup = parse_document(html);
        let scope = Selector::parse("*[itemscope], *[typeof]").expect("static selector");
        non_json_markup = soup.select(&scope).any(|tag| !inert(&tag)) || has_schema_rdfa(&soup);
    }
    let structural = checked["structural_state"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let state = if !invalid.is_empty() {
        "malformed".to_string()
    } else if matches!(structural.as_str(), "unavailable" | "unsupported_context") {
        structural.clone()
    } else if valid.is_empty() && !non_json_markup {
        "absent".to_string()
    } else {
        structural.clone()
    };
    let reason = match state.as_str() {
        "malformed" => "JSON-LD syntax failures captured",
        "structurally_invalid" => "Schema.org graph validation found structural errors",
        "unsupported_context" => {
            "one or more JSON-LD contexts could not be normalized as Schema.org"
        }
        "unavailable" if html.is_some() => {
            "one or more markup constructs require unsupported structural normalization"
        }
        "unavailable" => "captured HTML was unavailable for structural validation",
        "absent" | "valid" => "",
        _ => "structural validation is unavailable",
    };
    out.insert("state".into(), json!(state));
    out.insert("reason".into(), json!(reason));
    out.insert("blocks".into(), Value::Array(blocks));
    out.insert("nodes".into(), Value::Array(node_rows));
    out.insert("edges".into(), Value::Array(edges));
    out.insert("validation".into(), Value::Object(checked));
    Ok(out)
}

/// Capture raw/resolved hreflang declarations as directed source evidence.
pub fn language_payload(
    page_url_id: i64,
    source_document_id: i64,
    representation: &str,
    parsed: Option<&Map<String, Value>>,
    html: Option<&str>,
) -> Result<Map<String, Value>, StoreError> {
    check_identity(
        page_url_id,
        source_document_id,
        representation,
        "language evidence",
    )?;
    let mut out = envelope(
        LANGUAGE_VERSION,
        page_url_id,
        source_document_id,
        representation,
    );
    let Some(parsed) = parsed else {
        out.insert("state".into(), json!("unavailable"));
        out.insert("reason".into(), json!("captured document was not parsed"));
        out.insert("html_lang".into(), Value::Null);
        out.insert("declarations".into(), json!([]));
        return Ok(out);
    };
    let mut declarations = Vec::new();
    for (ordinal, alternate) in list_of(parsed, "hreflang").iter().enumerate() {
        if !alternate.is_object() {
            continue;
        }
        let target = text_of(alternate, "url");
        declarations.push(json!({
            "ordinal": ordinal,
            "lang": text_of(alternate, "lang"),
            "raw_href": text_of(alternate, "raw_href"),
            "state": if alternate.get("url").and_then(Value::as_str).is_some_and(|u| !u.is_empty()) { "declared" } else { "malformed" },
            "target": target,
        }));
    }
    let html_lang = html.and_then(|h| {
        let soup = parse_document(h);
        let selector = Selector::parse("html").expect("static selector");
        soup.select(&selector)
            .next()
            .map(|el| attr(&el, "lang").unwrap_or("").to_string())
    });
    out.insert(
        "state".into(),
        json!(if declarations.is_empty() {
            "absent"
        } else {
            "declared"
        }),
    );
    out.insert("reason".into(), json!(""));
    out.insert("html_lang".into(), json!(html_lang));
    out.insert("declarations".into(), Value::Array(declarations));
    Ok(out)
}

/// Return closed context envelopes for the existing atomic writer hook.
pub fn context_items(
    structured: &Map<String, Value>,
    language: &Map<String, Value>,
) -> Vec<Map<String, Value>> {
    vec![
        item(STRUCTURED_KIND, structured),
        item(LANGUAGE_KIND, language),
    ]
}

fn item_key(payload: &Map<String, Value>) -> String {
    let get = |k: &str| match payload.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    };
    format!(
        "page:{}:document:{}:representation:{}",
        get("page_url_id"),
        get("source_document_id"),
        get("representation")
    )
}

/// One `context_items` envelope around a payload.
pub fn item(kind: &str, payload: &Map<String, Value>) -> Map<String, Value> {
    let unavailable = payload.get("state") == Some(&json!("unavailable"));
    let mut out = Map::new();
    out.insert("kind".into(), json!(kind));
    out.insert("item_key".into(), json!(item_key(payload)));
    out.insert("payload_version".into(), json!(OUTER_VERSION));
    out.insert(
        "payload_json".into(),
        json!(canonical_json(&Value::Object(payload.clone()))),
    );
    out.insert(
        "completeness".into(),
        json!(if unavailable {
            "unavailable"
        } else {
            "complete"
        }),
    );
    out.insert(
        "reason".into(),
        payload.get("reason").cloned().unwrap_or(Value::Null),
    );
    out
}

const VALIDATION_KEYS: [&str; 7] = [
    "syntax_state",
    "structural_state",
    "feature_eligibility_state",
    "unsupported_context_normalization",
    "entity_errors",
    "entity_warnings",
    "feature_checks",
];

fn is_int(value: Option<&Value>) -> bool {
    value.is_some_and(|v| v.is_i64() || v.is_u64())
}

/// Validate closed payload shape and exact document/page/representation binding.
pub fn validate_context(
    con: &Connection,
    item: &Map<String, Value>,
    payload: &Value,
) -> Result<(), StoreError> {
    let kind = item.get("kind").and_then(Value::as_str);
    let version = match kind {
        Some(STRUCTURED_KIND) => Some(STRUCTURED_VERSION),
        Some(LANGUAGE_KIND) => Some(LANGUAGE_VERSION),
        _ => None,
    };
    let invalid = || scan_err("structured or language evidence context is invalid");
    let (Some(version), Some(payload)) = (version, payload.as_object()) else {
        return Err(invalid());
    };
    if item.get("payload_version") != Some(&json!(OUTER_VERSION)) {
        return Err(invalid());
    }
    let structured = kind == Some(STRUCTURED_KIND);
    let mut required: Vec<&str> = vec![
        "schema_version",
        "page_url_id",
        "source_document_id",
        "representation",
        "state",
        "reason",
    ];
    if structured {
        required.extend(["blocks", "nodes", "edges"]);
        match payload.get("schema_version").and_then(Value::as_str) {
            Some(STRUCTURED_VERSION) => required.push("validation"),
            Some(STRUCTURED_LEGACY_VERSION) => {}
            _ => {
                return Err(scan_err(
                    "structured evidence payload has unsupported version",
                ))
            }
        }
    } else {
        required.extend(["html_lang", "declarations"]);
    }
    let same_keys =
        payload.len() == required.len() && required.iter().all(|k| payload.contains_key(*k));
    if !same_keys || (!structured && payload["schema_version"] != json!(version)) {
        return Err(scan_err(
            "structured or language evidence payload has unsupported fields",
        ));
    }
    let representation = payload["representation"].as_str().unwrap_or("");
    if !is_int(payload.get("page_url_id"))
        || !is_int(payload.get("source_document_id"))
        || !REPRESENTATIONS.contains(&representation)
    {
        return Err(scan_err(
            "structured or language evidence identity is invalid",
        ));
    }
    let state_ok = payload["state"].as_str().is_some_and(|s| {
        matches!(
            s,
            "eligible"
                | "valid"
                | "malformed"
                | "structurally_invalid"
                | "unsupported_context"
                | "absent"
                | "declared"
                | "unavailable"
        )
    });
    if !state_ok || !payload["reason"].is_string() {
        return Err(scan_err("structured or language evidence state is invalid"));
    }
    if structured && payload["schema_version"] == json!(STRUCTURED_VERSION) {
        let bad = || scan_err("structured evidence validation state is invalid");
        let v = payload["validation"].as_object().ok_or_else(bad)?;
        let in_set = |key: &str, allowed: &[&str]| {
            v.get(key)
                .and_then(Value::as_str)
                .is_some_and(|s| allowed.contains(&s))
        };
        if v.len() != VALIDATION_KEYS.len()
            || !VALIDATION_KEYS.iter().all(|k| v.contains_key(*k))
            || !in_set("syntax_state", &["parsed", "malformed", "unavailable"])
            || !in_set(
                "structural_state",
                &[
                    "valid",
                    "malformed",
                    "structurally_invalid",
                    "unsupported_context",
                    "unavailable",
                ],
            )
            || !in_set("feature_eligibility_state", &["locally_checked", "unknown"])
            || !v["unsupported_context_normalization"].is_boolean()
            || !is_int(v.get("entity_errors"))
            || !is_int(v.get("entity_warnings"))
            || !v["feature_checks"].is_array()
        {
            return Err(bad());
        }
    }
    if item.get("item_key").and_then(Value::as_str) != Some(item_key(payload).as_str()) {
        return Err(scan_err("structured or language evidence key is invalid"));
    }
    let bound = con
        .query_row(
            "SELECT 1 FROM documents WHERE document_id=? AND url_id=? AND representation=?",
            params![
                payload["source_document_id"].as_i64(),
                payload["page_url_id"].as_i64(),
                representation
            ],
            |_| Ok(()),
        )
        .is_ok();
    if !bound {
        return Err(scan_err(
            "structured or language evidence binds the wrong document",
        ));
    }
    Ok(())
}

/// Typed evidence as bounded re-iterable retained collections: `(structured, language)`.
pub fn read_rows(con: &Connection) -> (ContextRows<'_>, ContextRows<'_>) {
    (
        ContextRows::new(con, STRUCTURED_KIND, false),
        ContextRows::new(con, LANGUAGE_KIND, false),
    )
}

/// Read typed evidence as lists (`{"structured": [...], "language": [...]}`).
pub fn read(con: &Connection) -> Result<Map<String, Value>, StoreError> {
    let (structured, language) = read_rows(con);
    let mut out = Map::new();
    out.insert("structured".into(), Value::Array(structured.rows()?));
    out.insert("language".into(), Value::Array(language.rows()?));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(parsed: Option<Value>, html: Option<&str>) -> Map<String, Value> {
        let map = parsed.map(|v| v.as_object().cloned().unwrap_or_default());
        structured_payload(1, 2, "static", map.as_ref(), html).unwrap()
    }

    #[test]
    fn unparsed_document_is_unavailable() {
        let p = payload(None, None);
        assert_eq!(p["state"], json!("unavailable"));
        assert_eq!(p["validation"]["syntax_state"], json!("unavailable"));
        let item = item(STRUCTURED_KIND, &p);
        assert_eq!(item["completeness"], json!("unavailable"));
        assert_eq!(
            item["item_key"],
            json!("page:1:document:2:representation:static")
        );
    }

    #[test]
    fn graph_nodes_and_edges() {
        let block = json!({"@id": "a", "@type": ["Z", "A"], "author": {"@id": "b"}, "x": [{"@id": "c"}, 1]});
        let found = nodes(&block);
        assert_eq!(found.len(), 3);
        let p = structured_payload(
            1,
            2,
            "static",
            json!({"jsonld": [block], "jsonld_invalid": [{"index": 3}]}).as_object(),
            None,
        )
        .unwrap();
        assert_eq!(p["blocks"][0]["state"], json!("parsed"));
        assert_eq!(p["blocks"][1]["parser_ordinal"], json!(3));
        assert_eq!(p["nodes"][0]["types"], json!(["A", "Z"]));
        assert_eq!(p["edges"].as_array().unwrap().len(), 2);
        assert_eq!(p["state"], json!("malformed"));
        assert_eq!(p["validation"]["syntax_state"], json!("malformed"));
    }

    #[test]
    fn hash_ignores_key_order() {
        assert_eq!(
            hash(&json!({"a": 1, "b": 2})),
            hash(&json!({"b": 2, "a": 1}))
        );
        assert_ne!(hash(&json!({"a": 1})), hash(&json!({"a": 2})));
    }

    #[test]
    fn rdfa_detection() {
        let yes = [
            r#"<div typeof="x">a</div>"#,
            r#"<div vocab="https://schema.org/"><span property="name">a</span></div>"#,
            r#"<span property="schema:name">a</span>"#,
            r#"<div prefix="s: http://schema.org/"><span property="s:name">a</span></div>"#,
        ];
        for html in yes {
            assert!(has_schema_rdfa(&parse_document(html)), "{html}");
        }
        let no = [
            r#"<span property="og:title">a</span>"#,
            r#"<template><div typeof="x"></div></template>"#,
            r#"<a rel="nofollow">a</a>"#,
        ];
        for html in no {
            assert!(!has_schema_rdfa(&parse_document(html)), "{html}");
        }
    }

    #[test]
    fn language_declarations() {
        let parsed = json!({"hreflang": [
            {"lang": "de", "raw_href": "/de", "url": "https://e.test/de"},
            {"lang": "fr", "raw_href": "/fr", "url": ""},
            "junk",
        ]});
        let p = language_payload(
            1,
            2,
            "static",
            parsed.as_object(),
            Some("<html lang=\"en\"></html>"),
        )
        .unwrap();
        assert_eq!(p["state"], json!("declared"));
        assert_eq!(p["html_lang"], json!("en"));
        assert_eq!(p["declarations"][0]["state"], json!("declared"));
        assert_eq!(p["declarations"][1]["state"], json!("malformed"));
        assert_eq!(p["declarations"].as_array().unwrap().len(), 2);
        let none = language_payload(1, 2, "static", json!({}).as_object(), None).unwrap();
        assert_eq!(none["state"], json!("absent"));
        assert_eq!(none["html_lang"], Value::Null);
        assert!(language_payload(1, 2, "weird", None, None).is_err());
    }

    fn documents() -> Connection {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(
            "CREATE TABLE documents(document_id INTEGER,url_id INTEGER,representation TEXT);
             INSERT INTO documents VALUES(2,1,'static');",
        )
        .unwrap();
        con
    }

    #[test]
    fn validate_context_checks_shape_and_binding() {
        let con = documents();
        let p = language_payload(1, 2, "static", json!({}).as_object(), None).unwrap();
        let envelope = item(LANGUAGE_KIND, &p);
        validate_context(&con, &envelope, &Value::Object(p.clone())).unwrap();
        let mut wrong = p.clone();
        wrong.insert("source_document_id".into(), json!(9));
        let wrong_item = item(LANGUAGE_KIND, &wrong);
        assert!(validate_context(&con, &wrong_item, &Value::Object(wrong)).is_err());
        let mut extra = p.clone();
        extra.insert("x".into(), json!(1));
        assert!(validate_context(&con, &envelope, &Value::Object(extra)).is_err());
        let mut bad_state = p;
        bad_state.insert("state".into(), json!("nope"));
        assert!(validate_context(&con, &envelope, &Value::Object(bad_state)).is_err());
        let unknown = item("other", &Map::new());
        assert!(validate_context(&con, &unknown, &json!({})).is_err());
    }

    #[test]
    fn structured_validation_block_is_closed() {
        let con = documents();
        let p = payload(None, None);
        let envelope = item(STRUCTURED_KIND, &p);
        validate_context(&con, &envelope, &Value::Object(p.clone())).unwrap();
        let mut broken = p;
        broken["validation"]["entity_errors"] = json!("x");
        assert!(validate_context(&con, &envelope, &Value::Object(broken)).is_err());
    }
}

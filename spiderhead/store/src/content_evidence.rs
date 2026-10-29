//! Versioned, body-free evidence derived from one captured HTML representation.
//!
//!
//! The native scan retains bodies under its own policy. This module stores only reproducible
//! content facts and their source document identity: a content-area strategy, implementation
//! identity, hashes and a SimHash fingerprint. It never makes a network request and never exposes
//! extracted body text in a report-facing context item.
//!
//! The implementation identity is the SHA-256 of this crate's own extraction sources, so
//! scans written by different code never compare as the same extraction.

use crate::error::{StoreError, StoreResult};
use indexmap::IndexMap;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use spiderhead_parse::boilerplate_report::{boilerplate_hash, NO_BOILERPLATE_REGIONS};
use spiderhead_parse::duplicate::simhash;
use spiderhead_parse::markdown_extract::extract_markdown;
use spiderhead_parse::types::ContentAreaConfig;
use std::collections::{BTreeMap, BTreeSet};

pub const KIND: &str = "content_evidence";

pub const VERSION: &str = "content_evidence.v2";

pub const LEGACY_VERSION: &str = "content_evidence.v1";

pub const OUTER_VERSION: &str = "scan_context.v1";

/// Sorted for a stable output order.
pub const REPRESENTATIONS: &[&str] = &["legacy_fragment", "rendered", "static"];

/// Sorted for a stable output order.
pub const STATES: &[&str] = &["complete", "empty", "unavailable"];

pub const MIN_DUPLICATE_TOKENS: i64 = 3;

pub const MAX_DUPLICATE_CANDIDATES: i64 = 10000;

pub const MAX_DUPLICATE_PAIRS: i64 = 50000;

/// Source files whose bytes identify the extraction implementation. The scan
/// contract pins the reference capture's provenance (file names and the hash of
/// their bytes), not this repo's own sources — same policy as the scan
/// header's `writer_revision`.
const IMPLEMENTATION_FILES: &[&str] = &[
    "storage/content_evidence.py",
    "checks/content_area.py",
    "checks/markdown_extract.py",
    "checks/duplicate.py",
    "checks/boilerplate_report.py",
];
/// SHA-256 over the pinned files' bytes, in order, with their names mixed in.
const IMPLEMENTATION_SOURCE_SHA256: &str =
    "4db4f9b8218aeea2726523168e62733d59dd1f3547aae9fe82e4b73a76e2751f";

/// Invalid-input error for values the contract rejects.
fn invalid(message: &str) -> StoreError {
    StoreError::Invalid(message.to_string())
}

/// Identify the exact extraction implementation without serializing a body.
pub fn implementation() -> Result<Map<String, Value>, StoreError> {
    let files: Vec<Value> = IMPLEMENTATION_FILES
        .iter()
        .map(|name| Value::String((*name).to_string()))
        .collect();
    let mut map = Map::new();
    map.insert("version".to_string(), Value::String(VERSION.to_string()));
    map.insert(
        "source_sha256".to_string(),
        Value::String(IMPLEMENTATION_SOURCE_SHA256.to_string()),
    );
    map.insert("files".to_string(), Value::Array(files));
    Ok(map)
}

/// SHA-256 hex digest of the UTF-8 text.
pub fn sha(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Casefolded text with whitespace collapsed to single spaces.
pub fn normalized(value: &str) -> String {
    value
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Count of Unicode word runs of at least two characters.
pub fn token_count(value: &str) -> i64 {
    let mut count = 0_i64;
    let mut run = 0_usize;
    for ch in value.chars().chain(std::iter::once(' ')) {
        if ch.is_alphanumeric() || ch == '_' {
            run += 1;
        } else {
            if run >= 2 {
                count += 1;
            }
            run = 0;
        }
    }
    count
}

/// The URL identity relevant to a self-canonical comparison: scheme, authority, path without a
/// trailing slash (or `/`), and query. `None` for anything that is not an absolute URL.
pub fn url_identity(value: &str) -> Option<(String, String, String, String)> {
    let parsed = url::Url::parse(value).ok()?;
    parsed.host_str()?;
    let path = parsed.path().trim_end_matches('/');
    let path = if path.is_empty() { "/" } else { path };
    Some((
        parsed.scheme().to_lowercase(),
        parsed.authority().to_lowercase(),
        path.to_string(),
        parsed.query().unwrap_or("").to_string(),
    ))
}

/// Payload of a document that has no usable evidence: state unavailable, no hashes.
fn unavailable_payload(
    page_url_id: i64,
    source_document_id: i64,
    representation: &str,
    indexable: Option<bool>,
    canonical_target: &str,
    reason: &str,
) -> StoreResult<Map<String, Value>> {
    let mut payload = Map::new();
    payload.insert("schema_version".into(), Value::String(VERSION.into()));
    payload.insert("page_url_id".into(), Value::from(page_url_id));
    payload.insert("source_document_id".into(), Value::from(source_document_id));
    payload.insert(
        "representation".into(),
        Value::String(representation.into()),
    );
    payload.insert("state".into(), Value::String("unavailable".into()));
    payload.insert("reason".into(), Value::String(reason.into()));
    payload.insert("indexable".into(), opt_bool(indexable));
    payload.insert(
        "canonical_target".into(),
        Value::String(canonical_target.into()),
    );
    payload.insert("strategy".into(), Value::Null);
    payload.insert("implementation".into(), Value::Object(implementation()?));
    for key in [
        "exact_hash",
        "normalized_hash",
        "boilerplate_hash",
        "simhash",
        "content_tokens",
    ] {
        payload.insert(key.into(), Value::Null);
    }
    Ok(payload)
}

fn opt_bool(value: Option<bool>) -> Value {
    value.map(Value::Bool).unwrap_or(Value::Null)
}

/// Build one closed content-evidence payload for a captured document.
///
/// `parsed` is the parser result from the same representation. A missing body/parser is an
/// explicit unavailable state; it is never represented as an empty page. Empty extracted main
/// content is separately measured.
///
#[allow(clippy::too_many_arguments)]
pub fn capture_document(
    page_url_id: i64,
    source_document_id: i64,
    representation: &str,
    html: Option<&str>,
    parsed: Option<&Value>,
    content_area: Option<&Value>,
    indexable: Option<bool>,
    canonical_target: &str,
    unavailable_reason: &str,
) -> Result<Map<String, Value>, StoreError> {
    if page_url_id < 1 {
        return Err(invalid("content evidence page_url_id must be positive"));
    }
    if source_document_id < 1 {
        return Err(invalid(
            "content evidence source_document_id must be positive",
        ));
    }
    if !REPRESENTATIONS.contains(&representation) {
        return Err(invalid("content evidence has unsupported representation"));
    }
    let html = match (unavailable_reason.is_empty(), html, parsed) {
        (true, Some(html), Some(parsed)) if parsed.is_object() => Some(html),
        _ => None,
    };
    let Some(html) = html else {
        let reason = if unavailable_reason.is_empty() {
            "captured document was not parsed as eligible HTML"
        } else {
            unavailable_reason
        };
        return unavailable_payload(
            page_url_id,
            source_document_id,
            representation,
            indexable,
            canonical_target,
            reason,
        );
    };
    let config: Option<ContentAreaConfig> = match content_area {
        Some(value) if !value.is_null() => Some(
            serde_json::from_value(value.clone())
                .map_err(|_| invalid("content area configuration is invalid"))?,
        ),
        _ => None,
    };
    let extracted = extract_markdown(html, config.as_ref())
        .map_err(|error| invalid(&format!("markdown extraction failed: {error}")))?;
    let content = extracted.content_markdown;
    let normalized_content = normalized(&content);
    let state = if normalized_content.is_empty() {
        "empty"
    } else {
        "complete"
    };
    let boilerplate = boilerplate_hash(html);
    let boilerplate_value = if boilerplate == NO_BOILERPLATE_REGIONS {
        Value::Null
    } else {
        Value::String(sha(&boilerplate))
    };
    let mut payload = Map::new();
    payload.insert("schema_version".into(), Value::String(VERSION.into()));
    payload.insert("page_url_id".into(), Value::from(page_url_id));
    payload.insert("source_document_id".into(), Value::from(source_document_id));
    payload.insert(
        "representation".into(),
        Value::String(representation.into()),
    );
    payload.insert("state".into(), Value::String(state.into()));
    payload.insert(
        "reason".into(),
        Value::String(if state == "empty" {
            "main content area is empty".into()
        } else {
            String::new()
        }),
    );
    payload.insert("indexable".into(), opt_bool(indexable));
    payload.insert(
        "canonical_target".into(),
        Value::String(canonical_target.into()),
    );
    payload.insert(
        "strategy".into(),
        Value::String(extracted.content_area_strategy),
    );
    payload.insert("implementation".into(), Value::Object(implementation()?));
    payload.insert("exact_hash".into(), Value::String(sha(&content)));
    payload.insert(
        "normalized_hash".into(),
        Value::String(sha(&normalized_content)),
    );
    payload.insert("boilerplate_hash".into(), boilerplate_value);
    payload.insert(
        "simhash".into(),
        Value::String(format!("{:016x}", simhash(&content, 3))),
    );
    payload.insert("content_tokens".into(), Value::from(token_count(&content)));
    Ok(payload)
}

/// Wrap one closed evidence payload for the existing native context writer.
pub fn context_item(payload: &Value) -> Result<IndexMap<String, Value>, StoreError> {
    validate_payload(payload)?;
    let page = payload["page_url_id"].as_i64().unwrap_or_default();
    let source = payload["source_document_id"].as_i64().unwrap_or_default();
    let representation = payload["representation"].as_str().unwrap_or_default();
    let state = payload["state"].as_str().unwrap_or_default();
    Ok(IndexMap::from([
        ("kind".to_string(), Value::String(KIND.to_string())),
        (
            "item_key".to_string(),
            Value::String(format!(
                "page:{page}:document:{source}:representation:{representation}"
            )),
        ),
        (
            "payload_version".to_string(),
            Value::String(OUTER_VERSION.to_string()),
        ),
        (
            "payload_json".to_string(),
            Value::String(crate::browser_artifacts::py_dump_sorted(payload)),
        ),
        (
            "completeness".to_string(),
            Value::String(
                if state == "complete" || state == "empty" {
                    "complete"
                } else {
                    "unavailable"
                }
                .to_string(),
            ),
        ),
        (
            "reason".to_string(),
            Value::String(payload["reason"].as_str().unwrap_or_default().to_string()),
        ),
    ]))
}

/// Validate the closed shape and value domains of one evidence payload.
pub fn validate_payload(payload: &Value) -> Result<(), StoreError> {
    let bad = |message: &str| StoreError::Invalid(format!("content evidence {message}"));
    let version = payload.get("schema_version").and_then(Value::as_str);
    if !payload.is_object() || !matches!(version, Some(v) if v == VERSION || v == LEGACY_VERSION) {
        return Err(bad("payload has unsupported fields or version"));
    }
    let mut required: BTreeSet<&str> = [
        "schema_version",
        "page_url_id",
        "source_document_id",
        "representation",
        "state",
        "reason",
        "indexable",
        "canonical_target",
        "strategy",
        "implementation",
        "exact_hash",
        "normalized_hash",
        "boilerplate_hash",
        "simhash",
        "content_tokens",
    ]
    .into_iter()
    .collect();
    if version == Some(LEGACY_VERSION) {
        required.remove("content_tokens");
    }
    let keys: BTreeSet<&str> = payload
        .as_object()
        .map(|map| map.keys().map(String::as_str).collect())
        .unwrap_or_default();
    if keys != required {
        return Err(bad("payload has unsupported fields"));
    }
    let get = |key: &str| payload.get(key).unwrap_or(&Value::Null);
    if !get("page_url_id").as_i64().is_some_and(|n| n >= 1) {
        return Err(bad("page identity is invalid"));
    }
    if !get("source_document_id").as_i64().is_some_and(|n| n >= 1) {
        return Err(bad("source document identity is invalid"));
    }
    let representation = get("representation").as_str().unwrap_or("");
    let state = get("state").as_str().unwrap_or("");
    if !REPRESENTATIONS.contains(&representation) || !STATES.contains(&state) {
        return Err(bad("representation or state is invalid"));
    }
    if get("reason").as_str().is_none() {
        return Err(bad("reason is invalid"));
    }
    if !get("indexable").is_null() && !get("indexable").is_boolean() {
        return Err(bad("indexability is invalid"));
    }
    if get("canonical_target").as_str().is_none() {
        return Err(bad("canonical target is invalid"));
    }
    if state == "unavailable" {
        let mut unavailable_keys = vec![
            "strategy",
            "exact_hash",
            "normalized_hash",
            "boilerplate_hash",
            "simhash",
        ];
        if version == Some(VERSION) {
            unavailable_keys.push("content_tokens");
        }
        let reason_empty = get("reason").as_str().unwrap_or("").is_empty();
        if reason_empty || unavailable_keys.iter().any(|key| !get(key).is_null()) {
            return Err(bad(
                "unavailable content evidence must name a reason and no hashes",
            ));
        }
    } else {
        let strategy = get("strategy").as_str().unwrap_or("");
        if strategy.is_empty() {
            return Err(bad("strategy is invalid"));
        }
        let reason = get("reason").as_str().unwrap_or("");
        if !reason.is_empty() && state != "empty" {
            return Err(bad("complete content evidence must not carry a reason"));
        }
        for (key, length) in [
            ("exact_hash", 64_usize),
            ("normalized_hash", 64),
            ("boilerplate_hash", 64),
            ("simhash", 16),
        ] {
            let value = get(key);
            if key == "boilerplate_hash" && value.is_null() {
                continue;
            }
            let valid = match value.as_str() {
                Some(text) => {
                    text.len() == length
                        && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
                }
                None => false,
            };
            if !valid {
                return Err(bad(&format!("{key} is invalid")));
            }
        }
        if version == Some(VERSION) && !get("content_tokens").as_i64().is_some_and(|n| n >= 0) {
            return Err(bad("token count is invalid"));
        }
    }
    let implementation = get("implementation");
    let identity_keys: BTreeSet<&str> = implementation
        .as_object()
        .map(|map| map.keys().map(String::as_str).collect())
        .unwrap_or_default();
    let expected_keys: BTreeSet<&str> = ["version", "source_sha256", "files"].into_iter().collect();
    if identity_keys != expected_keys
        || implementation.get("version").and_then(Value::as_str) != version
        || !implementation
            .get("source_sha256")
            .and_then(Value::as_str)
            .is_some_and(|text| text.len() == 64)
        || !implementation.get("files").is_some_and(Value::is_array)
    {
        return Err(bad("implementation identity is invalid"));
    }
    Ok(())
}

/// Validate one persisted context plus its page/document representation binding.
pub fn validate_context(
    con: &Connection,
    item: &IndexMap<String, Value>,
    payload: &Value,
) -> Result<(), StoreError> {
    validate_payload(payload)?;
    let bad = |message: &str| StoreError::Invalid(format!("content evidence {message}"));
    if item.get("kind").and_then(Value::as_str) != Some(KIND)
        || item.get("payload_version").and_then(Value::as_str) != Some(OUTER_VERSION)
    {
        return Err(bad("context kind or version is invalid"));
    }
    let page = payload["page_url_id"].as_i64().unwrap_or_default();
    let source = payload["source_document_id"].as_i64().unwrap_or_default();
    let representation = payload["representation"].as_str().unwrap_or_default();
    let expected_key = format!("page:{page}:document:{source}:representation:{representation}");
    if item.get("item_key").and_then(Value::as_str) != Some(expected_key.as_str()) {
        return Err(bad("context key is invalid"));
    }
    let state = payload["state"].as_str().unwrap_or_default();
    let expected = if state == "complete" || state == "empty" {
        "complete"
    } else {
        "unavailable"
    };
    let reason = payload["reason"].as_str().unwrap_or_default();
    if item.get("completeness").and_then(Value::as_str) != Some(expected)
        || item.get("reason").and_then(Value::as_str) != Some(reason)
    {
        return Err(bad("context completeness is invalid"));
    }
    let bound = con
        .query_row(
            "SELECT 1 FROM documents WHERE document_id=?1 AND url_id=?2 AND representation=?3",
            params![source, page, representation],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !bound {
        return Err(bad("context references the wrong page or representation"));
    }
    Ok(())
}

/// Re-iterable retained JSON payloads for one kind; the connection owns their lifetime.
pub struct ContextRows<'a> {
    con: &'a Connection,
    kind: String,
    identity_order: bool,
}

impl<'a> ContextRows<'a> {
    pub fn new(con: &'a Connection, kind: &str, identity_order: bool) -> Self {
        Self {
            con,
            kind: kind.to_string(),
            identity_order,
        }
    }

    /// Number of retained items of this kind.
    pub fn len(&self) -> StoreResult<i64> {
        Ok(self.con.query_row(
            "SELECT COUNT(*) FROM context_items WHERE kind=?1",
            params![self.kind],
            |row| row.get(0),
        )?)
    }

    /// True when no item of this kind is retained.
    pub fn is_empty(&self) -> StoreResult<bool> {
        Ok(self.len()? == 0)
    }

    /// Every retained payload in identity or key order.
    pub fn rows(&self) -> StoreResult<Vec<Value>> {
        let order = if self.identity_order {
            "json_extract(payload_json,'$.page_url_id'),json_extract(payload_json,'$.source_document_id'),item_key"
        } else {
            "item_key"
        };
        let sql = format!("SELECT payload_json FROM context_items WHERE kind=?1 ORDER BY {order}");
        let mut statement = self.con.prepare(&sql)?;
        let texts = statement.query_map(params![self.kind], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for text in texts {
            out.push(serde_json::from_str(&text?)?);
        }
        Ok(out)
    }

    /// The same rows in duplicate-analysis identity order.
    pub fn ordered_for_duplicates(&self) -> ContextRows<'a> {
        ContextRows {
            con: self.con,
            kind: self.kind.clone(),
            identity_order: true,
        }
    }
}

/// A deterministic derived collection, materialized once.
pub struct DerivedRows {
    rows: Vec<Value>,
}

impl DerivedRows {
    /// Wrap already-derived rows.
    pub fn new(rows: Vec<Value>) -> Self {
        Self { rows }
    }

    /// Number of derived rows.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// True when there are no rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The rows in order.
    pub fn as_slice(&self) -> &[Value] {
        &self.rows
    }
}

/// Read retained facts as one versioned collection with a coverage statement.
///
/// A cursor-backed streaming mode is not needed here: the retained rows are always
/// materialized, and the output is identical either way.
pub fn read(con: &Connection, streaming: bool) -> Result<IndexMap<String, Value>, StoreError> {
    let _ = streaming;
    let rows = ContextRows::new(con, KIND, false).rows()?;
    let count = rows.len() as i64;
    let coverage = if count > 0 {
        json!({"state": "complete", "observed": count})
    } else {
        json!({"state": "unavailable", "reason": "content evidence was not stored in this scan"})
    };
    Ok(IndexMap::from([
        (
            "schema_version".to_string(),
            Value::String(VERSION.to_string()),
        ),
        ("items".to_string(), Value::Array(rows)),
        ("coverage".to_string(), coverage),
    ]))
}

/// Why one item is not a duplicate candidate, or `None` when it is eligible.
fn exclusion_reason(
    item: &Value,
    min_tokens: i64,
    include_nonindexable: bool,
    page_url: &dyn Fn(i64) -> Result<Option<String>, StoreError>,
) -> Result<Option<String>, StoreError> {
    let state = item.get("state").and_then(Value::as_str).unwrap_or("");
    if state == "empty" {
        return Ok(Some("main content is empty".to_string()));
    }
    if state != "complete" {
        let reason = item
            .get("reason")
            .and_then(Value::as_str)
            .filter(|r| !r.is_empty());
        return Ok(Some(
            reason
                .unwrap_or("content evidence is unavailable")
                .to_string(),
        ));
    }
    let Some(tokens) = item.get("content_tokens").and_then(Value::as_i64) else {
        return Ok(Some("content length is unavailable".to_string()));
    };
    if tokens < min_tokens {
        return Ok(Some("main content is too short".to_string()));
    }
    let indexable = item.get("indexable");
    if !include_nonindexable && indexable.and_then(Value::as_bool) != Some(true) {
        return Ok(Some(if indexable.and_then(Value::as_bool) == Some(false) {
            "non-indexable".to_string()
        } else {
            "indexability is unmeasured".to_string()
        }));
    }
    let canonical = item
        .get("canonical_target")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !canonical.is_empty() {
        let source_url = match item.get("page_url").and_then(Value::as_str) {
            Some(url) => Some(url.to_string()),
            None => {
                let id = item.get("page_url_id").and_then(Value::as_i64).unwrap_or(0);
                page_url(id)?
            }
        };
        let source_identity = source_url.as_deref().and_then(url_identity);
        let canonical_identity = url_identity(canonical);
        match (source_identity, canonical_identity) {
            (Some(source), Some(target)) => {
                if source != target {
                    return Ok(Some("canonicalized to another target".to_string()));
                }
            }
            _ => return Ok(Some("canonical target identity is unknown".to_string())),
        }
    }
    Ok(None)
}

/// Derive reproducible duplicate witnesses from stored hashes/fingerprints only.
///
/// The output names filters and every excluded population. It does not fetch bodies, recalculate
/// extraction, or turn unavailable evidence into a clean non-duplicate result.
///
#[allow(clippy::too_many_arguments)]
pub fn derive_duplicates(
    items: &[Value],
    threshold: f64,
    include_nonindexable: bool,
    page_urls: Option<&BTreeMap<i64, String>>,
    min_tokens: i64,
    max_candidates: i64,
    max_pairs: i64,
    streaming: bool,
) -> Result<IndexMap<String, Value>, StoreError> {
    derive_duplicates_lookup(
        items,
        threshold,
        include_nonindexable,
        &|id| Ok(page_urls.and_then(|urls| urls.get(&id)).cloned()),
        min_tokens,
        max_candidates,
        max_pairs,
        streaming,
    )
}

/// [`derive_duplicates`] with a lazy page-URL lookup, so streaming callers do not
/// materialize every captured page.
#[allow(clippy::too_many_arguments)]
pub fn derive_duplicates_lookup(
    items: &[Value],
    threshold: f64,
    include_nonindexable: bool,
    page_url: &dyn Fn(i64) -> Result<Option<String>, StoreError>,
    min_tokens: i64,
    max_candidates: i64,
    max_pairs: i64,
    streaming: bool,
) -> Result<IndexMap<String, Value>, StoreError> {
    let _ = streaming;
    if !(0.0..=1.0).contains(&threshold) {
        return Err(invalid("duplicate threshold must be a float from 0 to 1"));
    }
    if min_tokens < 1 {
        return Err(invalid("duplicate minimum token count must be positive"));
    }
    if max_candidates < 1 || max_pairs < 1 {
        return Err(invalid(
            "duplicate analysis limits must be positive integers",
        ));
    }
    let page_id = |item: &Value| item.get("page_url_id").and_then(Value::as_i64).unwrap_or(0);
    let doc_id = |item: &Value| {
        item.get("source_document_id")
            .and_then(Value::as_i64)
            .unwrap_or(0)
    };
    let mut ordered: Vec<&Value> = items.iter().collect();
    ordered.sort_by_key(|item| (page_id(item), doc_id(item)));

    let mut eligible: Vec<&Value> = Vec::new();
    let mut excluded: Vec<Value> = Vec::new();
    let mut candidate_cap_reached = false;
    for item in &ordered {
        let mut reason = exclusion_reason(item, min_tokens, include_nonindexable, page_url)?;
        if reason.is_none() {
            if (eligible.len() as i64) < max_candidates {
                eligible.push(item);
                continue;
            }
            candidate_cap_reached = true;
            reason = Some("duplicate candidate cap reached".to_string());
        }
        excluded.push(json!({
            "page_url_id": item.get("page_url_id").cloned().unwrap_or(Value::Null),
            "reason": reason.unwrap_or_default(),
        }));
    }
    let mut partial_reasons: Vec<&str> = Vec::new();
    if candidate_cap_reached {
        partial_reasons.push("candidate cap reached before all eligible content could be compared");
    }

    // Exact groups: same identity and normalized hash, more than one page.
    let identity_of = |item: &Value| {
        let implementation = item.get("implementation");
        (
            item.get("representation")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            item.get("strategy")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            implementation
                .and_then(|i| i.get("version"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            implementation
                .and_then(|i| i.get("source_sha256"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        )
    };
    let mut exact: BTreeMap<(String, String, String, String, String), BTreeSet<i64>> =
        BTreeMap::new();
    for item in &eligible {
        let (representation, strategy, version, source) = identity_of(item);
        let normalized_hash = item
            .get("normalized_hash")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        exact
            .entry((representation, strategy, version, source, normalized_hash))
            .or_default()
            .insert(page_id(item));
    }
    let exact_groups: Vec<Value> = exact
        .iter()
        .filter(|(_, pages)| pages.len() > 1)
        .map(
            |((representation, strategy, version, source, hash), pages)| {
                json!({
                    "representation": representation,
                    "strategy": strategy,
                    "implementation_version": version,
                    "implementation_source_sha256": source,
                    "normalized_hash": hash,
                    "pages": pages.iter().collect::<Vec<_>>(),
                })
            },
        )
        .collect();

    // Near-duplicate witnesses: pairs within one identity group, capped by `max_pairs`.
    let mut by_identity: BTreeMap<(String, String, String, String), Vec<&Value>> = BTreeMap::new();
    for item in &eligible {
        by_identity.entry(identity_of(item)).or_default().push(item);
    }
    let mut witnesses: Vec<(f64, i64, i64, Value)> = Vec::new();
    let mut pairs_considered: i64 = 0;
    let mut capped = false;
    'identities: for candidates in by_identity.values() {
        for (index, left) in candidates.iter().enumerate() {
            for right in &candidates[index + 1..] {
                if pairs_considered >= max_pairs {
                    capped = true;
                    break;
                }
                pairs_considered += 1;
                if page_id(left) == page_id(right)
                    || left.get("normalized_hash") == right.get("normalized_hash")
                {
                    continue;
                }
                let left_bits = left.get("simhash").and_then(Value::as_str).unwrap_or("");
                let right_bits = right.get("simhash").and_then(Value::as_str).unwrap_or("");
                let (Ok(l), Ok(r)) = (
                    u64::from_str_radix(left_bits, 16),
                    u64::from_str_radix(right_bits, 16),
                ) else {
                    continue;
                };
                let distance = (l ^ r).count_ones();
                let similarity = 1.0 - distance as f64 / 64.0;
                if similarity >= threshold {
                    witnesses.push((
                        similarity,
                        page_id(left),
                        page_id(right),
                        json!({
                            "left_page_url_id": page_id(left),
                            "left_source_document_id": doc_id(left),
                            "right_page_url_id": page_id(right),
                            "right_source_document_id": doc_id(right),
                            "similarity": similarity,
                            "representation": left.get("representation").cloned().unwrap_or(Value::Null),
                            "strategy": left.get("strategy").cloned().unwrap_or(Value::Null),
                        }),
                    ));
                }
            }
            if capped {
                break;
            }
        }
        if capped {
            break 'identities;
        }
    }
    if capped {
        partial_reasons.push("near-duplicate pair comparison cap reached");
    }
    witnesses.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
            .then(a.2.cmp(&b.2))
    });
    let near_witnesses: Vec<Value> = witnesses.into_iter().map(|w| w.3).collect();
    Ok(IndexMap::from([
        (
            "schema_version".to_string(),
            Value::String("content_duplicate_derivation.v2".to_string()),
        ),
        (
            "settings".to_string(),
            json!({
                "threshold": threshold,
                "include_nonindexable": include_nonindexable,
                "minimum_tokens": min_tokens,
                "identity": "normalized_hash and simhash within representation, strategy and implementation identity",
                "max_candidates": max_candidates,
                "max_pairs": max_pairs,
            }),
        ),
        ("exact_groups".to_string(), Value::Array(exact_groups)),
        ("near_witnesses".to_string(), Value::Array(near_witnesses)),
        ("excluded".to_string(), Value::Array(excluded)),
        (
            "partial".to_string(),
            Value::Bool(!partial_reasons.is_empty()),
        ),
        (
            "partial_reasons".to_string(),
            Value::Array(
                partial_reasons
                    .iter()
                    .map(|r| Value::String((*r).to_string()))
                    .collect(),
            ),
        ),
        (
            "comparisons".to_string(),
            json!({
                "eligible_candidates": eligible.len(),
                "pairs_considered": pairs_considered,
                "pair_cap_reached": capped,
            }),
        ),
    ]))
}

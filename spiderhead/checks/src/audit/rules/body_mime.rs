//! Compare supported response MIME declarations with the
//! retained body signatures of the selected static documents (`evaluate`).

use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Map, Value};
use spiderhead_parse::py_str::py_strip;
use spiderhead_store::bodies::read_document_body;

use super::asset_checks::decode_json_bytes;
use super::py_json_loads;

/// `CHECK_ID`.
pub const CHECK_ID: &str = "BODY_MIME_MISMATCH";
/// `MAX_BODY_BYTES`.
const MAX_BODY_BYTES: i64 = 2 * 1024 * 1024;
/// `_CACHE_LIMIT`: the per-sha signature cache keeps this many entries (LRU).
const CACHE_LIMIT: usize = 128;

/// `_PAGE_DOCUMENTS_SQL` verbatim.
const PAGE_DOCUMENTS_SQL: &str =
    "SELECT u.url,p.url_id,p.status_code,p.content_type,p.representation,\
    CASE WHEN p.representation='static' THEN p.document_id \
    WHEN p.representation!='static' THEN (\
     SELECT d.document_id FROM documents d JOIN context_items c \
     ON c.item_key='document:'||d.document_id AND c.kind='resource_inventory' \
     WHERE d.url_id=p.url_id AND d.representation='static' \
     AND NOT EXISTS(SELECT 1 FROM context_items later JOIN documents newer \
     ON later.item_key='document:'||newer.document_id \
     WHERE later.kind='resource_inventory' AND newer.url_id=d.url_id \
     AND newer.representation=d.representation AND newer.document_id>d.document_id) \
     ORDER BY d.document_id DESC LIMIT 1) ELSE NULL END AS source_document_id \
    FROM pages p JOIN urls u USING(url_id) ORDER BY p.page_ordinal";

/// `_DOCUMENT_RESPONSE_SQL` verbatim.
const DOCUMENT_RESPONSE_SQL: &str = "SELECT d.url_id,d.representation,d.body_sha256,d.body_state,\
    d.body_reason,d.source_response_id,d.fidelity,d.renderer_json,r.request_url_id,r.body_sha256,\
    r.body_state,r.body_reason,r.body_fidelity,r.content_type,r.status_code,\
    r.effective_status_code FROM documents d LEFT JOIN responses r \
    ON r.response_id=d.source_response_id WHERE d.document_id=?";

/// `_DECLARED_FAMILIES`.
fn declared_family_of(declared: &str) -> Option<&'static str> {
    Some(match declared {
        "text/html" | "application/xhtml+xml" => "html",
        "application/json" | "text/json" => "json",
        "application/pdf" => "pdf",
        "image/png" => "png",
        "image/jpeg" => "jpeg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        _ => return None,
    })
}

/// `_declared_family`: `(declared, family)` for a supported Content-Type string.
fn declared_family(value: Option<&str>) -> Option<(String, &'static str)> {
    let value = value?;
    if py_strip(value).is_empty() {
        return None;
    }
    let declared = py_strip(value.split(';').next().unwrap_or(value)).to_lowercase();
    declared_family_of(&declared).map(|family| (declared, family))
}

/// `bytes.lstrip(set)`.
fn lstrip<'a>(mut bytes: &'a [u8], set: &[u8]) -> &'a [u8] {
    while let Some(&b) = bytes.first() {
        if !set.contains(&b) {
            break;
        }
        bytes = &bytes[1..];
    }
    bytes
}

/// `haystack.find(needle)`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// `_signature`: a supported body family from a bounded prefix, or `None` when
/// the evidence is ambiguous.
fn signature(body: &[u8]) -> Option<&'static str> {
    let mut prefix = lstrip(&body[..body.len().min(4096)], b"\xef\xbb\xbf\x00\t\r\n ");
    for _ in 0..8 {
        if !prefix.starts_with(b"<!--") {
            break;
        }
        let end = find(prefix, b"-->")?;
        prefix = lstrip(&prefix[end + 3..], b"\x00\t\r\n ");
    }
    let lower: Vec<u8> = prefix.iter().map(u8::to_ascii_lowercase).collect();
    if lower.starts_with(b"<!doctype html")
        || lower.starts_with(b"<html")
        || lower.starts_with(b"<head")
        || lower.starts_with(b"<body")
    {
        return Some("html");
    }
    if prefix.starts_with(b"{") || prefix.starts_with(b"[") {
        // `json.loads` on bytes detects the codec; unparseable bodies are
        // ambiguous, never "some other family".
        let raw = &body[..body.len().min(MAX_BODY_BYTES as usize)];
        let text = decode_json_bytes(raw)?;
        py_json_loads(&text)?;
        return Some("json");
    }
    if prefix.starts_with(b"%PDF-") {
        return Some("pdf");
    }
    if prefix.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("png");
    }
    if prefix.starts_with(b"\xff\xd8\xff") {
        return Some("jpeg");
    }
    if prefix.starts_with(b"GIF87a") || prefix.starts_with(b"GIF89a") {
        return Some("gif");
    }
    if prefix.len() >= 12 && &prefix[..4] == b"RIFF" && &prefix[8..12] == b"WEBP" {
        return Some("webp");
    }
    if (lower.starts_with(b"<svg") || lower.starts_with(b"<?xml"))
        && find(&lower, b"<svg").is_some()
    {
        return Some("svg");
    }
    None
}

/// `_unavailable_reason`: classify the body reader's error text.
fn unavailable_reason(message: &str) -> &'static str {
    let message = message.to_lowercase();
    if message.contains("byte limit") || message.contains("exceeds decoded byte limit") {
        "oversized_body"
    } else if message.contains("body unavailable") || message.contains("not_in_corpus") {
        "absent_body"
    } else {
        "invalid_document_or_response_lineage"
    }
}

/// Integer value, or `None` for every other SQLite storage class (`type(v) is int`).
fn sql_int(value: &SqlValue) -> Option<i64> {
    match value {
        SqlValue::Integer(i) => Some(*i),
        _ => None,
    }
}

/// Text value, or `None` (`isinstance(v, str)`).
fn sql_str(value: &SqlValue) -> Option<&str> {
    match value {
        SqlValue::Text(s) => Some(s.as_str()),
        _ => None,
    }
}

/// `not value` on a raw SQLite value (NULL, 0, 0.0, "", b"" are falsy).
fn sql_falsy(value: &SqlValue) -> bool {
    match value {
        SqlValue::Null => true,
        SqlValue::Integer(i) => *i == 0,
        SqlValue::Real(f) => *f == 0.0,
        SqlValue::Text(s) => s.is_empty(),
        SqlValue::Blob(b) => b.is_empty(),
    }
}

/// `str(value or "unknown")` for the body-retention-state key parts.
fn state_str(value: &SqlValue) -> String {
    if sql_falsy(value) {
        return "unknown".into();
    }
    match value {
        SqlValue::Text(s) => s.clone(),
        SqlValue::Integer(i) => i.to_string(),
        SqlValue::Real(f) => spiderhead_parse::py_json::float_repr(*f),
        SqlValue::Blob(b) => format!("b{bytes:?}", bytes = String::from_utf8_lossy(b)),
        SqlValue::Null => "unknown".into(),
    }
}

/// `StoreError` -> the message `str(exc)` carries.
fn error_message(err: &spiderhead_store::error::StoreError) -> String {
    match err {
        spiderhead_store::error::StoreError::Invalid(message) => message.clone(),
        other => other.to_string(),
    }
}

/// Raw SQLite value -> the JSON value stored in `details`.
fn sql_json(value: &SqlValue) -> Value {
    match value {
        SqlValue::Null => Value::Null,
        SqlValue::Integer(i) => Value::from(*i),
        SqlValue::Real(f) => Value::from(*f),
        SqlValue::Text(s) => Value::from(s.as_str()),
        // A blob inside `details` cannot be serialized as JSON; unreachable in
        // the columns this check copies.
        SqlValue::Blob(_) => Value::Null,
    }
}

/// What `evaluate` produces for its `check_body_mime` caller.
pub struct BodyEvidence {
    /// `pages_eligible`.
    pub eligible: i64,
    /// `pages_measured`.
    pub measured: i64,
    /// `pages_unavailable` (the reasons except the two declaration misses).
    pub unavailable: i64,
    /// `findings_count`.
    pub findings_count: i64,
    /// `unavailable_reasons` in first-seen order.
    pub unavailable_reasons: Map<String, Value>,
    /// `body_retention_states` in first-seen order.
    pub body_retention_states: Map<String, Value>,
    /// `(target_url, details)` findings in emission order.
    pub findings: Vec<(String, Map<String, Value>)>,
}

/// `evaluate(con)`: stream per-page findings from each page's selected static
/// document, counting every unavailable reason.
pub fn evaluate(con: &Connection) -> BodyEvidence {
    let mut eligible = 0i64;
    let mut measured = 0i64;
    let mut emitted = 0i64;
    let mut reasons: Map<String, Value> = Map::new();
    let mut body_retention_states: Map<String, Value> = Map::new();
    let mut findings: Vec<(String, Map<String, Value>)> = Vec::new();
    #[allow(clippy::type_complexity)]
    let mut family_cache: Vec<(String, (Option<String>, Option<&'static str>))> = Vec::new();
    fn unavailable(reasons: &mut Map<String, Value>, reason: &str) {
        let count = reasons.get(reason).and_then(Value::as_i64).unwrap_or(0) + 1;
        reasons.insert(reason.to_owned(), Value::from(count));
    }

    let Ok(mut stmt) = con.prepare(PAGE_DOCUMENTS_SQL) else {
        return BodyEvidence {
            eligible,
            measured,
            unavailable: 0,
            findings_count: emitted,
            unavailable_reasons: reasons,
            body_retention_states,
            findings,
        };
    };
    let rows: Vec<PageRow> = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, SqlValue>(1)?,
                r.get::<_, SqlValue>(2)?,
                r.get::<_, SqlValue>(3)?,
                r.get::<_, SqlValue>(4)?,
                r.get::<_, Option<i64>>(5)?,
            ))
        })
        .ok()
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();

    for (url, url_id, page_status, page_type, representation, document_id) in rows {
        if !matches!(
            representation,
            SqlValue::Text(ref s)
                if s == "static" || s == "rendered" || s == "legacy_fragment"
        ) {
            unavailable(&mut reasons, "missing_page_representation");
            continue;
        }
        let Some(page_status) = sql_int(&page_status) else {
            continue;
        };
        if !(200..300).contains(&page_status) {
            continue;
        }
        let Some(document_id) = document_id else {
            if sql_falsy(&page_type) {
                unavailable(&mut reasons, "missing_declaration");
            } else if declared_family(sql_str(&page_type)).is_none() {
                unavailable(&mut reasons, "unsupported_family");
            } else {
                eligible += 1;
                unavailable(&mut reasons, "missing_selected_static_document");
            }
            continue;
        };
        let document = con.prepare(DOCUMENT_RESPONSE_SQL).ok().and_then(|mut s| {
            s.query_row([document_id], |r| {
                Ok((
                    r.get::<_, SqlValue>(0)?,
                    r.get::<_, SqlValue>(1)?,
                    r.get::<_, SqlValue>(2)?,
                    r.get::<_, SqlValue>(3)?,
                    r.get::<_, SqlValue>(4)?,
                    r.get::<_, SqlValue>(5)?,
                    r.get::<_, SqlValue>(6)?,
                    r.get::<_, SqlValue>(7)?,
                    r.get::<_, SqlValue>(8)?,
                    r.get::<_, SqlValue>(9)?,
                    r.get::<_, SqlValue>(10)?,
                    r.get::<_, SqlValue>(11)?,
                    r.get::<_, SqlValue>(12)?,
                    r.get::<_, SqlValue>(13)?,
                    r.get::<_, SqlValue>(14)?,
                    r.get::<_, SqlValue>(15)?,
                ))
            })
            .optional()
            .ok()
            .flatten()
        });
        let Some((
            document_url_id,
            document_representation,
            document_sha,
            document_state,
            document_reason,
            response_id,
            fidelity,
            renderer_json,
            request_url_id,
            response_sha,
            response_state,
            response_reason,
            response_fidelity,
            response_type,
            response_status,
            effective_status,
        )) = document
        else {
            unavailable(&mut reasons, "missing_selected_static_document");
            continue;
        };
        let declaration = declared_family(sql_str(&response_type));
        let Some((declared, expected)) = declaration else {
            unavailable(
                &mut reasons,
                if sql_falsy(&response_type) {
                    "missing_declaration"
                } else {
                    "unsupported_family"
                },
            );
            continue;
        };
        eligible += 1;
        if sql_str(&document_state) != Some("complete")
            || sql_str(&response_state) != Some("complete")
        {
            unavailable(&mut reasons, "absent_body");
            let state = [
                &document_state,
                &response_state,
                &document_reason,
                &response_reason,
            ]
            .iter()
            .map(|v| state_str(v))
            .collect::<Vec<_>>()
            .join("/");
            let count = body_retention_states
                .get(&state)
                .and_then(Value::as_i64)
                .unwrap_or(0)
                + 1;
            body_retention_states.insert(state, Value::from(count));
            continue;
        }
        let Some(document_sha) = sql_str(&document_sha).map(str::to_owned) else {
            unavailable(&mut reasons, "absent_or_mismatched_body_reference");
            continue;
        };
        if response_sha != SqlValue::Text(document_sha.clone()) {
            unavailable(&mut reasons, "absent_or_mismatched_body_reference");
            continue;
        }
        if document_url_id != url_id
            || sql_str(&document_representation) != Some("static")
            || sql_str(&renderer_json) != Some("{}")
            || sql_int(&response_id).is_none()
            || request_url_id != document_url_id
            || response_fidelity != fidelity
        {
            unavailable(
                &mut reasons,
                "incomplete_or_mismatched_document_response_binding",
            );
            continue;
        }
        let status = sql_int(&effective_status).or_else(|| sql_int(&response_status));
        let Some(status) = status else {
            unavailable(&mut reasons, "non_successful_source_response");
            continue;
        };
        if !(200..300).contains(&status) {
            unavailable(&mut reasons, "non_successful_source_response");
            continue;
        }
        let observed: Option<String> = if let Some(at) = family_cache
            .iter()
            .position(|(sha, _)| sha == &document_sha)
        {
            let (_, (observed, cached_reason)) = family_cache.remove(at);
            family_cache.push((document_sha.clone(), (observed.clone(), cached_reason)));
            if let Some(cached_reason) = cached_reason {
                unavailable(&mut reasons, cached_reason);
                continue;
            }
            observed
        } else {
            let observed = match read_document_body(con, document_id, MAX_BODY_BYTES) {
                Ok(body) => signature(&body).map(str::to_owned),
                Err(exc) => {
                    let reason = unavailable_reason(&error_message(&exc));
                    unavailable(&mut reasons, reason);
                    family_cache.push((document_sha.clone(), (None, Some(reason))));
                    if family_cache.len() > CACHE_LIMIT {
                        family_cache.remove(0);
                    }
                    continue;
                }
            };
            let cached_reason = if observed.is_none() {
                Some("ambiguous_body_signature")
            } else {
                None
            };
            family_cache.push((document_sha.clone(), (observed.clone(), cached_reason)));
            if family_cache.len() > CACHE_LIMIT {
                family_cache.remove(0);
            }
            observed
        };
        let Some(observed) = observed else {
            unavailable(&mut reasons, "ambiguous_body_signature");
            continue;
        };
        measured += 1;
        if observed != expected {
            findings.push((
                url.clone(),
                details_finding(
                    &url,
                    status,
                    &declared,
                    &observed,
                    &document_sha,
                    &document_state,
                    &document_reason,
                    document_id,
                    &response_id,
                    &document_representation,
                ),
            ));
            emitted += 1;
        }
    }
    let pages_unavailable: i64 = reasons
        .iter()
        .filter(|(reason, _)| {
            reason.as_str() != "missing_declaration" && reason.as_str() != "unsupported_family"
        })
        .map(|(_, count)| count.as_i64().unwrap_or(0))
        .sum();
    BodyEvidence {
        eligible,
        measured,
        unavailable: pages_unavailable,
        findings_count: emitted,
        unavailable_reasons: reasons,
        body_retention_states,
        findings,
    }
}

/// The one-page row of `_PAGE_DOCUMENTS_SQL`.
type PageRow = (String, SqlValue, SqlValue, SqlValue, SqlValue, Option<i64>);

/// The `item` dict `on_finding` passes to `ctx.add` (literal key order).
#[allow(clippy::too_many_arguments)]
fn details_finding(
    url: &str,
    status: i64,
    declared: &str,
    observed: &str,
    document_sha: &str,
    document_state: &SqlValue,
    document_reason: &SqlValue,
    document_id: i64,
    response_id: &SqlValue,
    document_representation: &SqlValue,
) -> Map<String, Value> {
    let mut item = Map::new();
    item.insert("target_url".into(), Value::from(url));
    item.insert("status_code".into(), Value::from(status));
    item.insert("declared_content_type".into(), Value::from(declared));
    item.insert("observed_body_family".into(), Value::from(observed));
    item.insert("body_sha256".into(), Value::from(document_sha));
    item.insert("body_bytes_limit".into(), Value::from(MAX_BODY_BYTES));
    item.insert("body_state".into(), sql_json(document_state));
    item.insert("body_reason".into(), sql_json(document_reason));
    item.insert("document_id".into(), Value::from(document_id));
    item.insert("source_response_id".into(), sql_json(response_id));
    item.insert("representation".into(), sql_json(document_representation));
    item.insert("target_scope".into(), Value::from("page_response_body"));
    item.insert("status_scope".into(), Value::from("page_response"));
    item.insert(
        "reason".into(),
        Value::from(
            "retained selected-document response body signature conflicts with its declared Content-Type",
        ),
    );
    item
}

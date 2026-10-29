//! One exact native URL read offline: its page record, bounded transport
//! responses, forms, source-HTML window and render console. Query values are
//! never returned and retained header/console text is redacted.

use std::path::Path;

use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};

use spiderhead_parse::py_url::{quote_plus, unquote, urlsplit, urlunsplit};

use crate::bodies::read_page_document_content;
use crate::browser_artifacts::{redact, retained_console};
use crate::discovery_ledger::query_maps;
use crate::error::StoreError;
use crate::scan_artifact::open_scan_mode;

const SENSITIVE_HEADERS: [&str; 6] = [
    "authorization",
    "cookie",
    "set-cookie",
    "proxy-authorization",
    "x-api-key",
    "x-auth-token",
];
const PAGE_URL_FIELDS: [&str; 7] = [
    "canonical",
    "redirect_url",
    "final_url",
    "og_image",
    "og_url",
    "meta_refresh",
    "http_refresh",
];
const NESTED_URL_FIELDS: [&str; 9] = [
    "url",
    "href",
    "action",
    "canonical",
    "next_url",
    "request_url",
    "location_raw",
    "source",
    "destination",
];
const PAGE_JSON_FIELDS: [&str; 6] = [
    "hreflang_json",
    "heading_outline_json",
    "link_placement_json",
    "trust_signals_json",
    "duplicate_ids_json",
    "canonical_chain_json",
];

/// Requested windows and budgets of one detail read.
pub struct DetailRequest<'a> {
    /// Exact retained URL.
    pub url: &'a str,
    /// First response row.
    pub response_offset: i64,
    /// Response rows per page.
    pub response_limit: i64,
    /// First form row.
    pub form_offset: i64,
    /// Form rows per page.
    pub form_limit: i64,
    /// Output byte ceiling.
    pub max_bytes: i64,
    /// First source character.
    pub body_offset: i64,
    /// Source characters returned.
    pub body_length: i64,
    /// Decoded body ceiling.
    pub max_body_bytes: i64,
}

fn invalid(message: impl Into<String>) -> StoreError {
    StoreError::Invalid(message.into())
}

/// reference `parse_qsl(keep_blank_values=True)`: pairs without `=` keep a blank value.
fn qsl_keep_blank(query: &str) -> Vec<String> {
    query
        .split('&')
        .filter(|piece| !piece.is_empty())
        .map(|piece| {
            let name = piece.split_once('=').map_or(piece, |(name, _)| name);
            unquote(&name.replace('+', " "))
        })
        .collect()
}

/// A retained URL's identity with every query value replaced by `[redacted]`.
pub fn detail_url(value: &Value) -> Value {
    let Some(text) = value.as_str() else {
        return value.clone();
    };
    if !text.contains('?') {
        return value.clone();
    }
    let Ok(parts) = urlsplit(text) else {
        return json!("[query-redacted]");
    };
    let names = qsl_keep_blank(&parts.query);
    let query = if names.is_empty() {
        "[redacted]".to_owned()
    } else {
        names
            .iter()
            .map(|name| format!("{}={}", quote_plus(name), quote_plus("[redacted]")))
            .collect::<Vec<_>>()
            .join("&")
    };
    json!(urlunsplit(
        &parts.scheme,
        &parts.netloc,
        &parts.path,
        &query,
        &parts.fragment
    ))
}

fn json_of(value: &Value, label: &str) -> Result<Value, StoreError> {
    value
        .as_str()
        .and_then(|text| serde_json::from_str(text).ok())
        .ok_or_else(|| invalid(format!("{label} is not valid retained JSON")))
}

fn headers_of(value: &Value, label: &str) -> Result<Value, StoreError> {
    let refused = || invalid(format!("{label} are not valid retained header pairs"));
    let pairs = value
        .as_str()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .ok_or_else(refused)?;
    let mut safe = Vec::new();
    for pair in pairs.as_array().ok_or_else(refused)? {
        let pair = pair
            .as_array()
            .filter(|p| p.len() == 2)
            .ok_or_else(refused)?;
        let (Some(name), Some(header)) = (pair[0].as_str(), pair[1].as_str()) else {
            return Err(refused());
        };
        if SENSITIVE_HEADERS.contains(&name.to_lowercase().as_str()) {
            safe.push(json!(["X-SEOHEAD-Redacted-Headers", name.to_lowercase()]));
        } else {
            safe.push(json!([name, detail_url(&json!(header))]));
        }
    }
    Ok(Value::Array(safe))
}

fn nested_urls(value: &Value, key: Option<&str>, depth: usize) -> Value {
    if depth > 8 {
        return value.clone();
    }
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), nested_urls(v, Some(k), depth + 1)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| nested_urls(v, key, depth + 1))
                .collect(),
        ),
        other if key.is_some_and(|k| NESTED_URL_FIELDS.contains(&k)) => detail_url(other),
        other => other.clone(),
    }
}

fn redirects_of(value: &Value) -> Result<Value, StoreError> {
    let chain = json_of(value, "redirect chain")?;
    let items = chain
        .as_array()
        .filter(|items| items.iter().all(Value::is_object))
        .ok_or_else(|| invalid("redirect chain is not an ordered retained object list"))?;
    Ok(Value::Array(
        items.iter().map(|e| nested_urls(e, None, 0)).collect(),
    ))
}

fn page_of(mut page: Map<String, Value>) -> Result<Value, StoreError> {
    if let Some(url) = page.get("url").cloned() {
        page.insert("url".into(), detail_url(&url));
    }
    for key in PAGE_URL_FIELDS {
        if let Some(value) = page.get(key).cloned() {
            page.insert(key.into(), detail_url(&value));
        }
    }
    for key in PAGE_JSON_FIELDS {
        if let Some(value) = page.get(key).filter(|v| !v.is_null()).cloned() {
            page.insert(key.into(), nested_urls(&json_of(&value, key)?, None, 0));
        }
    }
    let chain = page
        .shift_remove("redirect_chain_json")
        .unwrap_or(Value::Null);
    page.insert("redirect_chain".into(), redirects_of(&chain)?);
    for key in ["error", "body_unavailable"] {
        if let Some(value) = page.get(key).cloned() {
            page.insert(key.into(), json!(redact(&value)));
        }
    }
    Ok(Value::Object(page))
}

fn response_of(mut item: Map<String, Value>) -> Result<Value, StoreError> {
    for key in ["request_url", "effective_url"] {
        let value = item.get(key).cloned().unwrap_or(Value::Null);
        item.insert(key.into(), detail_url(&value));
    }
    let take =
        |item: &mut Map<String, Value>, key: &str| item.shift_remove(key).unwrap_or(Value::Null);
    let chain = take(&mut item, "redirect_chain_json");
    item.insert("redirect_chain".into(), redirects_of(&chain)?);
    for (from, to, label) in [
        (
            "request_headers_redacted_json",
            "request_headers",
            "request headers",
        ),
        (
            "response_headers_redacted_json",
            "response_headers",
            "response headers",
        ),
        (
            "effective_headers_redacted_json",
            "effective_headers",
            "effective headers",
        ),
    ] {
        let raw = take(&mut item, from);
        item.insert(to.into(), headers_of(&raw, label)?);
    }
    let error = item.get("error").cloned().unwrap_or(Value::Null);
    item.insert("error".into(), json!(redact(&error)));
    Ok(Value::Object(item))
}

fn form_of(mut row: Map<String, Value>) -> Value {
    let action = row.get("action").cloned().unwrap_or(Value::Null);
    row.insert("action".into(), detail_url(&action));
    let password = row.get("has_password").and_then(Value::as_i64).unwrap_or(0) != 0;
    row.insert("has_password".into(), json!(password));
    Value::Object(row)
}

fn source_html(con: &Connection, request: &DetailRequest<'_>) -> Result<Value, StoreError> {
    let retained =
        read_page_document_content(con, request.url, "static", None, request.max_body_bytes)?;
    if retained.get("state").and_then(Value::as_str) != Some("available") {
        let reason = retained
            .get("reason")
            .cloned()
            .unwrap_or(json!("not_retained"));
        let mut out = Map::new();
        out.insert("state".into(), json!("unavailable"));
        out.insert("reason".into(), reason);
        out.insert("representation".into(), json!("static"));
        if let Some(id) = retained.get("document_id").filter(|v| !v.is_null()) {
            out.insert("document_id".into(), id.clone());
        }
        return Ok(Value::Object(out));
    }
    let text: Vec<char> = retained["text"]
        .as_str()
        .unwrap_or_default()
        .chars()
        .collect();
    let start = (request.body_offset as usize).min(text.len());
    let end = (start + request.body_length as usize).min(text.len());
    let window: String = text[start..end].iter().collect();
    let field = |key: &str| retained.get(key).cloned().unwrap_or(Value::Null);
    Ok(json!({
        "state": "available",
        "representation": "static",
        "document_id": field("document_id"),
        "offset": request.body_offset,
        "length": end - start,
        "total_chars": field("total_chars"),
        "truncated": (request.body_offset as usize) + (end - start) < text.len(),
        "lines": field("lines"),
        "encoding": field("encoding"),
        "sha256": field("sha256"),
        "raw_body_sha256": field("raw_body_sha256"),
        "text": window,
    }))
}

fn check_request(request: &DetailRequest<'_>) -> Result<(), StoreError> {
    if request.url.is_empty() || request.url.chars().count() > 8192 {
        return Err(invalid(
            "url must be nonempty exact retained URL text of at most 8192 characters",
        ));
    }
    let nonneg = |value: i64, name: &str| {
        if value < 0 {
            Err(invalid(format!("{name} must be a nonnegative integer")))
        } else {
            Ok(())
        }
    };
    nonneg(request.response_offset, "response_offset")?;
    nonneg(request.form_offset, "form_offset")?;
    let ranged = |value: i64, low: i64, high: i64, message: &str| {
        if (low..=high).contains(&value) {
            Ok(())
        } else {
            Err(invalid(message))
        }
    };
    ranged(
        request.response_limit,
        1,
        100,
        "response_limit must be 1..100",
    )?;
    ranged(request.form_limit, 1, 100, "form_limit must be 1..100")?;
    ranged(
        request.max_bytes,
        4096,
        8 * 1024 * 1024,
        "max_bytes must be 4096..8388608",
    )?;
    nonneg(request.body_offset, "body_offset")?;
    ranged(
        request.body_length,
        1,
        64 * 1024,
        "body_length must be 1..65536",
    )?;
    ranged(
        request.max_body_bytes,
        1,
        64 * 1024 * 1024,
        "max_body_bytes must be 1..67108864",
    )
}

/// Read one exact native scan URL with bounded retained evidence, offline.
pub fn scan_url_detail(path: &str, request: &DetailRequest<'_>) -> Result<Value, StoreError> {
    if path.is_empty() {
        return Err(invalid("input is required"));
    }
    check_request(request)?;
    if path.to_lowercase().ends_with(".json") {
        return Ok(json!({
            "ok": true,
            "state": "unavailable",
            "reason": "Screaming Frog audit JSON does not retain native per-URL transport evidence",
            "source": {"source_kind": "screaming_frog", "scan_uuid": null, "evidence_revision": null},
        }));
    }
    let (con, validation) = open_scan_mode(Path::new(path), false, None, true)?;
    let mut sources = query_maps(
        &con,
        "SELECT scan_uuid,format_version,source_kind,evidence_revision,lifecycle,finish_reason,\
         crawl_partial,corpus_partial FROM scan WHERE singleton=1",
        [],
    )?;
    let Some(mut source) = sources.pop() else {
        return Err(invalid("scan source identity is unavailable"));
    };
    for key in ["crawl_partial", "corpus_partial"] {
        let flag = source.get(key).and_then(Value::as_i64).unwrap_or(0) != 0;
        source.insert(key.into(), json!(flag));
    }
    if source["source_kind"] != json!("native") {
        return Ok(json!({
            "validation": validation,
            "ok": true,
            "state": "unavailable",
            "reason": "this saved source does not retain native per-URL transport evidence",
            "source": source,
        }));
    }
    let url = request.url;
    let url_rows = query_maps(&con, "SELECT url_id,url FROM urls WHERE url=?", [url])?;
    let Some(url_row) = url_rows.first() else {
        return not_found(&con, &source, url, validation);
    };
    let url_id = url_row["url_id"].as_i64().unwrap_or(0);
    let pages = query_maps(
        &con,
        "SELECT p.*,u.url FROM pages p JOIN urls u USING(url_id) WHERE p.url_id=?",
        [url_id],
    )?;
    let Some(page) = pages.into_iter().next() else {
        return Ok(json!({
            "validation": validation,
            "ok": true,
            "state": "unavailable",
            "reason": "URL was retained without a page record",
            "source": source,
            "url": detail_url(&url_row["url"]),
            "url_id": url_id,
        }));
    };
    let mut ids: Vec<i64> = Vec::new();
    for row in query_maps(
        &con,
        "SELECT response_id FROM responses WHERE request_url_id=?",
        [url_id],
    )? {
        ids.extend(row["response_id"].as_i64());
    }
    for row in query_maps(
        &con,
        "SELECT response_id FROM responses WHERE effective_url_id=? AND request_url_id!=?",
        [url_id, url_id],
    )? {
        ids.extend(row["response_id"].as_i64());
    }
    let response_rows = query_maps(
        &con,
        "SELECT r.response_id,r.request_ordinal,r.request_url_id,r.effective_url_id,\
         request_url.url AS request_url,effective_url.url AS effective_url,r.redirect_chain_json,\
         r.method,r.purpose,r.requested_at,r.received_at,r.request_headers_redacted_json,\
         r.credentials_used,r.variant_key,r.status_code,r.effective_status_code,\
         r.response_headers_redacted_json,r.effective_headers_redacted_json,r.content_type,\
         r.charset,r.content_encoding,r.reported_size_bytes,r.response_time,r.transport_source,\
         r.cache_status,r.source_response_id,r.body_sha256,r.body_fidelity,r.body_state,\
         r.body_reason,r.error,r.error_kind \
         FROM responses r JOIN urls request_url ON request_url.url_id=r.request_url_id \
         LEFT JOIN urls effective_url ON effective_url.url_id=r.effective_url_id \
         WHERE r.response_id IN (SELECT value FROM json_each(?)) ORDER BY r.request_ordinal \
         LIMIT ? OFFSET ?",
        params![
            serde_json::to_string(&ids)?,
            request.response_limit + 1,
            request.response_offset
        ],
    )?;
    let form_rows = query_maps(
        &con,
        "SELECT form_id,ordinal,source_document_id,evidence_representation,method,action,has_password \
         FROM forms WHERE page_url_id=? ORDER BY form_id LIMIT ? OFFSET ?",
        params![url_id, request.form_limit + 1, request.form_offset],
    )?;
    let source_html = source_html(&con, request)?;
    let (response_limit, form_limit) =
        (request.response_limit as usize, request.form_limit as usize);
    let responses = response_rows
        .iter()
        .take(response_limit)
        .cloned()
        .map(response_of)
        .collect::<Result<Vec<_>, _>>()?;
    let forms: Vec<Value> = form_rows
        .iter()
        .take(form_limit)
        .cloned()
        .map(form_of)
        .collect();
    let console = retained_console(&con, Path::new(path), url_id)?;
    let detail = json!({
        "ok": true,
        "state": "available",
        "validation": validation,
        "source": source,
        "url": detail_url(&url_row["url"]),
        "url_id": url_id,
        "page": page_of(page)?,
        "responses": {
            "offset": request.response_offset,
            "limit": request.response_limit,
            "items": responses,
            "has_more": response_rows.len() > response_limit,
            "next_offset": request.response_offset + response_rows.len().min(response_limit) as i64,
            "redirect_target_lookup": {"state": "complete"},
        },
        "forms": {
            "offset": request.form_offset,
            "limit": request.form_limit,
            "items": forms,
            "has_more": form_rows.len() > form_limit,
            "next_offset": request.form_offset + form_rows.len().min(form_limit) as i64,
        },
        "source_html": source_html,
        "browser_console": console,
        "scope": "bounded retained HTML and native metadata only; no network request or artifact mutation",
    });
    let size = spiderhead_core::common::python_json(&detail).len() as i64;
    if size > request.max_bytes {
        return Ok(json!({
            "validation": validation,
            "ok": false,
            "state": "limit_reached",
            "reason": "output_byte_limit_exceeded",
            "source": source,
            "url": detail_url(&url_row["url"]),
            "max_bytes": request.max_bytes,
            "bytes_required": size,
        }));
    }
    Ok(detail)
}

/// The miss answer: the scan's hosts and a same-path variant, so a trailing-slash
/// or normalization slip is obvious instead of a bare miss.
fn not_found(
    con: &Connection,
    source: &Map<String, Value>,
    url: &str,
    validation: &str,
) -> Result<Value, StoreError> {
    let mut hosts: Vec<String> = query_maps(
        con,
        "SELECT DISTINCT u.url FROM urls u JOIN pages p ON p.url_id=u.url_id LIMIT 500",
        [],
    )?
    .iter()
    .filter_map(|row| row["url"].as_str())
    .filter(|text| text.contains("//"))
    .map(|text| {
        let after = text.split_once("//").map_or(text, |(_, rest)| rest);
        after.split('/').next().unwrap_or_default().to_owned()
    })
    .collect();
    hosts.sort();
    hosts.dedup();
    hosts.truncate(5);
    let flipped = match url.strip_suffix('/') {
        Some(stripped) => stripped.to_owned(),
        None => format!("{url}/"),
    };
    let variant = query_maps(con, "SELECT url FROM urls WHERE url=? LIMIT 1", [flipped])?
        .first()
        .map_or(Value::Null, |row| row["url"].clone());
    Ok(json!({
        "ok": true,
        "state": "not_found",
        "source": source,
        "url": detail_url(&json!(url)),
        "validation": validation,
        "hint": {"scan_hosts": hosts, "variant_present": variant},
    }))
}

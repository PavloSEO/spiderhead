//! Document readers: verified lineage from a stored document to its response and body bytes.
//!
//! Renderer provenance validation and navigation expansion belong to `checks::navigation` and
//! `checks::render`, which the store may not depend on. They enter through the [`RenderChecks`]
//! seam, installed once by the layer that owns them; static documents never need it.

use super::{decode_entity, line_count, read_body, DECODER_VERSION};
use crate::body_diff::rows as dict_rows;
use crate::error::StoreError;
use crate::scan_artifact::ScanError;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

/// Largest character window `read_document_window` returns.
pub const MAX_WINDOW_CHARS: i64 = 65536;

/// Largest decoded document `read_document_content` accepts (64 MiB).
pub const MAX_DOCUMENT_BYTES: i64 = 67108864;

/// Largest renderer concurrency recorded in provenance (`crawl.settings`).
const MAX_RENDER_PAGE_CONCURRENCY: i64 = 16;

/// SQL predicate selecting the documents a page may expose (selected, inventoried or unclaimed).
pub const DOCUMENT_CANDIDATE: &str = "(p.document_id=d.document_id OR i.item_key IS NOT NULL OR (NOT EXISTS(SELECT 1 FROM pages selected_page JOIN documents selected_doc ON selected_doc.document_id=selected_page.document_id WHERE selected_doc.url_id=d.url_id AND selected_doc.representation=d.representation) AND NOT EXISTS(SELECT 1 FROM context_items active JOIN documents selected_doc ON active.item_key='document:'||selected_doc.document_id WHERE active.kind='resource_inventory' AND selected_doc.url_id=d.url_id AND selected_doc.representation=d.representation)))";

/// Navigation and render checks owned by the `checks` crate (`checks::navigation`, `checks::render`).
pub trait RenderChecks: Send + Sync {
    /// `validate_navigation(navigation, stored=True, resolve_url=...)`; `Err` carries the
    /// `ValueError` text.
    fn validate_navigation(
        &self,
        navigation: &Value,
        resolve_url: Option<&dyn Fn(i64) -> Option<String>>,
    ) -> Result<(), String>;
    /// Fragment target for `url` and `html`.
    fn legacy_fragment_target(&self, url: &str, html: &str) -> Option<String>;
    /// `retain_navigation({}, lambda url: None)`: the empty retained navigation.
    fn empty_navigation(&self) -> Value;
    /// `expand_navigation(navigation, resolve_url)`.
    fn expand_navigation(
        &self,
        navigation: &Value,
        resolve_url: &dyn Fn(i64) -> Option<String>,
    ) -> Value;
}

static CHECKS: OnceLock<Box<dyn RenderChecks>> = OnceLock::new();

/// Install the render checks; returns `false` when some were installed already.
pub fn install_render_checks(checks: Box<dyn RenderChecks>) -> bool {
    CHECKS.set(checks).is_ok()
}

fn checks() -> Result<&'static dyn RenderChecks, StoreError> {
    CHECKS.get().map(|c| c.as_ref()).ok_or_else(|| {
        StoreError::Invalid("render checks are not installed (checks::navigation/render)".into())
    })
}

fn scan_err(message: impl Into<String>) -> StoreError {
    StoreError::ScanError(ScanError {
        message: message.into(),
    })
}

fn str_of(v: &Value) -> Option<&str> {
    v.as_str()
}

fn is_int(v: &Value) -> bool {
    v.is_i64() || v.is_u64()
}

/// Read the compatible encoding or the earlier prerelease top-level form of navigation evidence.
/// Returns `(present, value)`.
pub fn navigation_evidence(renderer: &Map<String, Value>) -> Result<(bool, Value), StoreError> {
    let settings = renderer.get("settings").and_then(Value::as_object);
    let in_settings = settings.is_some_and(|s| s.contains_key("navigation_evidence"));
    if renderer.contains_key("navigation") && in_settings {
        return Err(scan_err(
            "rendered navigation evidence has conflicting encodings",
        ));
    }
    if in_settings {
        let value = settings
            .and_then(|s| s.get("navigation_evidence"))
            .cloned()
            .unwrap_or(Value::Null);
        return Ok((true, value));
    }
    Ok((
        renderer.contains_key("navigation"),
        renderer.get("navigation").cloned().unwrap_or(Value::Null),
    ))
}

fn url_of(con: &Connection, url_id: i64) -> Option<String> {
    con.query_row("SELECT url FROM urls WHERE url_id=?", [url_id], |r| {
        r.get(0)
    })
    .optional()
    .ok()
    .flatten()
}

/// Validate and return the renderer provenance of a stored document.
pub fn renderer(
    document: &Map<String, Value>,
    con: Option<&Connection>,
) -> Result<Map<String, Value>, StoreError> {
    let invalid = || scan_err("document renderer provenance is invalid");
    let raw = document
        .get("renderer_json")
        .and_then(str_of)
        .ok_or_else(invalid)?;
    let value: Value = serde_json::from_str(raw).map_err(|_| invalid())?;
    let representation = document
        .get("representation")
        .and_then(str_of)
        .unwrap_or("");
    let complete = document.get("body_state").and_then(str_of) == Some("complete");
    let serialized = document.get("fidelity").and_then(str_of) == Some("serialized_dom");
    if complete && ((representation == "rendered") != serialized) {
        return Err(scan_err(
            "document representation and body fidelity disagree",
        ));
    }
    if representation == "static" {
        if value != json!({}) {
            return Err(scan_err("static document renderer must be empty"));
        }
        return Ok(Map::new());
    }
    const REQUIRED: [&str; 8] = [
        "engine",
        "engine_version",
        "settings",
        "flattened_iframes",
        "capture_limitations",
        "navigation_url_id",
        "final_url_id",
        "navigation_transform",
    ];
    let bad = || scan_err("rendered document renderer provenance is invalid");
    let map = value.as_object().ok_or_else(bad)?;
    let shape_ok = REQUIRED.iter().all(|k| map.contains_key(*k))
        && map.keys().all(|k| {
            REQUIRED.contains(&k.as_str()) || k == "page_concurrency" || k == "navigation"
        })
        && map["engine"].is_string()
        && map["engine_version"].is_string()
        && map["settings"].is_object()
        && map["flattened_iframes"].is_boolean()
        && map["capture_limitations"]
            .as_array()
            .is_some_and(|a| a.iter().all(Value::is_string))
        && matches!(
            map["navigation_transform"].as_str(),
            Some("direct" | "legacy_escaped_fragment" | "unknown")
        );
    if !shape_ok {
        return Err(bad());
    }
    let (has_navigation, navigation) = navigation_evidence(map)?;
    if has_navigation {
        let resolve = |id: i64| con.and_then(|c| url_of(c, id));
        let resolver: Option<&dyn Fn(i64) -> Option<String>> =
            con.map(|_| &resolve as &dyn Fn(i64) -> Option<String>);
        checks()?
            .validate_navigation(&navigation, resolver)
            .map_err(|e| scan_err(format!("rendered navigation evidence is invalid: {e}")))?;
    }
    if let Some(pc) = map.get("page_concurrency") {
        if !is_int(pc) || !(1..=MAX_RENDER_PAGE_CONCURRENCY).contains(&pc.as_i64().unwrap_or(0)) {
            return Err(scan_err("rendered page_concurrency provenance is invalid"));
        }
    }
    if representation == "legacy_fragment"
        && map["navigation_transform"] != "legacy_escaped_fragment"
    {
        return Err(scan_err(
            "legacy fragment requires a reproducible navigation transform",
        ));
    }
    for name in ["navigation_url_id", "final_url_id"] {
        let v = &map[name];
        if !v.is_null() && (!is_int(v) || v.as_i64().unwrap_or(0) < 1) {
            return Err(scan_err("renderer URL reference is invalid"));
        }
        if let (Some(c), Some(id)) = (con, v.as_i64()) {
            let present: Option<i64> = c
                .query_row("SELECT 1 FROM urls WHERE url_id=?", [id], |r| r.get(0))
                .optional()?;
            if present.is_none() {
                return Err(scan_err("renderer URL reference is absent"));
            }
        }
    }
    let transform = map["navigation_transform"].as_str().unwrap_or("");
    if complete
        && (transform == "unknown"
            || map["navigation_url_id"].is_null()
            || map["final_url_id"].is_null())
    {
        return Err(scan_err(
            "complete document requires known renderer navigation",
        ));
    }
    if transform == "direct"
        && map["navigation_url_id"] != document.get("url_id").cloned().unwrap_or(Value::Null)
    {
        return Err(scan_err(
            "direct renderer navigation differs from the logical page",
        ));
    }
    if transform == "legacy_escaped_fragment" {
        if let Some(c) = con {
            let logical = document
                .get("url_id")
                .and_then(Value::as_i64)
                .and_then(|id| url_of(c, id));
            let navigation = map["navigation_url_id"]
                .as_i64()
                .and_then(|id| url_of(c, id));
            let expected = match logical {
                Some(url) => {
                    checks()?.legacy_fragment_target(&url, "<meta name=\"fragment\" content=\"!\">")
                }
                None => None,
            };
            if navigation.is_none() || navigation != expected {
                return Err(scan_err(
                    "legacy fragment navigation does not reproduce the declared transform",
                ));
            }
        }
    }
    Ok(map.clone())
}

/// Read one document and verify both its exact body bytes and decoder metadata.
#[allow(clippy::type_complexity)]
pub fn read_document_parts(
    con: &Connection,
    document_id: i64,
    max_decoded_bytes: i64,
) -> Result<(String, Map<String, Value>, Vec<u8>), StoreError> {
    if document_id < 1 {
        return Err(scan_err("document ID must be a positive integer"));
    }
    let rows = dict_rows(
        con,
        "SELECT d.*, r.request_url_id AS response_request_url_id, \
         r.body_sha256 AS response_body_sha256, r.content_type AS response_content_type \
         , r.body_state AS response_body_state, r.body_fidelity AS response_body_fidelity \
         FROM documents d LEFT JOIN responses r ON r.response_id=d.source_response_id \
         WHERE d.document_id=?",
        &[json!(document_id)],
    )?;
    let Some(document) = rows.into_iter().next() else {
        return Err(scan_err("document unavailable: not_in_corpus"));
    };
    let text_of = |k: &str| document.get(k).and_then(Value::as_str).map(str::to_owned);
    let state = text_of("body_state").unwrap_or_default();
    let renderer = renderer(&document, Some(con))?;
    let body_sha = text_of("body_sha256");
    let Some(body_sha) = body_sha.filter(|_| state == "complete") else {
        return Err(scan_err(format!(
            "document unavailable: {state}/{}",
            text_of("body_reason").unwrap_or_default()
        )));
    };
    let fidelity = text_of("fidelity").unwrap_or_default();
    let raw = read_body(con, &body_sha, max_decoded_bytes)?;
    let representation = text_of("representation").unwrap_or_default();
    let (text, decoder) = match fidelity.as_str() {
        "entity_bytes" | "reencoded_text" => {
            let expected_request_url = if representation == "legacy_fragment" {
                match renderer.get("navigation_url_id") {
                    Some(v) if is_int(v) => v.clone(),
                    _ => {
                        return Err(scan_err(
                            "legacy fragment navigation URL provenance is invalid",
                        ))
                    }
                }
            } else {
                document["url_id"].clone()
            };
            if document["source_response_id"].is_null()
                || document["response_request_url_id"] != expected_request_url
                || document["response_body_sha256"] != json!(body_sha)
                || text_of("response_body_state").as_deref() != Some("complete")
                || text_of("response_body_fidelity").as_deref() != Some(fidelity.as_str())
            {
                return Err(scan_err(
                    "document response state, fidelity, URL, or body hash lineage disagrees",
                ));
            }
            if fidelity == "entity_bytes" {
                let Some(content_type) = text_of("response_content_type") else {
                    return Err(scan_err(
                        "entity decoder requires bytes and a content-type string",
                    ));
                };
                decode_entity(&raw, &content_type)?
            } else {
                let meta = decoder_meta(["legacy_unknown", "unknown", "unknown"]);
                (String::from_utf8_lossy(&raw).into_owned(), meta)
            }
        }
        "serialized_dom" => {
            if representation != "rendered" {
                return Err(scan_err("serialized DOM requires rendered representation"));
            }
            let text = String::from_utf8(raw.clone()).map_err(|e| {
                StoreError::Invalid(format!("utf-8 codec can't decode document: {e}"))
            })?;
            (
                text,
                decoder_meta(["renderer_utf8", "utf-8", "not_applicable"]),
            )
        }
        _ => return Err(scan_err("document fidelity is unavailable or unsupported")),
    };
    if decoder.iter().any(|(k, v)| document.get(k) != Some(v)) {
        return Err(scan_err(
            "document decoder metadata disagrees with stored bytes",
        ));
    }
    Ok((text, decoder, raw))
}

fn decoder_meta(values: [&str; 3]) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("decoder_version".into(), json!(DECODER_VERSION));
    m.insert("decoder_source".into(), json!(values[0]));
    m.insert("decoder_charset".into(), json!(values[1]));
    m.insert("decoder_errors".into(), json!(values[2]));
    m
}

/// Read one stored document only when its state and response lineage are complete.
pub fn read_document(
    con: &Connection,
    document_id: i64,
    max_decoded_bytes: i64,
) -> Result<String, StoreError> {
    Ok(read_document_parts(con, document_id, max_decoded_bytes)?.0)
}

/// Read exact response bytes through the selected document's verified lineage.
pub fn read_document_body(
    con: &Connection,
    document_id: i64,
    max_decoded_bytes: i64,
) -> Result<Vec<u8>, StoreError> {
    Ok(read_document_parts(con, document_id, max_decoded_bytes)?.2)
}

/// Keep the original private reader shape for storage extensions during integration.
pub fn read_document_(
    con: &Connection,
    document_id: i64,
    max_decoded_bytes: i64,
) -> Result<(String, Map<String, Value>), StoreError> {
    let (text, decoder, _) = read_document_parts(con, document_id, max_decoded_bytes)?;
    Ok((text, decoder))
}

/// Read the page's selected document by exact URL, preserving response lineage.
///
/// URL normalization is deliberately forbidden here: slash and query variants
/// can select different captured bodies. `read_document` verifies the
/// document-to-response-to-body hash join and its declared retention state.
pub fn read_page_document(
    con: &Connection,
    url: &str,
    max_decoded_bytes: i64,
) -> Result<String, StoreError> {
    let row: Option<Option<i64>> = con
        .query_row(
            "SELECT p.document_id FROM pages p JOIN urls u USING(url_id) WHERE u.url=?",
            [url],
            |r| r.get(0),
        )
        .optional()?;
    match row.flatten() {
        Some(id) => read_document(con, id, max_decoded_bytes),
        None => Err(scan_err("document unavailable: not_in_corpus")),
    }
}

fn sha_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Return a bounded character window of one verified document.
///
/// Offsets and lengths count decoded characters, so a multibyte sequence is never split.
/// `sha256` hashes the whole decoded text as UTF-8, not the stored bytes.
pub fn read_document_window(
    con: &Connection,
    document_id: i64,
    offset: i64,
    length: i64,
    max_decoded_bytes: i64,
) -> Result<Map<String, Value>, StoreError> {
    if offset < 0 {
        return Err(scan_err("window offset must be a nonnegative integer"));
    }
    if !(1..=MAX_WINDOW_CHARS).contains(&length) {
        return Err(scan_err(format!(
            "window length must be an integer from 1 to {MAX_WINDOW_CHARS}"
        )));
    }
    let (text, decoder, _) = read_document_parts(con, document_id, max_decoded_bytes)?;
    let window: String = text
        .chars()
        .skip(offset as usize)
        .take(length as usize)
        .collect();
    let total = text.chars().count() as i64;
    let shown = window.chars().count() as i64;
    let raw_sha: Option<String> = con.query_row(
        "SELECT body_sha256 FROM documents WHERE document_id=?",
        [document_id],
        |r| r.get(0),
    )?;
    let mut out = Map::new();
    out.insert("document_id".into(), json!(document_id));
    out.insert("offset".into(), json!(offset));
    out.insert("length".into(), json!(shown));
    out.insert("total_chars".into(), json!(total));
    out.insert("truncated".into(), json!(offset + shown < total));
    out.insert("lines".into(), json!(line_count(&text)));
    out.insert("encoding".into(), decoder["decoder_charset"].clone());
    out.insert("sha256".into(), json!(sha_hex(&text)));
    out.insert("raw_body_sha256".into(), json!(raw_sha));
    out.insert("text".into(), json!(window));
    Ok(out)
}

/// Read one complete retained document under an explicit decoded-byte cap.
pub fn read_document_content(
    con: &Connection,
    document_id: i64,
    max_decoded_bytes: i64,
) -> Result<Map<String, Value>, StoreError> {
    if !(1..=MAX_DOCUMENT_BYTES).contains(&max_decoded_bytes) {
        return Err(scan_err(format!(
            "document byte limit must be 1..{MAX_DOCUMENT_BYTES}"
        )));
    }
    let (text, decoder, _) = read_document_parts(con, document_id, max_decoded_bytes)?;
    let row: Option<(String, Option<String>, Option<String>)> = con
        .query_row(
            "SELECT d.representation,d.body_sha256,r.content_type FROM documents d \
             LEFT JOIN responses r ON r.response_id=d.source_response_id WHERE d.document_id=?",
            [document_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((representation, Some(raw_sha), content_type)) = row else {
        return Err(scan_err("document body disappeared during read"));
    };
    let scan: Option<(String, i64)> = con
        .query_row(
            "SELECT scan_uuid,evidence_revision FROM scan WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let mut out = Map::new();
    out.insert("scan_uuid".into(), json!(scan.as_ref().map(|s| &s.0)));
    out.insert(
        "evidence_revision".into(),
        json!(scan.as_ref().map(|s| s.1)),
    );
    out.insert("document_id".into(), json!(document_id));
    out.insert("representation".into(), json!(representation));
    out.insert("content_type".into(), json!(content_type));
    out.insert("raw_body_sha256".into(), json!(raw_sha));
    out.insert("sha256".into(), json!(sha_hex(&text)));
    out.insert("encoding".into(), decoder["decoder_charset"].clone());
    out.insert("decoder".into(), Value::Object(decoder));
    out.insert("total_chars".into(), json!(text.chars().count()));
    out.insert("lines".into(), json!(line_count(&text)));
    out.insert("text".into(), json!(text));
    Ok(out)
}

/// Resolve one exact retained page document and return its content with identity.
pub fn read_page_document_content(
    con: &Connection,
    url: &str,
    representation: &str,
    variant_key: Option<&str>,
    max_decoded_bytes: i64,
) -> Result<Map<String, Value>, StoreError> {
    if url.is_empty() || url.chars().count() > 8192 {
        return Err(scan_err(
            "document URL must be exact nonempty text of at most 8192 characters",
        ));
    }
    if !["static", "rendered", "legacy_fragment"].contains(&representation) {
        return Err(scan_err(
            "representation must be static, rendered, or legacy_fragment",
        ));
    }
    let joins = "LEFT JOIN responses r ON r.response_id=d.source_response_id \
                 LEFT JOIN pages p ON p.url_id=d.url_id \
                 LEFT JOIN context_items i ON i.kind='resource_inventory' AND i.item_key='document:'||d.document_id ";
    let variants: Vec<Option<String>> = dict_rows(
        con,
        &format!(
            "SELECT DISTINCT r.variant_key AS v FROM documents d JOIN urls u USING(url_id) {joins}\
             WHERE u.url=? AND d.representation=? AND {DOCUMENT_CANDIDATE} ORDER BY r.variant_key LIMIT 2"
        ),
        &[json!(url), json!(representation)],
    )?
    .into_iter()
    .map(|m| m["v"].as_str().map(str::to_owned))
    .collect();
    let mut out = Map::new();
    let unavailable = |reason: &str, extra: Vec<(&str, Value)>| {
        let mut m = Map::new();
        m.insert("state".into(), json!("unavailable"));
        m.insert("reason".into(), json!(reason));
        m.insert("url".into(), json!(url));
        for (k, v) in extra {
            m.insert(k.into(), v);
        }
        m
    };
    if variants.is_empty() {
        return Ok(unavailable("document_not_in_corpus", vec![]));
    }
    if variant_key.is_none() && variants.len() > 1 {
        return Ok(unavailable(
            "multiple_http_variants_require_variant_key",
            vec![("representation", json!(representation))],
        ));
    }
    let selected: Option<String> = variant_key
        .map(str::to_owned)
        .or_else(|| variants[0].clone());
    let rows = dict_rows(
        con,
        &format!(
            "SELECT d.document_id,d.body_state,d.body_reason FROM documents d \
             JOIN urls u USING(url_id) {joins}\
             WHERE u.url=? AND d.representation=? AND {DOCUMENT_CANDIDATE} \
             AND COALESCE(r.variant_key,'')=COALESCE(?,'') \
             ORDER BY (p.document_id=d.document_id) DESC,(i.item_key IS NOT NULL) DESC,\
             d.document_id DESC LIMIT 1"
        ),
        &[json!(url), json!(representation), json!(selected)],
    )?;
    let Some(row) = rows.into_iter().next() else {
        return Ok(unavailable(
            "requested_http_variant_not_in_corpus",
            vec![
                ("representation", json!(representation)),
                ("variant_key", json!(variant_key)),
            ],
        ));
    };
    if row["body_state"] != "complete" {
        let reason = format!(
            "{}/{}",
            row["body_state"].as_str().unwrap_or("None"),
            row["body_reason"].as_str().unwrap_or("None")
        );
        return Ok(unavailable(
            &reason,
            vec![
                ("document_id", row["document_id"].clone()),
                ("representation", json!(representation)),
                ("variant_key", json!(selected)),
            ],
        ));
    }
    out.insert("state".into(), json!("available"));
    out.insert("url".into(), json!(url));
    out.insert("variant_key".into(), json!(selected));
    let id = row["document_id"].as_i64().unwrap_or(0);
    out.extend(read_document_content(con, id, max_decoded_bytes)?);
    Ok(out)
}

/// Read bounded navigation evidence without fetching or interpreting its cause.
pub fn read_document_navigation(
    con: &Connection,
    document_id: i64,
) -> Result<Map<String, Value>, StoreError> {
    if document_id < 1 {
        return Err(StoreError::Invalid(
            "document_id must be a positive integer".into(),
        ));
    }
    let rows = dict_rows(
        con,
        "SELECT * FROM documents WHERE document_id=?",
        &[json!(document_id)],
    )?;
    let Some(document) = rows.into_iter().next() else {
        return Err(scan_err("navigation document is absent"));
    };
    let renderer = renderer(&document, Some(con))?;
    let (has_navigation, navigation) = if renderer.is_empty() {
        (false, Value::Null)
    } else {
        navigation_evidence(&renderer)?
    };
    let navigation = if has_navigation {
        navigation
    } else {
        checks()?.empty_navigation()
    };
    let resolve = |id: i64| url_of(con, id);
    let public_keys = [
        "engine",
        "engine_version",
        "settings",
        "navigation_transform",
    ];
    let mut public = Map::new();
    for key in public_keys {
        if let Some(v) = renderer.get(key) {
            public.insert(key.into(), v.clone());
        }
    }
    if let Some(Value::Object(settings)) = public.get_mut("settings") {
        settings.shift_remove("navigation_evidence");
    }
    let url_for = |name: &str| {
        renderer
            .get(name)
            .and_then(Value::as_i64)
            .filter(|id| *id != 0)
            .and_then(resolve)
    };
    let mut out = Map::new();
    out.insert("document_id".into(), json!(document_id));
    out.insert("representation".into(), document["representation"].clone());
    out.insert("renderer".into(), Value::Object(public));
    out.insert("requested_url".into(), json!(url_for("navigation_url_id")));
    out.insert("final_url".into(), json!(url_for("final_url_id")));
    out.insert(
        "navigation".into(),
        checks()?.expand_navigation(&navigation, &resolve),
    );
    Ok(out)
}

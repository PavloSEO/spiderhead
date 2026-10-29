//! Versioned canonical/pagination attributes without changing the frozen scan SQL.
#![allow(clippy::too_many_arguments, clippy::type_complexity)]

use crate::discovery_ledger::{canonical_json, is_py_int};
use crate::error::StoreError;
use rusqlite::Connection;
use serde_json::{Map, Value};

pub const VERSION: &str = "head_links.v1";

/// Largest record accepted (8 MiB), kept in step with `scan_artifact`.
const MAX_RECORD_BYTES: usize = 8 * 1024 * 1024;

const UNAVAILABLE_NOT_HTML: &str = "head annotations were not parsed as eligible HTML";
const OVERSIZED: &str = "raw head annotations exceed native context byte limit";
const LIMIT_REACHED: &str = "head annotation limit reached";

fn invalid(msg: &str) -> StoreError {
    StoreError::Invalid(msg.to_string())
}

/// Completeness and reason of a head-link context, from its measured declarations.
fn classify(measured: bool, count: usize) -> (&'static str, &'static str) {
    if measured && count >= 100 {
        ("partial", LIMIT_REACHED)
    } else if measured {
        ("complete", "")
    } else {
        ("unavailable", UNAVAILABLE_NOT_HTML)
    }
}

pub fn context_item(
    page_url_id: i64,
    source_document_id: i64,
    representation: &str,
    parsed: &Value,
) -> Value {
    let head = parsed.as_object().and_then(|o| o.get("head_links"));
    let mut measured = head.is_some();
    let declarations = head.cloned();
    let count = head.and_then(Value::as_array).map_or(0, Vec::len);
    let build = |decl: Option<Value>| {
        let mut payload = Map::new();
        payload.insert("schema_version".into(), Value::from(VERSION));
        payload.insert("page_url_id".into(), Value::from(page_url_id));
        payload.insert("source_document_id".into(), Value::from(source_document_id));
        payload.insert("representation".into(), Value::from(representation));
        payload.insert("declarations".into(), decl.unwrap_or(Value::Null));
        canonical_json(&Value::Object(payload))
    };
    let mut encoded = build(declarations);
    let oversized = encoded.len() > MAX_RECORD_BYTES;
    if oversized {
        encoded = build(None);
        measured = false;
    }
    let (completeness, reason) = if oversized {
        ("unavailable", OVERSIZED)
    } else {
        classify(measured, count)
    };
    let mut item = Map::new();
    item.insert("kind".into(), Value::from("head_links"));
    item.insert(
        "item_key".into(),
        Value::from(format!(
            "page:{page_url_id}:document:{source_document_id}:representation:{representation}"
        )),
    );
    item.insert("payload_version".into(), Value::from("scan_context.v1"));
    item.insert("payload_json".into(), Value::from(encoded));
    item.insert("completeness".into(), Value::from(completeness));
    item.insert("reason".into(), Value::from(reason));
    Value::Object(item)
}

pub fn validate_context(
    con: &Connection,
    item: &Map<String, Value>,
    payload: &Value,
) -> Result<(), StoreError> {
    let bad = || invalid("native head annotation context is invalid");
    let Value::Object(p) = payload else {
        return Err(bad());
    };
    let keys = [
        "schema_version",
        "page_url_id",
        "source_document_id",
        "representation",
        "declarations",
    ];
    if p.len() != keys.len() || keys.iter().any(|k| !p.contains_key(*k)) {
        return Err(bad());
    }
    let page_url_id = p["page_url_id"]
        .as_i64()
        .filter(|_| is_py_int(&p["page_url_id"]));
    let document_id = p["source_document_id"]
        .as_i64()
        .filter(|_| is_py_int(&p["source_document_id"]));
    let representation = p["representation"].as_str().unwrap_or("");
    let (Some(page_url_id), Some(document_id)) = (page_url_id, document_id) else {
        return Err(bad());
    };
    if p["schema_version"].as_str() != Some(VERSION)
        || !(1..i64::MAX).contains(&page_url_id)
        || !(1..i64::MAX).contains(&document_id)
        || !["static", "rendered", "legacy_fragment"].contains(&representation)
    {
        return Err(bad());
    }
    let declarations = &p["declarations"];
    let decl_rows: Option<&Vec<Value>> = match declarations {
        Value::Null => None,
        Value::Array(rows) => {
            if rows.len() > 100 {
                return Err(invalid("native head annotation declarations are invalid"));
            }
            for row in rows {
                let Value::Object(r) = row else {
                    return Err(invalid("native head annotation declarations are invalid"));
                };
                let has = |k: &str| r.contains_key(k);
                let required_ok = has("rel") && has("href") && has("target");
                let allowed = ["rel", "href", "target", "hreflang", "lang", "media", "type"];
                let all_allowed = r.keys().all(|k| allowed.contains(&k.as_str()));
                if !(required_ok && all_allowed) || r.values().any(|v| !v.is_string()) {
                    return Err(invalid("native head annotation declarations are invalid"));
                }
            }
            Some(rows)
        }
        _ => return Err(invalid("native head annotation declarations are invalid")),
    };
    let measured = decl_rows.is_some();
    let count = decl_rows.map_or(0, Vec::len);
    let (completeness, expected_reason) = classify(measured, count);
    let item_reason = item.get("reason").and_then(Value::as_str).unwrap_or("");
    let unavailable_reason = item_reason == UNAVAILABLE_NOT_HTML || item_reason == OVERSIZED;
    let reason_expected = if measured {
        expected_reason.to_string()
    } else if unavailable_reason {
        item_reason.to_string()
    } else {
        UNAVAILABLE_NOT_HTML.to_string()
    };
    let expected_key =
        format!("page:{page_url_id}:document:{document_id}:representation:{representation}");
    if item.get("item_key").and_then(Value::as_str) != Some(expected_key.as_str())
        || item.get("completeness").and_then(Value::as_str) != Some(completeness)
        || item_reason != reason_expected
    {
        return Err(invalid(
            "native head annotation document identity disagrees",
        ));
    }
    let found: Option<i64> = con
        .query_row(
            "SELECT 1 FROM documents WHERE document_id=?1 AND url_id=?2 AND representation=?3",
            rusqlite::params![document_id, page_url_id, representation],
            |r| r.get(0),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    if found.is_none() {
        return Err(invalid(
            "native head annotation document identity disagrees",
        ));
    }
    Ok(())
}

/// Stream annotations only from each captured page's selected document, in page order.
/// Each entry is the page URL and its declarations (None when the context is unmeasured).
pub fn active(
    con: &Connection,
) -> Result<Vec<(String, Option<Vec<Map<String, Value>>>)>, StoreError> {
    let mut st = con.prepare(
        "SELECT u.url,c.payload_json FROM pages p JOIN urls u USING(url_id) \
         JOIN context_items c ON c.kind='head_links' AND c.item_key=\
         'page:'||p.url_id||':document:'||p.document_id||':representation:'||p.representation \
         ORDER BY p.page_ordinal",
    )?;
    let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = Vec::new();
    for row in rows {
        let (url, payload) = row?;
        let evidence: Value = serde_json::from_str(&payload)
            .map_err(|_| invalid("native head annotation context is invalid"))?;
        let declarations = attach_document(&evidence);
        out.push((url, declarations));
    }
    Ok(out)
}

/// Declarations of one evidence payload with `source_document_id` merged into each row.
fn attach_document(evidence: &Value) -> Option<Vec<Map<String, Value>>> {
    let source = evidence
        .get("source_document_id")
        .cloned()
        .unwrap_or(Value::Null);
    evidence
        .get("declarations")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(Value::as_object)
                .map(|row| {
                    let mut merged = row.clone();
                    merged.insert("source_document_id".into(), source.clone());
                    merged
                })
                .collect()
        })
}

/// Read one selected document's complete annotations through indexed URL identity.
pub fn for_url(con: &Connection, url: &str) -> Result<Option<Vec<Map<String, Value>>>, StoreError> {
    let payload: Option<String> = con
        .query_row(
            "SELECT c.payload_json FROM pages p JOIN urls u USING(url_id) \
             JOIN context_items c ON c.kind='head_links' AND c.item_key=\
             'page:'||p.url_id||':document:'||p.document_id||':representation:'||p.representation \
             WHERE u.url=?1 AND c.completeness='complete'",
            [url],
            |r| r.get(0),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    let Some(payload) = payload else {
        return Ok(None);
    };
    let evidence: Value = serde_json::from_str(&payload)
        .map_err(|_| invalid("native head annotation context is invalid"))?;
    Ok(Some(attach_document(&evidence).unwrap_or_default()))
}

/// Count the same fetched HTML source population as AuditContext.html_pages.
pub fn eligible_count(con: &Connection) -> Result<i64, StoreError> {
    Ok(con.query_row(
        "SELECT COUNT(*) FROM pages WHERE status_code BETWEEN 200 AND 299 AND lower(content_type) LIKE '%html%'",
        [],
        |r| r.get(0),
    )?)
}

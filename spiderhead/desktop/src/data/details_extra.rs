//! The remaining details-tab payloads: cheap badge counters, the retained
//! resource inventory, the redirect chain plus canonical fields, the cross-scan
//! history of one URL and its structured-data evidence. All reads are bounded —
//! resources cap at [`RESOURCE_CAP`] rows, history at [`HISTORY_LIMIT`] scans.

use rusqlite::types::Value as SqlValue;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Map, Value};

use super::filters::QueryError;
use super::session::ScanSession;

/// Resource rows the resources tab returns at most.
pub const RESOURCE_CAP: i64 = 1000;
/// Scans the history tab compares against at most.
pub const HISTORY_LIMIT: i64 = 50;

fn qe(reason: &'static str, e: rusqlite::Error) -> QueryError {
    QueryError::unavailable(reason, format!("cannot read scan: {e}"))
}

/// The retained URL text of a `url_id` (needed by history/canonical joins).
fn url_text(session: &ScanSession, url_id: i64) -> Result<Option<String>, QueryError> {
    session
        .con
        .query_row(
            "SELECT url FROM urls WHERE url_id=?1",
            params![url_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| qe("cannot_read", e))
}

/// Count of rows of `table` owned by `field = url_id`; `table` is a fixed name.
fn count_owned(session: &ScanSession, sql: &str, url_id: i64) -> i64 {
    session
        .con
        .query_row(sql, params![url_id], |r| r.get(0))
        .unwrap_or(0)
}

/// Cheap per-tab counters for the details tab bar (`tab = "counts"`).
/// Each badge reflects retained evidence only: zero stays distinguishable
/// from absent because the tab payloads carry their own `state`.
pub fn counts(session: &ScanSession, url_id: i64) -> Result<Value, QueryError> {
    let page_row = session
        .con
        .query_row(
            "SELECT images_total, images_missing_alt_attr, jsonld_blocks_found
             FROM pages WHERE url_id=?1",
            params![url_id],
            |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|e| qe("cannot_read", e))?
        .ok_or_else(|| {
            QueryError::invalid("unknown_url", format!("no page row for url_id {url_id}"))
        })?;
    let resp_row: Option<(Option<String>, Option<String>, Option<String>)> = session
        .con
        .query_row(
            "SELECT request_headers_redacted_json, response_headers_redacted_json,
                    redirect_chain_json
             FROM responses WHERE request_url_id=?1 AND purpose='page'
             ORDER BY request_ordinal DESC LIMIT 1",
            params![url_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|e| qe("cannot_read", e))?;
    let json_len = |raw: &Option<String>| -> i64 {
        raw.as_deref()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .and_then(|v| match v {
                Value::Object(o) => Some(o.len() as i64),
                Value::Array(a) => Some(a.len() as i64),
                _ => None,
            })
            .unwrap_or(0)
    };
    let (req, resp, chain) = resp_row.unwrap_or_default();
    let evidence_items = count_owned(
        session,
        "SELECT COUNT(*) FROM context_items c
         WHERE c.kind='structured_evidence'
         AND CAST(json_extract(c.payload_json,'$.page_url_id') AS INTEGER)=?1",
        url_id,
    );
    Ok(json!({
        "links_in": count_owned(
            session, "SELECT COUNT(*) FROM links WHERE destination_url_id=?1", url_id),
        "links_out": count_owned(
            session, "SELECT COUNT(*) FROM links WHERE source_url_id=?1", url_id),
        "headers": json_len(&req) + json_len(&resp),
        "resources": count_owned(
            session,
            "SELECT COUNT(*) FROM resource_refs WHERE page_url_id=?1",
            url_id) + page_row.0.unwrap_or(0),
        "redirects": json_len(&chain),
        "structured": page_row.2.unwrap_or(0) + evidence_items,
        "images_total": page_row.0,
        "images_missing_alt": page_row.1,
        "jsonld_blocks_found": page_row.2,
    }))
}

/// The retained resource inventory of one page (`tab = "resources"`).
/// `items` are declared script/stylesheet refs; images come from `pages`
/// counters because image links live in `links`, not `resource_refs`.
pub fn resources(session: &ScanSession, url_id: i64) -> Result<Value, QueryError> {
    let has_page = count_owned(
        session,
        "SELECT COUNT(*) FROM pages WHERE url_id=?1",
        url_id,
    );
    if has_page == 0 {
        return Err(QueryError::invalid(
            "unknown_url",
            format!("no page row for url_id {url_id}"),
        ));
    }
    let total = count_owned(
        session,
        "SELECT COUNT(*) FROM resource_refs WHERE page_url_id=?1",
        url_id,
    );
    let mut stmt = session
        .con
        .prepare(
            "SELECT rr.kind, u.url, r.status_code, r.reported_size_bytes,
                    rr.capture_state, rr.reason, rr.ordinal
             FROM resource_refs rr
             JOIN urls u ON u.url_id=rr.resource_url_id
             LEFT JOIN responses r ON r.response_id=rr.response_id
             WHERE rr.page_url_id=?1
             ORDER BY rr.kind, rr.ordinal LIMIT ?2",
        )
        .map_err(|e| qe("cannot_read", e))?;
    let rows = stmt
        .query_map(params![url_id, RESOURCE_CAP], |r| {
            Ok(vec![
                r.get::<_, SqlValue>(0)?,
                r.get::<_, SqlValue>(1)?,
                r.get::<_, SqlValue>(2)?,
                r.get::<_, SqlValue>(3)?,
                r.get::<_, SqlValue>(4)?,
                r.get::<_, SqlValue>(5)?,
                r.get::<_, SqlValue>(6)?,
            ])
        })
        .map_err(|e| qe("cannot_read", e))?;
    let mut items = Vec::new();
    for row in rows {
        let v = row.map_err(|e| qe("cannot_read", e))?;
        items.push(json!({
            "kind": super::query::sql_to_json(v[0].clone()),
            "url": super::query::sql_to_json(v[1].clone()),
            "status_code": super::query::sql_to_json(v[2].clone()),
            "size_bytes": super::query::sql_to_json(v[3].clone()),
            "capture_state": super::query::sql_to_json(v[4].clone()),
            "reason": super::query::sql_to_json(v[5].clone()),
            "ordinal": super::query::sql_to_json(v[6].clone()),
        }));
    }
    let images: (Option<i64>, Option<i64>) = session
        .con
        .query_row(
            "SELECT images_total, images_missing_alt_attr FROM pages WHERE url_id=?1",
            params![url_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or((None, None));
    let capabilities = session
        .con
        .query_row(
            "SELECT capabilities_json FROM scan WHERE singleton=1",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.get("resource_refs").cloned())
        .unwrap_or(Value::Null);
    Ok(json!({
        "state": "available",
        "items": items,
        "total": total,
        "returned": total.min(RESOURCE_CAP),
        "capped": total > RESOURCE_CAP,
        "images": {"total": images.0, "missing_alt": images.1},
        "capability": capabilities,
    }))
}

/// The retained redirect chain and canonical fields (`tab = "redirects"`).
/// Chain entries carry `request_url_id`/`next_url_id`; each hop resolves to
/// its URL text here so the view never re-queries the `urls` table.
pub fn redirects(session: &ScanSession, url_id: i64) -> Result<Value, QueryError> {
    let url = url_text(session, url_id)?.ok_or_else(|| {
        QueryError::invalid("unknown_url", format!("no page row for url_id {url_id}"))
    })?;
    let row = session
        .con
        .query_row(
            "SELECT p.status_code, p.canonical, p.final_canonical, p.redirect_url,
                    p.final_url,
                    (SELECT r.redirect_chain_json FROM responses r
                      WHERE r.request_url_id=p.url_id AND r.purpose='page'
                      ORDER BY r.request_ordinal DESC LIMIT 1)
             FROM pages p WHERE p.url_id=?1",
            params![url_id],
            |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ))
            },
        )
        .optional()
        .map_err(|e| qe("cannot_read", e))?
        .ok_or_else(|| {
            QueryError::invalid("unknown_url", format!("no page row for url_id {url_id}"))
        })?;
    let (status, canonical, final_canonical, redirect_url, final_url, chain_raw) = row;
    let resolve = |id: &Value| -> Value {
        id.as_i64()
            .and_then(|n| {
                session
                    .con
                    .query_row("SELECT url FROM urls WHERE url_id=?1", params![n], |r| {
                        r.get::<_, String>(0)
                    })
                    .ok()
            })
            .map(Value::String)
            .unwrap_or(Value::Null)
    };
    let mut chain = Vec::new();
    if let Some(raw) = chain_raw.as_deref() {
        if let Ok(Value::Array(hops)) = serde_json::from_str::<Value>(raw) {
            for hop in hops {
                if let Value::Object(o) = hop {
                    chain.push(json!({
                        "url": resolve(o.get("request_url_id").unwrap_or(&Value::Null)),
                        "status_code": o.get("status_code").cloned().unwrap_or(Value::Null),
                        "location": o.get("location_raw").cloned().unwrap_or(Value::Null),
                        "next_url": resolve(o.get("next_url_id").unwrap_or(&Value::Null)),
                        "blocked": o.get("blocked").cloned().unwrap_or(Value::Null),
                    }));
                }
            }
        }
    }
    let canonical_self = canonical
        .as_deref()
        .is_some_and(|c| !c.is_empty() && c == url);
    Ok(json!({
        "state": "available",
        "url": url,
        "status_code": status,
        "chain": chain,
        "hops": chain.len() as i64,
        "redirect_url": redirect_url,
        "final_url": final_url,
        "canonical": canonical,
        "final_canonical": final_canonical,
        "canonical_self": canonical_self,
    }))
}

/// Cross-scan history of the selected URL (`tab = "history"`).
/// Reads the retained `scan.v1` files beside the open one directly — the
/// store's `url_history` is intentionally not used because its
/// external-links field still waits on an unported store slice. A one-file
/// directory reports exactly one entry; fields the sibling scan lacks stay
/// `null`, never fabricated zeroes.
pub fn history(session: &ScanSession, url_id: i64) -> Result<Value, QueryError> {
    let url = url_text(session, url_id)?.ok_or_else(|| {
        QueryError::invalid("unknown_url", format!("no page row for url_id {url_id}"))
    })?;
    let dir = session
        .path
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_default();
    let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    name.ends_with(".sqlite") && !name.contains(".audit-v2")
                })
                .collect()
        })
        .unwrap_or_default();
    paths.sort();
    let mut scans = Vec::new();
    let mut errors = Vec::new();
    for path in paths.iter().take(HISTORY_LIMIT as usize) {
        match observe_page(path, &url) {
            Ok(entry) => {
                scans.push(entry);
            }
            Err(e) => {
                errors.push(json!({
                    "file": path.display().to_string(),
                    "reason": e.to_string().chars().take(512).collect::<String>(),
                }));
                scans.push(json!({
                    "scan_uuid": scan_uuid_of(path),
                    "state": "unavailable",
                    "finished_at": Value::Null,
                }));
            }
        }
    }
    // Newest first; `number` counts down like the store reader does.
    scans.sort_by(|a, b| {
        let key = |s: &Value| {
            s.get("finished_at")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        key(b).cmp(&key(a))
    });
    let total = scans.len() as i64;
    for (i, entry) in scans.iter_mut().enumerate() {
        if let Value::Object(o) = entry {
            o.insert("number".into(), json!(total - i as i64));
        }
    }
    Ok(json!({
        "ok": true,
        "state": if errors.is_empty() { "available" } else { "partial" },
        "format": "spiderhead.scan-url-history.v1",
        "url": url,
        "scan_total": total,
        "returned": scans.len() as i64,
        "has_more": false,
        "scans": Value::Array(scans),
        "transitions": Value::Array(vec![]),
        "errors": Value::Array(errors),
    }))
}

/// `scan_uuid` of a retained scan file; falls back to the file stem when the
/// header cannot be read.
fn scan_uuid_of(path: &std::path::Path) -> String {
    rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()
    .and_then(|con| {
        con.query_row("SELECT scan_uuid FROM scan WHERE singleton=1", [], |r| {
            r.get::<_, String>(0)
        })
        .ok()
    })
    .unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string()
    })
}

/// One sibling scan's row for the history table: page fields plus the scan
/// header; `absent` when the URL never appeared in that scan.
fn observe_page(path: &std::path::Path, url: &str) -> Result<Value, Box<dyn std::error::Error>> {
    let con = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let (uuid, finished_at): (String, Option<String>) = con.query_row(
        "SELECT scan_uuid, finished_at FROM scan WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let row = con
        .query_row(
            "SELECT p.status_code, p.title, p.word_count, p.response_time,
                    p.size_bytes,
                    (SELECT COUNT(*) FROM links l WHERE l.destination_url_id=u.url_id) AS inlinks,
                    (SELECT COUNT(*) FROM links l WHERE l.source_url_id=p.url_id) AS outlinks
             FROM urls u JOIN pages p ON p.url_id=u.url_id WHERE u.url=?1",
            params![url],
            |r| {
                Ok(json!({
                    "status_code": super::query::sql_to_json(r.get::<_, SqlValue>(0)?),
                    "title": super::query::sql_to_json(r.get::<_, SqlValue>(1)?),
                    "word_count": super::query::sql_to_json(r.get::<_, SqlValue>(2)?),
                    "response_time": super::query::sql_to_json(r.get::<_, SqlValue>(3)?),
                    "size_bytes": super::query::sql_to_json(r.get::<_, SqlValue>(4)?),
                    "inlinks": super::query::sql_to_json(r.get::<_, SqlValue>(5)?),
                    "outlinks": super::query::sql_to_json(r.get::<_, SqlValue>(6)?),
                }))
            },
        )
        .optional()?;
    let mut out = Map::new();
    out.insert("scan_uuid".into(), json!(uuid));
    out.insert(
        "finished_at".into(),
        finished_at.map(Value::from).unwrap_or(Value::Null),
    );
    match row {
        Some(Value::Object(o)) => {
            out.insert("state".into(), json!("present"));
            out.extend(o);
        }
        _ => {
            out.insert("state".into(), json!("absent"));
        }
    }
    Ok(Value::Object(out))
}

/// Structured-data evidence of one page (`tab = "structured"`).
/// Payloads are `structured_evidence.v2` objects: state, blocks, nodes,
/// edges and the validation summary, one per retained representation.
pub fn structured(session: &ScanSession, url_id: i64) -> Result<Value, QueryError> {
    let blocks_found: Option<i64> = session
        .con
        .query_row(
            "SELECT jsonld_blocks_found FROM pages WHERE url_id=?1",
            params![url_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| qe("cannot_read", e))?
        .flatten();
    if blocks_found.is_none()
        && count_owned(
            session,
            "SELECT COUNT(*) FROM pages WHERE url_id=?1",
            url_id,
        ) == 0
    {
        return Err(QueryError::invalid(
            "unknown_url",
            format!("no page row for url_id {url_id}"),
        ));
    }
    let mut stmt = session
        .con
        .prepare(
            "SELECT c.item_key, c.payload_json FROM context_items c
             WHERE c.kind='structured_evidence'
             AND CAST(json_extract(c.payload_json,'$.page_url_id') AS INTEGER)=?1
             ORDER BY c.item_key",
        )
        .map_err(|e| qe("cannot_read", e))?;
    let rows = stmt
        .query_map(params![url_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(|e| qe("cannot_read", e))?;
    let mut items = Vec::new();
    for row in rows {
        let (item_key, raw) = row.map_err(|e| qe("cannot_read", e))?;
        let payload = serde_json::from_str::<Value>(&raw)
            .unwrap_or_else(|_| json!({"state": "unavailable", "reason": "payload is not JSON"}));
        items.push(json!({"item_key": item_key, "payload": payload}));
    }
    let mut out = Map::new();
    out.insert("state".into(), json!("available"));
    out.insert("items".into(), Value::Array(items));
    out.insert(
        "jsonld_blocks_found".into(),
        blocks_found.map(Value::from).unwrap_or(Value::Null),
    );
    Ok(Value::Object(out))
}

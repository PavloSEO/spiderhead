//! The URL details panel's data: page fields, inbound/outbound links, the last
//! retained response (headers) and the retained body. All reads are bounded —
//! links cap at [`LINK_CAP`] rows, the body at [`BODY_CAP`] bytes.

use rusqlite::types::Value as SqlValue;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Map, Value};

use super::filters::QueryError;
use super::session::ScanSession;

/// Rows a links tab returns at most (rest reported via `capped`).
pub const LINK_CAP: i64 = 1000;
/// Body bytes decoded at most for the body tab.
pub const BODY_CAP: usize = 512 * 1024;

fn qe(reason: &'static str, e: rusqlite::Error) -> QueryError {
    QueryError::unavailable(reason, format!("cannot read scan: {e}"))
}

/// Fields of the page summary tab (`tab = "summary"`).
pub fn summary(session: &ScanSession, url_id: i64) -> Result<Value, QueryError> {
    let row = session
        .con
        .query_row(
            "SELECT u.url, p.url_id, p.page_ordinal, p.status_code, p.content_type,
                    p.size_bytes, p.response_time, p.title, p.meta_description, p.h1,
                    p.h1_2, p.canonical, p.final_canonical, p.meta_robots, p.x_robots,
                    p.word_count, p.text_ratio, p.crawl_depth, p.outlinks,
                    p.external_outlinks, p.redirect_url, p.final_url, p.representation,
                    p.error, p.error_kind, p.cache_status, p.charset, p.content_encoding,
                    p.images_total, p.images_missing_alt_attr, p.jsonld_blocks_found
             FROM pages p JOIN urls u ON u.url_id=p.url_id WHERE p.url_id=?1",
            params![url_id],
            |r| {
                let mut v = Vec::new();
                for i in 0..31 {
                    v.push(r.get::<_, SqlValue>(i)?);
                }
                Ok(v)
            },
        )
        .optional()
        .map_err(|e| qe("cannot_read", e))?;
    let Some(v) = row else {
        return Err(QueryError::invalid(
            "unknown_url",
            format!("no page row for url_id {url_id}"),
        ));
    };
    let names = [
        "url",
        "url_id",
        "page_ordinal",
        "status_code",
        "content_type",
        "size_bytes",
        "response_time",
        "title",
        "meta_description",
        "h1",
        "h1_2",
        "canonical",
        "final_canonical",
        "meta_robots",
        "x_robots",
        "word_count",
        "text_ratio",
        "crawl_depth",
        "outlinks",
        "external_outlinks",
        "redirect_url",
        "final_url",
        "representation",
        "error",
        "error_kind",
        "cache_status",
        "charset",
        "content_encoding",
        "images_total",
        "images_missing_alt_attr",
        "jsonld_blocks_found",
    ];
    let mut out = Map::new();
    for (name, value) in names.iter().zip(v) {
        out.insert((*name).to_string(), super::query::sql_to_json(value));
    }
    let inlinks: i64 = session
        .con
        .query_row(
            "SELECT COUNT(*) FROM links WHERE destination_url_id=?1",
            params![url_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    out.insert("inlinks".into(), json!(inlinks));
    Ok(Value::Object(out))
}

/// Inbound or outbound link rows for the links tabs.
/// `direction` is `in` (who links here) or `out` (where this page links).
pub fn links(session: &ScanSession, url_id: i64, direction: &str) -> Result<Value, QueryError> {
    let (sql, total_sql) = match direction {
        "in" => (
            "SELECT u.url, l.anchor, l.position, l.nofollow, p.status_code, l.ordinal,
                    l.rel_json, l.target, l.raw_href
             FROM links l JOIN urls u ON u.url_id=l.source_url_id
             LEFT JOIN pages p ON p.url_id=l.source_url_id
             WHERE l.destination_url_id=?1
             ORDER BY l.source_url_id, l.ordinal LIMIT ?2",
            "SELECT COUNT(*) FROM links WHERE destination_url_id=?1",
        ),
        "out" => (
            "SELECT u.url, l.anchor, l.position, l.nofollow, p.status_code, l.ordinal,
                    l.rel_json, l.target, l.raw_href
             FROM links l JOIN urls u ON u.url_id=l.destination_url_id
             LEFT JOIN pages p ON p.url_id=l.destination_url_id
             WHERE l.source_url_id=?1
             ORDER BY l.ordinal LIMIT ?2",
            "SELECT COUNT(*) FROM links WHERE source_url_id=?1",
        ),
        _ => {
            return Err(QueryError::invalid(
                "invalid_direction",
                "direction must be in or out",
            ))
        }
    };
    let total: i64 = session
        .con
        .query_row(total_sql, params![url_id], |r| r.get(0))
        .unwrap_or(0);
    let mut stmt = session.con.prepare(sql).map_err(|e| qe("cannot_read", e))?;
    let rows = stmt
        .query_map(params![url_id, LINK_CAP], |r| {
            Ok(vec![
                r.get::<_, SqlValue>(0)?,
                r.get::<_, SqlValue>(1)?,
                r.get::<_, SqlValue>(2)?,
                r.get::<_, SqlValue>(3)?,
                r.get::<_, SqlValue>(4)?,
                r.get::<_, SqlValue>(5)?,
                r.get::<_, SqlValue>(6)?,
                r.get::<_, SqlValue>(7)?,
                r.get::<_, SqlValue>(8)?,
            ])
        })
        .map_err(|e| qe("cannot_read", e))?;
    let mut items = Vec::new();
    for row in rows {
        let v = row.map_err(|e| qe("cannot_read", e))?;
        let rel = super::query::sql_to_json(v[6].clone());
        let rel = rel
            .as_str()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .unwrap_or(rel);
        items.push(json!({
            "url": super::query::sql_to_json(v[0].clone()),
            "anchor": super::query::sql_to_json(v[1].clone()),
            "position": super::query::sql_to_json(v[2].clone()),
            "nofollow": super::query::sql_to_json(v[3].clone()),
            "status_code": super::query::sql_to_json(v[4].clone()),
            "ordinal": super::query::sql_to_json(v[5].clone()),
            "rel": rel,
            "target": super::query::sql_to_json(v[7].clone()),
            "raw_href": super::query::sql_to_json(v[8].clone()),
        }));
    }
    Ok(json!({
        "direction": direction,
        "total": total,
        "returned": items.len() as i64,
        "capped": total > items.len() as i64,
        "items": items,
    }))
}

/// The newest retained `page` response for the URL: status, headers, timing.
/// Header JSONs are redacted dictionaries — returned verbatim, not re-ordered.
pub fn response(session: &ScanSession, url_id: i64) -> Result<Value, QueryError> {
    let row = session
        .con
        .query_row(
            "SELECT r.status_code, r.effective_status_code, r.method, r.content_type,
                    r.charset, r.content_encoding, r.response_time, r.reported_size_bytes,
                    r.request_headers_redacted_json, r.response_headers_redacted_json,
                    r.effective_headers_redacted_json, r.redirect_chain_json, r.error,
                    r.error_kind, r.requested_at, r.received_at, u.url AS effective_url
             FROM responses r
             LEFT JOIN urls u ON u.url_id=r.effective_url_id
             WHERE r.request_url_id=?1 AND r.purpose='page'
             ORDER BY r.request_ordinal DESC LIMIT 1",
            params![url_id],
            |r| {
                let mut v = Vec::new();
                for i in 0..17 {
                    v.push(r.get::<_, SqlValue>(i)?);
                }
                Ok(v)
            },
        )
        .optional()
        .map_err(|e| qe("cannot_read", e))?;
    let names = [
        "status_code",
        "effective_status_code",
        "method",
        "content_type",
        "charset",
        "content_encoding",
        "response_time",
        "reported_size_bytes",
        "request_headers",
        "response_headers",
        "effective_headers",
        "redirect_chain",
        "error",
        "error_kind",
        "requested_at",
        "received_at",
        "effective_url",
    ];
    let mut out = Map::new();
    match row {
        Some(v) => {
            for (name, value) in names.iter().zip(v) {
                let value = super::query::sql_to_json(value);
                let value = if name.ends_with("_headers") || *name == "redirect_chain" {
                    value
                        .as_str()
                        .and_then(|s| serde_json::from_str::<Value>(s).ok())
                        .unwrap_or(value)
                } else {
                    value
                };
                out.insert((*name).to_string(), value);
            }
            out.insert("state".into(), json!("available"));
        }
        None => {
            out.insert("state".into(), json!("unavailable"));
            out.insert(
                "reason".into(),
                json!("no retained page response for this URL"),
            );
        }
    }
    Ok(Value::Object(out))
}

/// Body bytes as text in the page's recorded charset (the one the crawler
/// parsed with); UTF-8 when it is unknown, so a cp1251 page does not show as
/// replacement characters in the HTML tab.
fn decode_body(bytes: &[u8], charset: Option<&str>) -> String {
    match charset.and_then(|l| encoding_rs::Encoding::for_label(l.trim().as_bytes())) {
        Some(enc) if enc != encoding_rs::UTF_8 && !enc.name().starts_with("UTF-16") => {
            enc.decode_without_bom_handling(bytes).0.into_owned()
        }
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// The retained body text of the page's document (zlib or identity codec).
/// `state` distinguishes missing documents, omitted bodies and decode failures.
pub fn body(session: &ScanSession, url_id: i64) -> Result<Value, QueryError> {
    let row = session
        .con
        .query_row(
            "SELECT d.body_sha256, b.codec, b.data, d.representation, p.charset
             FROM pages p JOIN documents d ON d.document_id=p.document_id
             JOIN bodies b ON b.sha256=d.body_sha256
             WHERE p.url_id=?1
             ORDER BY CASE d.representation WHEN 'rendered' THEN 0 ELSE 1 END
             LIMIT 1",
            params![url_id],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Vec<u8>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()
        .map_err(|e| qe("cannot_read", e))?;
    let Some((sha, codec, data, representation, charset)) = row else {
        return Ok(json!({
            "state": "unavailable",
            "reason": "this scan retained no body for the URL",
        }));
    };
    let decoded = match codec.as_str() {
        "identity" => data,
        "zlib" => {
            use std::io::Read;
            let mut out = Vec::new();
            match flate2::read::ZlibDecoder::new(&data[..]).read_to_end(&mut out) {
                Ok(_) => out,
                Err(e) => {
                    return Ok(json!({
                        "state": "unavailable",
                        "reason": format!("body bytes do not decode: {e}"),
                        "sha256": sha,
                    }))
                }
            }
        }
        other => {
            return Ok(json!({
                "state": "unavailable",
                "reason": format!("unknown body codec {other}"),
                "sha256": sha,
            }))
        }
    };
    let capped = decoded.len() > BODY_CAP;
    let text = decode_body(&decoded[..decoded.len().min(BODY_CAP)], charset.as_deref());
    Ok(json!({
        "state": "available",
        "sha256": sha,
        "representation": representation,
        "bytes": decoded.len(),
        "capped": capped,
        "text": text,
    }))
}

/// Dispatch `details.load` params to one tab's payload.
pub fn load(session: &ScanSession, params: &Value) -> Result<Value, QueryError> {
    let url_id = params
        .get("url_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| QueryError::invalid("invalid_url", "url_id is required"))?;
    let tab = params
        .get("tab")
        .and_then(Value::as_str)
        .unwrap_or("summary");
    match tab {
        "summary" => summary(session, url_id),
        "links_in" => links(session, url_id, "in"),
        "links_out" => links(session, url_id, "out"),
        // The canvas names these tabs `headers` and `html`; `response`/`body`
        // stay as aliases for earlier control-channel callers.
        "response" | "headers" => response(session, url_id),
        "body" | "html" => body(session, url_id),
        "counts" => super::details_extra::counts(session, url_id),
        "resources" => super::details_extra::resources(session, url_id),
        "redirects" => super::details_extra::redirects(session, url_id),
        "history" => super::details_extra::history(session, url_id),
        "structured" => super::details_extra::structured(session, url_id),
        other => Err(QueryError::invalid(
            "invalid_tab",
            format!("unknown details tab {other}"),
        )),
    }
}

#[cfg(test)]
mod charset_tests {
    use super::decode_body;

    #[test]
    fn cp1251_body_decodes_in_its_recorded_charset() {
        let raw = [0xD1, 0xF2, 0xE0, 0xF0, 0xE0, 0xFF];
        assert_eq!(decode_body(&raw, Some("windows-1251")), "Старая");
        assert_eq!(decode_body("ok".as_bytes(), None), "ok");
    }
}

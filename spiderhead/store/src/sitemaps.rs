//! Closed context for the URL membership of selected expanded sitemap roots.

use crate::error::StoreError;
use crate::scan_artifact::{dump, insert, url as intern_url, ScanError};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;

/// Context kinds owned by this module, sorted for a stable order.
pub const KINDS: &[&str] = &[
    "sitemap_declaration",
    "sitemap_declared_url",
    "sitemap_fetch_summary",
];

/// Most selected sitemap roots one scan keeps.
pub const MAX_SELECTED_ROOTS: i64 = 5000;

/// Largest valid row id (`2**63 - 1`).
pub const MAX_ID: i64 = 9223372036854775807;

/// Seam for the context store: validates and stores one item.
pub trait ContextSink {
    /// Validate and write one `context_items` row.
    fn put_context(&self, con: &Connection, item: &Map<String, Value>) -> Result<(), StoreError>;
}

fn scan_err(message: &str) -> StoreError {
    StoreError::ScanError(ScanError {
        message: message.into(),
    })
}

fn is_id(value: Option<&Value>) -> Option<i64> {
    value
        .and_then(Value::as_i64)
        .filter(|v| (1..=MAX_ID).contains(v))
}

/// Ids of every selected sitemap root, refusing duplicates and malformed rows.
pub fn root_ids(con: &Connection) -> Result<BTreeSet<i64>, StoreError> {
    let mut roots = BTreeSet::new();
    let mut stmt =
        con.prepare("SELECT payload_json FROM context_items WHERE kind='sitemap_declaration'")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    for (index, row) in rows.enumerate() {
        if index as i64 >= MAX_SELECTED_ROOTS {
            return Err(scan_err("too many selected sitemap roots"));
        }
        let value: Value = serde_json::from_str(&row?)?;
        let sid = value
            .as_object()
            .and_then(|o| o.get("sitemap_url_id"))
            .filter(|v| v.is_i64() || v.is_u64())
            .and_then(Value::as_i64)
            .ok_or_else(|| scan_err("invalid selected sitemap root"))?;
        if !(1..=MAX_ID).contains(&sid) || !roots.insert(sid) {
            return Err(scan_err(
                "selected sitemap roots must reference unique valid URLs",
            ));
        }
    }
    Ok(roots)
}

fn exists(con: &Connection, sql: &str, args: impl rusqlite::Params) -> Result<bool, StoreError> {
    Ok(con.query_row(sql, args, |_| Ok(())).optional()?.is_some())
}

/// Validate one sitemap context item against its payload and the stored URLs.
/// `selected_roots` defaults to unset.
pub fn validate_context(
    con: &Connection,
    item: &Map<String, Value>,
    payload: &Value,
    selected_roots: Option<&BTreeSet<i64>>,
) -> Result<(), StoreError> {
    let kind = item.get("kind").and_then(Value::as_str).unwrap_or("");
    let keys: &[&str] = match kind {
        "sitemap_declaration" => &["sitemap_url_id", "source", "ordinal"],
        "sitemap_declared_url" => &["sitemap_url_id", "url_id", "ordinal"],
        "sitemap_fetch_summary" => &["sitemap_url_id", "response_ids", "complete", "reason"],
        _ => return Err(scan_err("unknown sitemap context kind")),
    };
    let item_key = item.get("item_key").and_then(Value::as_str).unwrap_or("");
    let object = payload
        .as_object()
        .filter(|o| o.len() == keys.len() && keys.iter().all(|k| o.contains_key(*k)))
        .ok_or_else(|| scan_err(&format!("{kind}: invalid sitemap context shape")))?;
    let sid = is_id(object.get("sitemap_url_id"))
        .filter(|sid| {
            exists(con, "SELECT 1 FROM urls WHERE url_id=?", params![sid]).unwrap_or(false)
        })
        .ok_or_else(|| scan_err(&format!("{kind}: invalid sitemap URL reference")))?;
    match kind {
        "sitemap_declaration" => {
            let ordinal = object["ordinal"].as_i64();
            let source = object["source"].as_str().unwrap_or("");
            let ok = ordinal.is_some_and(|o| {
                (0..MAX_SELECTED_ROOTS).contains(&o) && item_key == format!("ordinal:{o}")
            }) && matches!(source, "explicit" | "robots" | "fallback" | "sitemap-only");
            if !ok {
                return Err(scan_err("invalid sitemap declaration source/order"));
            }
        }
        "sitemap_declared_url" => {
            let ordinal = object["ordinal"].as_i64();
            let url_id = is_id(object.get("url_id"));
            let ok = ordinal.is_some_and(|o| {
                (0..=MAX_ID).contains(&o) && item_key == format!("sitemap:{sid}:ordinal:{o}")
            }) && url_id.is_some();
            if !ok {
                return Err(scan_err("invalid sitemap member source/order"));
            }
            if !exists(
                con,
                "SELECT 1 FROM urls WHERE url_id=?",
                params![url_id.unwrap_or(0)],
            )? {
                return Err(scan_err("sitemap member references an unknown URL"));
            }
        }
        _ => {
            let complete = object["complete"].as_bool();
            if item_key != format!("url:{sid}")
                || complete.is_none()
                || !object["reason"].is_string()
                || !object["response_ids"].is_array()
            {
                return Err(scan_err("invalid sitemap expansion summary"));
            }
            let item_complete =
                item.get("completeness").and_then(Value::as_str) == Some("complete");
            if item_complete != complete.unwrap_or(false) {
                return Err(scan_err("sitemap expansion completeness disagrees"));
            }
            for response_id in object["response_ids"].as_array().into_iter().flatten() {
                let ok = match is_id(Some(response_id)) {
                    Some(id) => exists(
                        con,
                        "SELECT 1 FROM responses WHERE response_id=? AND purpose='sitemap'",
                        params![id],
                    )?,
                    None => false,
                };
                if !ok {
                    return Err(scan_err(
                        "sitemap expansion references an unavailable response",
                    ));
                }
            }
        }
    }
    if kind != "sitemap_declaration" {
        let known = match selected_roots {
            Some(roots) => roots.contains(&sid),
            // json_extract avoids re-parsing every declaration payload per validated item.
            None => exists(
                con,
                "SELECT 1 FROM context_items WHERE kind='sitemap_declaration' \
                 AND json_extract(payload_json,'$.sitemap_url_id')=?",
                params![sid],
            )?,
        };
        if !known {
            return Err(scan_err(
                "sitemap membership has no selected root declaration",
            ));
        }
    }
    Ok(())
}

/// Build one `context_items` envelope.
/// Defaults: `complete` is true and `reason` is empty.
pub fn context(
    kind: &str,
    key: &str,
    payload: &Value,
    complete: bool,
    reason: &str,
) -> Map<String, Value> {
    let mut item = Map::new();
    item.insert("kind".into(), json!(kind));
    item.insert("item_key".into(), json!(key));
    item.insert("payload_version".into(), json!("scan_context.v1"));
    item.insert("payload_json".into(), json!(dump(payload)));
    item.insert(
        "completeness".into(),
        json!(if complete { "complete" } else { "partial" }),
    );
    item.insert("reason".into(), json!(reason));
    item
}

/// Record one selected sitemap root and return its URL id.
pub fn declare(
    con: &Connection,
    sink: &dyn ContextSink,
    url: &str,
    source: &str,
    ordinal: i64,
) -> Result<i64, StoreError> {
    if url.is_empty()
        || !matches!(source, "explicit" | "robots" | "fallback" | "sitemap-only")
        || ordinal < 0
    {
        return Err(scan_err("invalid selected sitemap root"));
    }
    let key = format!("ordinal:{ordinal}");
    let existing = exists(
        con,
        "SELECT payload_json FROM context_items WHERE kind='sitemap_declaration' AND item_key=?",
        params![key],
    )?;
    let count: i64 = con.query_row(
        "SELECT COUNT(*) FROM context_items WHERE kind='sitemap_declaration'",
        [],
        |r| r.get(0),
    )?;
    if !existing && (ordinal != count || count >= MAX_SELECTED_ROOTS) {
        return Err(scan_err(
            "sitemap declaration order must append a bounded source sequence",
        ));
    }
    let sid = intern_url(con, url)?;
    // Membership check via json_extract avoids re-parsing every stored declaration per root.
    if !existing
        && exists(
            con,
            "SELECT 1 FROM context_items WHERE kind='sitemap_declaration' \
             AND json_extract(payload_json,'$.sitemap_url_id')=?",
            params![sid],
        )?
    {
        return Err(scan_err("selected sitemap roots must be unique"));
    }
    sink.put_context(
        con,
        &context(
            "sitemap_declaration",
            &key,
            &json!({"sitemap_url_id": sid, "source": source, "ordinal": ordinal}),
            true,
            "selected expanded sitemap root",
        ),
    )?;
    Ok(sid)
}

/// Write (or replace) the fetch summary of one sitemap root.
pub fn finish(con: &Connection, sid: i64, complete: bool, reason: &str) -> Result<(), StoreError> {
    let payload = json!({
        "sitemap_url_id": sid, "response_ids": [], "complete": complete, "reason": reason,
    });
    let item = context(
        "sitemap_fetch_summary",
        &format!("url:{sid}"),
        &payload,
        complete,
        reason,
    );
    let payload_back: Value = serde_json::from_str(item["payload_json"].as_str().unwrap_or("{}"))?;
    validate_context(con, &item, &payload_back, None)?;
    let key = item["item_key"].as_str().unwrap_or("");
    if exists(
        con,
        "SELECT 1 FROM context_items WHERE kind='sitemap_fetch_summary' AND item_key=?",
        params![key],
    )? {
        con.execute(
            "UPDATE context_items SET payload_json=?,completeness=?,reason=? WHERE kind='sitemap_fetch_summary' AND item_key=?",
            params![
                item["payload_json"].as_str(),
                item["completeness"].as_str(),
                reason,
                key
            ],
        )?;
    } else {
        insert(con, "context_items", &item)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(
            "CREATE TABLE urls(url_id INTEGER PRIMARY KEY,url TEXT UNIQUE);
             CREATE TABLE responses(response_id INTEGER PRIMARY KEY,purpose TEXT);
             CREATE TABLE context_items(kind TEXT,item_key TEXT,payload_version TEXT,payload_json TEXT,completeness TEXT,reason TEXT);
             INSERT INTO urls(url) VALUES('https://e.test/sitemap.xml');",
        )
        .unwrap();
        con
    }

    fn put(con: &Connection, key: &str, payload: Value) {
        let item = context("sitemap_declaration", key, &payload, true, "");
        con.execute(
            "INSERT INTO context_items VALUES('sitemap_declaration',?,?, ?, 'complete','')",
            params![key, "scan_context.v1", item["payload_json"].as_str()],
        )
        .unwrap();
    }

    #[test]
    fn duplicate_roots_are_refused() {
        let con = db();
        put(
            &con,
            "ordinal:0",
            json!({"sitemap_url_id": 1, "source": "robots", "ordinal": 0}),
        );
        put(
            &con,
            "ordinal:1",
            json!({"sitemap_url_id": 1, "source": "robots", "ordinal": 1}),
        );
        assert!(root_ids(&con).is_err());
    }

    #[test]
    fn declaration_source_and_order_are_checked() {
        let con = db();
        let good = json!({"sitemap_url_id": 1, "source": "robots", "ordinal": 0});
        let item = context("sitemap_declaration", "ordinal:0", &good, true, "");
        validate_context(&con, &item, &good, None).unwrap();
        let bad = json!({"sitemap_url_id": 1, "source": "guess", "ordinal": 0});
        assert!(validate_context(&con, &item, &bad, None).is_err());
        let bad_shape = json!({"sitemap_url_id": 1});
        assert!(validate_context(&con, &item, &bad_shape, None).is_err());
    }

    #[test]
    fn member_without_root_is_refused() {
        let con = db();
        let payload = json!({"sitemap_url_id": 1, "url_id": 1, "ordinal": 0});
        let item = context(
            "sitemap_declared_url",
            "sitemap:1:ordinal:0",
            &payload,
            true,
            "",
        );
        assert!(validate_context(&con, &item, &payload, None).is_err());
        let roots: BTreeSet<i64> = [1].into();
        validate_context(&con, &item, &payload, Some(&roots)).unwrap();
    }

    #[test]
    fn context_envelope_shape() {
        let item = context("k", "key", &json!({"b": 1, "a": 2}), false, "why");
        assert_eq!(item["payload_json"], json!("{\"a\": 2, \"b\": 1}"));
        assert_eq!(item["completeness"], json!("partial"));
    }
}

//! Append-only local history for bounded Cloudflare aggregate URL evidence.
//!
//! One private `cloudflare.sqlite` inside a project directory: a `cloudflare_meta`
//! marker row plus a `cloudflare_snapshots` table keyed by
//! `(zone_id, observed_at, fingerprint)`. Appends are atomic and idempotent.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::credentials::is_private_mode;
use crate::{pyjson, pystr};

mod storage;
use storage::*;

/// `FORMAT` — the history database identity.
pub const FORMAT: &str = "spiderhead.cloudflare-history.v1";
/// `DB_NAME` — the history file inside a project directory.
pub const DB_NAME: &str = "cloudflare.sqlite";
/// `MAX_SNAPSHOT_BYTES` — one serialized snapshot's UTF-8 budget (32 MiB).
pub const MAX_SNAPSHOT_BYTES: usize = 32 * 1024 * 1024;
/// `MAX_SNAPSHOTS` — retained-row ceiling.
pub const MAX_SNAPSHOTS: i64 = 1000;
/// `MAX_PAGE_BYTES` — a `list_snapshots` page's serialized budget.
pub const MAX_PAGE_BYTES: usize = 2 * 1024 * 1024;
/// `MAX_FACT_PAGE` — `facts_limit` ceiling.
pub const MAX_FACT_PAGE: i64 = 500;
/// `MAX_ROWS_WITH_FACTS` — `limit` ceiling when `include_facts` is set.
pub const MAX_ROWS_WITH_FACTS: i64 = 8;

/// Errors this module returns; driver failures keep their SQLite or I/O message.
#[derive(Debug, Error)]
pub enum CfhError {
    /// Invalid input.
    #[error("{0}")]
    Invalid(String),
    /// Stored JSON is corrupt.
    #[error("{0}")]
    JsonDecode(String),
    /// A required key is missing; the message names it.
    #[error("{0}")]
    KeyErr(String),
    /// Database driver failure.
    #[error("{0}")]
    Sqlite(#[from] rusqlite::Error),
    /// Filesystem error.
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

impl CfhError {
    /// Error class name reported to callers.
    pub fn python_class(&self) -> &'static str {
        match self {
            CfhError::Invalid(_) => "ValueError",
            CfhError::JsonDecode(_) => "JSONDecodeError",
            CfhError::KeyErr(_) => "KeyError",
            CfhError::Sqlite(err) => match err {
                rusqlite::Error::SqliteFailure(code, _) => match code.code {
                    rusqlite::ErrorCode::NotADatabase
                    | rusqlite::ErrorCode::DatabaseCorrupt
                    | rusqlite::ErrorCode::TypeMismatch => "DatabaseError",
                    _ => "OperationalError",
                },
                rusqlite::Error::ToSqlConversionFailure(_) => "InterfaceError",
                _ => "OperationalError",
            },
            CfhError::Io(err) => match err.kind() {
                std::io::ErrorKind::NotFound => "FileNotFoundError",
                std::io::ErrorKind::AlreadyExists => "FileExistsError",
                std::io::ErrorKind::PermissionDenied => "PermissionError",
                _ => "OSError",
            },
        }
    }
}

/// Append one aggregate snapshot without retaining IP addresses or changing older
/// snapshots.
pub fn save_snapshot(
    result: &Value,
    project: &Value,
    observed_at: Option<&Value>,
) -> Result<Value, CfhError> {
    if !pystr::truthy(result.get("ok")) || !pystr::truthy(result.get("zone_id")) {
        return invalid("a successful Cloudflare result with zone ID is required");
    }
    let payload = json!({
        "url_facts": result.get("url_facts").cloned().unwrap_or_else(|| json!([])),
        "url_facts_truncated": pystr::truthy(result.get("url_facts_truncated")),
        "url_facts_omitted": result.get("url_facts_omitted").cloned().unwrap_or_else(|| json!(0)),
        "requests": result.get("requests").cloned().unwrap_or(Value::Null),
        "sampled": result.get("sampled").cloned().unwrap_or(Value::Null),
        "verification": result.get("verification").cloned().unwrap_or(Value::Null),
        "note": result.get("note").cloned().unwrap_or(Value::Null),
    });
    let encoded = compact_dumps(&payload);
    if encoded.len() > MAX_SNAPSHOT_BYTES {
        return invalid("Cloudflare history snapshot exceeds 32 MiB");
    }
    let truncated_days = result
        .get("truncated_days")
        .filter(|v| pystr::truthy(Some(v)))
        .cloned()
        .unwrap_or_else(|| json!([]));
    let facts_truncated = pystr::truthy(result.get("url_facts_truncated"));
    let state = if pystr::truthy(result.get("sampled"))
        || !truncated_days
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(true)
        || facts_truncated
    {
        "partial"
    } else {
        "complete"
    };
    let stamp = match observed_at.filter(|v| pystr::truthy(Some(v))) {
        Some(v) => v.as_str().unwrap_or_default().to_owned(),
        None => crate::pytime::isoformat_seconds(crate::pytime::now_micros()),
    };
    let fingerprint = {
        let mut hasher = Sha256::new();
        hasher.update(encoded.as_bytes());
        format!("{:x}", hasher.finalize())
    };
    let path = path_arg(project)?;
    let zone_id = member(result, "zone_id")?
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let zone = member(result, "zone")?
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let period = member(result, "period")?;
    let period_from = member(period, "from")?
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let period_to = member(period, "to")?
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let con = connect(&path, true)?;
    let result_tx = (|| -> Result<i64, CfhError> {
        let count: i64 = con.query_row("SELECT COUNT(*) FROM cloudflare_snapshots", [], |r| {
            r.get(0)
        })?;
        let prior: Option<i64> = con
            .query_row(
                "SELECT id FROM cloudflare_snapshots WHERE zone_id=? AND observed_at=? AND fingerprint=?",
                rusqlite::params![zone_id, stamp, fingerprint],
                |r| r.get(0),
            )
            .optional()?;
        let snapshot_id = match prior {
            Some(id) => id,
            None => {
                if count >= MAX_SNAPSHOTS {
                    return invalid("Cloudflare history reached the 1000-snapshot limit");
                }
                con.execute(
                "INSERT INTO cloudflare_snapshots
                 (zone_id,zone,observed_at,fingerprint,period_from,period_to,state,sampled,truncated_days,payload)
                 VALUES(?,?,?,?,?,?,?,?,?,?)",
                rusqlite::params![
                    zone_id,
                    zone,
                    stamp,
                    fingerprint,
                    period_from,
                    period_to,
                    state,
                    i64::from(pystr::truthy(result.get("sampled"))),
                    crate::pyjson::dumps(&truncated_days),
                    encoded,
                ],
            )?;
                con.query_row(
                "SELECT id FROM cloudflare_snapshots WHERE zone_id=? AND observed_at=? AND fingerprint=?",
                rusqlite::params![zone_id, stamp, fingerprint],
                |r| r.get(0),
            )?
            }
        };
        Ok(snapshot_id)
    })();
    let snapshot_id = match result_tx {
        Ok(id) => id,
        Err(e) => {
            let _ = con.execute_batch("ROLLBACK");
            return Err(e);
        }
    };
    con.execute_batch("COMMIT")?;
    Ok(json!({"state": "stored", "snapshot_id": snapshot_id, "snapshot_state": state}))
}

/// Accept only JSON integers, not booleans or floats.
fn is_py_int(value: &Value) -> bool {
    pystr::is_py_int(value)
}

fn truncated_json_days(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::Null)
}

/// Read one bounded page of snapshots, optionally with their facts.
pub fn list_snapshots(args: &Value) -> Result<Value, CfhError> {
    let get = |key: &str| args.get(key).cloned().unwrap_or(Value::Null);
    let project = get("project");
    let zone_id = get("zone_id");
    let snapshot_id = get("snapshot_id");
    let limit = get("limit");
    let offset = get("offset");
    let include_facts = get("include_facts");
    let facts_offset = get("facts_offset");
    let facts_limit = get("facts_limit");
    let limit = if limit.is_null() { json!(100) } else { limit };
    let offset = if offset.is_null() { json!(0) } else { offset };
    let facts_offset = if facts_offset.is_null() {
        json!(0)
    } else {
        facts_offset
    };
    let facts_limit = if facts_limit.is_null() {
        json!(100)
    } else {
        facts_limit
    };
    if !is_py_int(&limit)
        || !(1..=100).contains(&limit.as_i64().unwrap_or(0))
        || !is_py_int(&offset)
        || offset.as_i64().unwrap_or(-1) < 0
    {
        return invalid("limit must be 1..100 and offset must be nonnegative");
    }
    if !include_facts.is_boolean() && !include_facts.is_null() {
        return invalid("include_facts must be a boolean");
    }
    let include_facts_b = include_facts.as_bool().unwrap_or(false);
    if !is_py_int(&facts_offset) || facts_offset.as_i64().unwrap_or(-1) < 0 {
        return invalid("facts_offset must be nonnegative");
    }
    if !is_py_int(&facts_limit) || !(1..=MAX_FACT_PAGE).contains(&facts_limit.as_i64().unwrap_or(0))
    {
        return invalid(format!("facts_limit must be 1..{MAX_FACT_PAGE}"));
    }
    if !snapshot_id.is_null() && (!is_py_int(&snapshot_id) || snapshot_id.as_i64().unwrap_or(0) < 1)
    {
        return invalid("snapshot_id must be positive");
    }
    let mut limit_i = limit.as_i64().unwrap_or(0);
    let offset_i = offset.as_i64().unwrap_or(0);
    let facts_offset_i = facts_offset.as_i64().unwrap_or(0);
    let facts_limit_i = facts_limit.as_i64().unwrap_or(0);
    if include_facts_b && limit_i > MAX_ROWS_WITH_FACTS {
        limit_i = MAX_ROWS_WITH_FACTS;
    }
    let path = path_arg(&project)?;
    let con = connect(&path, false)?;
    let mut filters = String::new();
    let mut params: Vec<rusqlite::types::Value> = Vec::new();
    if pystr::truthy(Some(&zone_id)) {
        filters.push_str("zone_id=?");
        params.push(rusqlite::types::Value::Text(
            zone_id.as_str().unwrap_or_default().to_owned(),
        ));
    }
    if !snapshot_id.is_null() {
        if !filters.is_empty() {
            filters.push_str(" AND ");
        }
        filters.push_str("id=?");
        params.push(rusqlite::types::Value::Integer(
            snapshot_id.as_i64().unwrap_or(0),
        ));
    }
    let clause = if filters.is_empty() {
        String::new()
    } else {
        format!(" WHERE {filters}")
    };
    let total: i64 = con
        .prepare(&format!(
            "SELECT COUNT(*) FROM cloudflare_snapshots{clause}"
        ))?
        .query_row(rusqlite::params_from_iter(params.iter()), |row| row.get(0))?;
    let payload_column = if include_facts_b {
        ",payload"
    } else {
        ",length(payload)"
    };
    let mut stmt = con.prepare(&format!(
        "SELECT id,zone_id,zone,observed_at,period_from,period_to,state,sampled,truncated_days\
         {payload_column} FROM cloudflare_snapshots{clause} ORDER BY id DESC LIMIT ? OFFSET ?"
    ))?;
    params.push(rusqlite::types::Value::Integer(limit_i));
    params.push(rusqlite::types::Value::Integer(offset_i));
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, String>(8)?,
                if include_facts_b {
                    rusqlite::types::Value::Text(row.get::<_, String>(9)?)
                } else {
                    rusqlite::types::Value::Integer(row.get::<_, i64>(9)?)
                },
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut result_rows = Vec::new();
    let mut page_bytes = 0usize;
    let mut byte_limited = false;
    for row in &rows {
        let payload: Option<Value> = if include_facts_b {
            let text = match &row.9 {
                rusqlite::types::Value::Text(t) => t.clone(),
                _ => String::new(),
            };
            serde_json::from_str(&text).map_err(|e| {
                CfhError::JsonDecode(pyjson::decode_error(&text).unwrap_or_else(|| e.to_string()))
            })?
        } else {
            None
        };
        let all_facts = payload
            .as_ref()
            .and_then(|p| p.get("url_facts"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let stored_omitted = payload.as_ref().map(|p| {
            p.get("url_facts_omitted")
                .cloned()
                .unwrap_or_else(|| json!(0))
        });
        let stored_omitted = stored_omitted.and_then(|v| {
            if is_py_int(&v) && v.as_i64().unwrap_or(-1) >= 0 {
                Some(v.as_i64().unwrap_or(0))
            } else {
                None
            }
        });
        let mut facts = Vec::new();
        if include_facts_b {
            for item in all_facts
                .iter()
                .skip(facts_offset_i.max(0) as usize)
                .take(facts_limit_i.max(0) as usize)
            {
                if !item.is_object() {
                    continue;
                }
                let get_s = |key: &str, default: &str, cap: usize| {
                    item.get(key)
                        .map(|v| pyjson::str_value(v).unwrap_or_else(|| default.to_owned()))
                        .unwrap_or_else(|| default.to_owned())
                        .chars()
                        .take(cap)
                        .collect::<String>()
                };
                let int_or_null = |key: &str| {
                    item.get(key)
                        .filter(|v| is_py_int(v))
                        .cloned()
                        .unwrap_or(Value::Null)
                };
                let requests = item.get("requests").cloned().unwrap_or(Value::Null);
                facts.push(json!({
                    "host": get_s("host", "unknown", 255),
                    "path": get_s("path", "/", 2048),
                    "date": get_s("date", "", 10),
                    "bot_family": get_s("bot_family", "unknown", 64),
                    "bot_name": get_s("bot_name", "unknown", 128),
                    "verified_bot_category": item
                        .get("verified_bot_category")
                        .and_then(Value::as_str)
                        .map(|s| Value::String(s.chars().take(128).collect::<String>()))
                        .unwrap_or(Value::Null),
                    "edge_status": int_or_null("edge_status"),
                    "origin_status": int_or_null("origin_status"),
                    "cache_status": get_s("cache_status", "unknown", 64),
                    "edge_redirect": pystr::truthy(item.get("edge_redirect")),
                    "requests": if is_py_int(&requests) && requests.as_i64().unwrap_or(-1) >= 0 {
                        requests
                    } else {
                        Value::Null
                    },
                }));
            }
        }
        let mut entry = json!({
            "snapshot_id": row.0,
            "zone_id": row.1,
            "zone": row.2,
            "observed_at": row.3,
            "period": {"from": row.4, "to": row.5},
            "state": row.6,
            "sampled": row.7 != 0,
            "truncated_days": truncated_json_days(&row.8),
            "url_facts_available": true,
        });
        if include_facts_b {
            let p = payload.as_ref().cloned().unwrap_or_default();
            let src_truncated = pystr::truthy(p.get("url_facts_truncated"));
            let next_offset = if facts_offset_i + (facts.len() as i64) < all_facts.len() as i64 {
                json!(facts_offset_i + facts.len() as i64)
            } else {
                Value::Null
            };
            entry.as_object_mut().unwrap().extend([
                ("url_facts".to_owned(), Value::Array(facts.clone())),
                (
                    "url_facts_source_truncated".to_owned(),
                    json!(src_truncated),
                ),
                (
                    "url_facts_truncated".to_owned(),
                    json!(src_truncated || stored_omitted.map(|n| n != 0).unwrap_or(false)),
                ),
                (
                    "url_facts_omitted".to_owned(),
                    stored_omitted.map_or(Value::Null, |n| json!(n)),
                ),
                (
                    "url_facts_total_retained".to_owned(),
                    json!(all_facts.len()),
                ),
                ("url_facts_offset".to_owned(), json!(facts_offset_i)),
                ("url_facts_next_offset".to_owned(), next_offset),
                (
                    "url_facts_page_omitted".to_owned(),
                    json!(std::cmp::max(
                        0,
                        all_facts.len() as i64 - facts_offset_i - facts.len() as i64
                    )),
                ),
            ]);
        } else {
            let size = match &row.9 {
                rusqlite::types::Value::Integer(n) => *n,
                _ => 0,
            };
            entry["url_facts_payload_bytes"] = json!(size);
        }
        let size = compact_dumps(&entry).len();
        if page_bytes + size > MAX_PAGE_BYTES - 4096 {
            byte_limited = true;
            break;
        }
        result_rows.push(entry);
        page_bytes += size;
    }
    let has_more = (offset_i + result_rows.len() as i64) < total;
    Ok(json!({
        "ok": true,
        "state": if has_more { "partial" } else { "complete" },
        "source": "cloudflare-history",
        "schema_version": FORMAT,
        "total": total,
        "offset": offset_i,
        "limit": result_rows.len(),
        "has_more": has_more,
        "next_offset": if has_more { json!(offset_i + result_rows.len() as i64) } else { Value::Null },
        "truncated_by_byte_limit": byte_limited,
        "facts_included": include_facts_b,
        "facts_page": {"offset": facts_offset_i, "limit": facts_limit_i},
        "rows": result_rows,
    }))
}

/// Distinct locally retained zone IDs, one bounded page.
pub fn list_zones(
    project: &Value,
    limit: Option<&Value>,
    offset: Option<&Value>,
) -> Result<Value, CfhError> {
    let default_limit = json!(100);
    let default_offset = json!(0);
    let limit = limit.unwrap_or(&default_limit);
    let offset = offset.unwrap_or(&default_offset);
    if !is_py_int(limit)
        || !(1..=100).contains(&limit.as_i64().unwrap_or(0))
        || !is_py_int(offset)
        || offset.as_i64().unwrap_or(-1) < 0
    {
        return invalid("limit must be 1..100 and offset must be nonnegative");
    }
    let path = path_arg(project)?;
    let con = connect(&path, false)?;
    let total: i64 = con.query_row(
        "SELECT COUNT(DISTINCT zone_id) FROM cloudflare_snapshots",
        [],
        |r| r.get(0),
    )?;
    let zones: Vec<String> = con
        .prepare(
            "SELECT zone_id FROM cloudflare_snapshots GROUP BY zone_id \
             ORDER BY MAX(id) DESC LIMIT ? OFFSET ?",
        )?
        .query_map(
            rusqlite::params![limit.as_i64().unwrap_or(0), offset.as_i64().unwrap_or(0)],
            |row| row.get(0),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    let mut result = Vec::new();
    for zone_id in zones {
        let row = con.query_row(
            "SELECT zone,observed_at,period_from,period_to,state,sampled,truncated_days,payload \
             FROM cloudflare_snapshots WHERE zone_id=? ORDER BY id DESC LIMIT 1",
            rusqlite::params![zone_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                ))
            },
        )?;
        let days: Value = serde_json::from_str(&row.6).unwrap_or(Value::Null);
        let payload: Value = serde_json::from_str(&row.7).unwrap_or(Value::Null);
        result.push(json!({
            "zone_id": zone_id,
            "zone": row.0,
            "latest_observed_at": row.1,
            "latest_period": {"from": row.2, "to": row.3},
            "state": row.4,
            "sampled": row.5 != 0,
            "truncated": pystr::truthy(Some(&days))
                || pystr::truthy(payload.get("url_facts_truncated")),
        }));
    }
    Ok(json!({
        "schema_version": FORMAT,
        "total": total,
        "offset": offset.as_i64().unwrap_or(0),
        "limit": limit.as_i64().unwrap_or(0),
        "zones": result,
    }))
}

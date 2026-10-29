//! Private local URL Inspection history, plus the
//! per-property rolling daily/minute request budget ledger.
//!
//! Two SQLite files: `<project>/gsc-inspection.sqlite` (the history, format
//! `spiderhead.gsc-inspection-history.v1`) and `<config root>/spiderhead/
//! gsc-inspection-quota.sqlite` (reservations, format `…-quota.v2`). No network
//! calls happen here; `reserve` runs before network access so interrupted runs
//! still count toward budget.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::{json, Value};
use thiserror::Error;

use crate::credentials::is_private_mode;
use crate::gsc::py_truthy;
use crate::{pyjson, pystr, pytime};

mod storage;
use storage::*;

/// `FORMAT` — the history database identity.
pub const FORMAT: &str = "spiderhead.gsc-inspection-history.v1";
/// `QUOTA_FORMAT` — the reservation ledger identity.
pub const QUOTA_FORMAT: &str = "spiderhead.gsc-inspection-quota.v2";
/// `DB_NAME` — the history file inside a project directory.
pub const DB_NAME: &str = "gsc-inspection.sqlite";
/// `MAX_DAILY_CALLS` — per-property rolling 24-hour call budget.
pub const MAX_DAILY_CALLS: i64 = 2000;
/// `MAX_MINUTE_CALLS` — per-property rolling minute budget.
pub const MAX_MINUTE_CALLS: i64 = 600;
/// `MAX_BATCH` — URLs one reservation may cover.
pub const MAX_BATCH: usize = 50;
/// `MAX_RECORDS` — history row cap.
pub const MAX_RECORDS: i64 = 100_000;
/// `MAX_RECORD_BYTES` — one serialized result section cap.
pub const MAX_RECORD_BYTES: usize = 32_768;
/// `MAX_PAGE_ROWS` — `latest` page size cap.
pub const MAX_PAGE_ROWS: i64 = 250;
/// `MAX_PAGE_BYTES` — `latest` response byte budget.
pub const MAX_PAGE_BYTES: usize = 2 * 1024 * 1024;
/// `MAX_RESERVATION_URLS` — candidate list cap.
pub const MAX_RESERVATION_URLS: usize = 100_000;
/// `MAX_URL_BYTES` — one URL's UTF-8 length cap.
pub const MAX_URL_BYTES: usize = 8_192;
/// `MAX_RESERVATION_BYTES` — serialized unique-URL population cap (1 MiB).
pub const MAX_RESERVATION_BYTES: usize = 1024 * 1024;
/// `MAX_SETTLEMENT_BYTES` — serialized settlement payload cap (2 MiB).
pub const MAX_SETTLEMENT_BYTES: usize = 2 * 1024 * 1024;

/// The errors this module returns; driver failures surface as I/O or driver errors.
#[derive(Debug, Error)]
pub enum HistoryError {
    /// Invalid input or a rejected state.
    #[error("{0}")]
    Invalid(String),
    /// Stored JSON is corrupt; the error is not masked.
    #[error("{0}")]
    JsonDecode(String),
    /// SQLite driver failure, carried as its message.
    #[error("{0}")]
    Sqlite(#[from] rusqlite::Error),
    /// Filesystem failure.
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

impl HistoryError {
    /// Stable error class name reported to callers.
    pub fn python_class(&self) -> &'static str {
        match self {
            HistoryError::Invalid(_) => "ValueError",
            HistoryError::JsonDecode(_) => "JSONDecodeError",
            HistoryError::Sqlite(_) => "sqlite3.Error",
            HistoryError::Io(err) => match err.kind() {
                std::io::ErrorKind::NotFound => "FileNotFoundError",
                std::io::ErrorKind::AlreadyExists => "FileExistsError",
                std::io::ErrorKind::PermissionDenied => "PermissionError",
                _ => "OSError",
            },
        }
    }
}

/// UTC timestamp in whole seconds with a `Z` suffix, used for `reserve` stamps.
fn utc_stamp(micros: i64) -> String {
    let text = pytime::isoformat_seconds(micros);
    text.strip_suffix("+00:00")
        .map(|s| format!("{s}Z"))
        .unwrap_or(text)
}

/// UTC timestamp with microseconds and a `Z` suffix, used for `settle`'s `inspected_at`.
fn utc_stamp_full(micros: i64) -> String {
    let text = pytime::isoformat(micros);
    text.strip_suffix("+00:00")
        .map(|s| format!("{s}Z"))
        .unwrap_or(text)
}

fn utc_shift(micros: i64, seconds: i64) -> String {
    utc_stamp(micros + seconds * 1_000_000)
}

/// `reserve` — reserve a capped subset before network access.
///
/// `root` is the config root holding the quota ledger. The `project` argument is accepted for call-site
/// compatibility and is not used.
/// `now` is a UTC instant as epoch microseconds; `None` reads the wall clock.
/// `reservation_id` may be injected for deterministic tests; otherwise a random UUID is used.
pub fn reserve(
    root: &Path,
    _project: &Path,
    site_url: &str,
    urls: Option<&[Value]>,
    now: Option<i64>,
    reservation_id: Option<&str>,
) -> Result<Value, HistoryError> {
    if site_url.is_empty() || site_url.len() > 2048 || urls.is_none() {
        return invalid("site_url and urls are required");
    }
    let urls = urls.unwrap_or_default();
    let urls_valid = urls.len() <= MAX_RESERVATION_URLS
        && urls.iter().all(|u| {
            u.as_str()
                .map(|s| !s.is_empty() && s.len() <= MAX_URL_BYTES)
                .unwrap_or(false)
        });
    if !urls_valid {
        return invalid("inspection URLs must be a bounded nonempty string list");
    }
    // Deduplicate keeping the first occurrence, so order is preserved.
    let mut seen = std::collections::BTreeSet::new();
    let unique: Vec<&str> = urls
        .iter()
        .filter_map(Value::as_str)
        .filter(|u| seen.insert(*u))
        .collect();
    if pyjson::dumps_opts(&json!(unique), false, false).len() > MAX_RESERVATION_BYTES {
        return invalid("inspection URL population exceeds the 1 MiB reservation limit");
    }
    let current = now.unwrap_or_else(pytime::now_micros);
    let path = quota_path(root)?;
    let reservation_id = reservation_id.map(str::to_owned).unwrap_or_else(uuid4);
    let con = connect_quota(&path)?;
    let result = (|| -> Result<Value, HistoryError> {
        con.execute(
            "DELETE FROM reservations WHERE reserved_at<?",
            [utc_shift(current, -30 * 86400)],
        )?;
        let used: i64 = con.query_row(
            "SELECT COALESCE(SUM(count),0) FROM reservations WHERE site_url=? AND state!='released' AND reserved_at>=?",
            rusqlite::params![site_url, utc_shift(current, -86400)],
            |row| row.get(0),
        )?;
        let minute_used: i64 = con.query_row(
            "SELECT COALESCE(SUM(count),0) FROM reservations WHERE site_url=? AND state!='released' AND reserved_at>=?",
            rusqlite::params![site_url, utc_shift(current, -60)],
            |row| row.get(0),
        )?;
        let allowed = (unique.len() as i64)
            .min(MAX_BATCH as i64)
            .min((MAX_DAILY_CALLS - used).max(0))
            .min((MAX_MINUTE_CALLS - minute_used).max(0)) as usize;
        let selected: Vec<&str> = unique.iter().take(allowed).copied().collect();
        if !selected.is_empty() {
            con.execute(
                "INSERT INTO reservations(id,site_url,reserved_at,count,state,urls) VALUES(?,?,?,?,'reserved',?)",
                rusqlite::params![
                    reservation_id,
                    site_url,
                    utc_stamp(current),
                    selected.len() as i64,
                    pyjson::dumps_opts_compact(&json!(selected), false, false),
                ],
            )?;
        }
        con.execute_batch("COMMIT")?;
        Ok(json!({
            "reservation_id": if selected.is_empty() { Value::Null } else { json!(reservation_id) },
            "site_url": site_url,
            "day": utc_stamp(current)[..10],
            "urls": selected,
            "requested": unique.len(),
            "reserved": selected.len(),
            "used_before": used,
            "remaining_after": MAX_DAILY_CALLS - used - selected.len() as i64,
            "minute_remaining_after": MAX_MINUTE_CALLS - minute_used - selected.len() as i64,
            "deferred": unique.len() - selected.len(),
            "budget_window": "rolling 24 hours and rolling minute",
            "shared_provider_usage": "unknown; this ledger counts only calls through this machine",
        }))
    })();
    if result.is_err() {
        let _ = con.execute_batch("ROLLBACK");
    }
    result
}

/// `settle` — settle a reservation with the outcomes the provider actually produced.
pub fn settle(
    root: &Path,
    project: &Path,
    reservation_id: &str,
    outcomes: Option<&[Value]>,
    requests_made: Option<&Value>,
    now: Option<i64>,
) -> Result<Value, HistoryError> {
    let requests_made = match requests_made.and_then(|v| pystr::as_strict_i64(Some(v))) {
        Some(n) if n >= 0 => n,
        _ => return invalid("invalid inspection settlement"),
    };
    let Some(outcomes) = outcomes else {
        return invalid("invalid inspection settlement");
    };
    let timestamp = utc_stamp_full(now.unwrap_or_else(pytime::now_micros));
    let quota = connect_quota(&quota_path(root)?)?;
    let result = (|| -> Result<Value, HistoryError> {
        let row: Option<(String, i64, String, String)> = quota
            .query_row(
                "SELECT site_url,count,state,urls FROM reservations WHERE id=?",
                [reservation_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .ok();
        let Some((site_url, reserved, state, urls_json)) = row else {
            return invalid("unknown or already settled inspection reservation");
        };
        if state != "reserved" || requests_made > reserved {
            return invalid("unknown or already settled inspection reservation");
        }
        if requests_made == 0 {
            if !outcomes.is_empty() {
                return invalid("zero-request settlement cannot contain inspection outcomes");
            }
            quota.execute(
                "UPDATE reservations SET state='released' WHERE id=?",
                [reservation_id],
            )?;
            quota.execute_batch("COMMIT")?;
            return Ok(json!({"state": "released", "stored": 0}));
        }
        if outcomes.len() as i64 > requests_made {
            return invalid("provider returned more inspection rows than requests");
        }
        let reserved_urls: Option<Vec<String>> = serde_json::from_str(&urls_json).ok();
        let valid = reserved_urls
            .as_ref()
            .map(|list| list.iter().all(|u| !u.is_empty()))
            .unwrap_or(false);
        if !valid {
            quota.execute(
                "UPDATE reservations SET state='unbound_legacy' WHERE id=?",
                [reservation_id],
            )?;
            quota.execute_batch("COMMIT")?;
            return Ok(json!({"state": "unbound_reservation", "stored": 0}));
        }
        let allowed: std::collections::BTreeSet<&str> =
            reserved_urls.iter().flatten().map(String::as_str).collect();
        let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        let mut rows: Vec<(String, String, String, String, String)> = Vec::new();
        let mut total_bytes = 0usize;
        for item in outcomes {
            let Some(item) = item.as_object() else {
                return invalid("inspection outcome must be an object");
            };
            let url = item.get("url").and_then(Value::as_str).unwrap_or("");
            if url.is_empty()
                || url.len() > MAX_URL_BYTES
                || !allowed.contains(url)
                || !seen.insert(url)
            {
                return invalid("inspection outcome URL is outside the reserved population");
            }
            total_bytes += url.len();
            if total_bytes > MAX_SETTLEMENT_BYTES {
                return invalid("inspection settlement exceeds its byte limit");
            }
            let state = item.get("state").and_then(Value::as_str).unwrap_or("");
            if !matches!(state, "complete" | "failed") {
                return invalid("inspection outcome state is invalid");
            }
            let mut fields: Vec<String> = Vec::new();
            for key in ["index_status", "amp_result", "rich_results_result"] {
                // A falsy value is treated as an empty object.
                let section = match item.get(key) {
                    Some(v) if py_truthy(v) => v.clone(),
                    _ => json!({}),
                };
                if !section.is_object() {
                    return invalid("inspection result sections must be objects");
                }
                let encoded = pyjson::dumps_opts_compact(&section, true, false);
                if encoded.len() > MAX_RECORD_BYTES {
                    return invalid("inspection result section exceeds its byte limit");
                }
                total_bytes += encoded.len();
                if total_bytes > MAX_SETTLEMENT_BYTES {
                    return invalid("inspection settlement exceeds its byte limit");
                }
                fields.push(encoded);
            }
            rows.push((
                url.to_owned(),
                state.to_owned(),
                fields[0].clone(),
                fields[1].clone(),
                fields[2].clone(),
            ));
        }
        let history = connect(&history_path(project)?, true)?;
        let stored = (|| -> Result<i64, HistoryError> {
            let total: i64 =
                history.query_row("SELECT COUNT(*) FROM inspections", [], |r| r.get(0))?;
            if total + rows.len() as i64 > MAX_RECORDS {
                history.execute_batch("ROLLBACK")?;
                quota.execute(
                    "UPDATE reservations SET state='retained_unstored' WHERE id=?",
                    [reservation_id],
                )?;
                quota.execute_batch("COMMIT")?;
                return Ok(-1);
            }
            for (url, state, index, amp, rich) in &rows {
                history.execute(
                    "INSERT INTO inspections(site_url,url,inspected_at,state,index_status,amp_result,rich_results_result) VALUES(?,?,?,?,?,?,?)",
                    rusqlite::params![site_url, url, timestamp, state, index, amp, rich],
                )?;
            }
            history.execute_batch("COMMIT")?;
            Ok(rows.len() as i64)
        })()?;
        if stored < 0 {
            return Ok(json!({"state": "history_full", "stored": 0}));
        }
        quota.execute(
            "UPDATE reservations SET state='complete' WHERE id=?",
            [reservation_id],
        )?;
        quota.execute_batch("COMMIT")?;
        Ok(json!({"state": "stored", "stored": stored}))
    })();
    if result.is_err() {
        let _ = quota.execute_batch("ROLLBACK");
    }
    result
}

/// `latest` — the newest record per URL, paged by row count and a byte budget.
pub fn latest(
    project: &Path,
    site_url: &str,
    limit: Option<&Value>,
    offset: Option<&Value>,
    since: Option<&str>,
) -> Result<Value, HistoryError> {
    let limit = match limit {
        None => 100,
        Some(v) => match pystr::as_strict_i64(Some(v)) {
            Some(n) if (1..=MAX_PAGE_ROWS).contains(&n) => n,
            _ => return invalid(format!("limit must be 1..{MAX_PAGE_ROWS}")),
        },
    };
    let offset = match offset {
        None => 0,
        Some(v) => match pystr::as_strict_i64(Some(v)) {
            Some(n) if n >= 0 => n,
            _ => return invalid("offset must be a nonnegative integer"),
        },
    };
    if site_url.is_empty() || site_url.len() > 2048 {
        return invalid("site_url must be a bounded nonempty string");
    }
    let path = history_path(project)?;
    let con = connect(&path, false)?;
    let result = (|| -> Result<Value, HistoryError> {
        let mut where_sql = "site_url=?".to_owned();
        let mut params: Vec<rusqlite::types::Value> =
            vec![rusqlite::types::Value::Text(site_url.to_owned())];
        if let Some(since) = since {
            // Parse the `since` timestamp; the error message quotes the normalized string.
            let normalized = since.replace('Z', "+00:00");
            if pytime::fromisoformat(&normalized).is_none() {
                return invalid(format!(
                    "Invalid isoformat string: {}",
                    pystr::repr_str(&normalized)
                ));
            }
            where_sql.push_str(" AND inspected_at>=?");
            params.push(rusqlite::types::Value::Text(since.to_owned()));
        }
        let total: i64 = con.query_row(
            &format!("SELECT COUNT(DISTINCT url) FROM inspections WHERE {where_sql}"),
            rusqlite::params_from_iter(params.iter()),
            |r| r.get(0),
        )?;
        params.push(rusqlite::types::Value::Integer(limit));
        params.push(rusqlite::types::Value::Integer(offset));
        let mut stmt = con.prepare(&format!(
            "SELECT url,inspected_at,state,index_status,amp_result,rich_results_result FROM (
             SELECT url,inspected_at,state,index_status,amp_result,rich_results_result,id,
             ROW_NUMBER() OVER(PARTITION BY url ORDER BY inspected_at DESC,id DESC) AS rank
             FROM inspections WHERE {where_sql}) WHERE rank=1
             ORDER BY inspected_at DESC,url LIMIT ? OFFSET ?"
        ))?;
        let mut selected: Vec<Value> = Vec::new();
        let mut page_bytes = 0usize;
        let mut has_more = false;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?;
        // Empty stored values become `{}`; corrupt stored JSON is an error, not masked.
        let loads = |text: Option<String>| -> Result<Value, HistoryError> {
            let source = match text.as_deref() {
                Some(s) if !s.is_empty() => s.to_owned(),
                _ => "{}".to_owned(),
            };
            serde_json::from_str(&source).map_err(|err| HistoryError::JsonDecode(err.to_string()))
        };
        for row in rows {
            let (url, inspected_at, state, index, amp, rich) = row?;
            let record = json!({
                "url": url,
                "inspected_at": inspected_at,
                "state": state,
                "index_status": loads(index)?,
                "amp_result": loads(amp)?,
                "rich_results_result": loads(rich)?,
            });
            let size = pyjson::dumps_opts_compact(&record, false, false).len();
            if page_bytes + size > MAX_PAGE_BYTES - 4096 {
                has_more = true;
                break;
            }
            page_bytes += size;
            selected.push(record);
        }
        has_more = has_more || (offset + selected.len() as i64) < total;
        Ok(json!({
            "schema_version": FORMAT,
            "site_url": site_url,
            "total": total,
            "offset": offset,
            "limit": limit,
            "has_more": has_more,
            "next_offset": if has_more { json!(offset + selected.len() as i64) } else { Value::Null },
            "truncated_by_byte_limit": page_bytes >= MAX_PAGE_BYTES
                || (has_more && (selected.len() as i64) < limit),
            "rows": selected,
        }))
    })();
    if result.is_err() {
        let _ = con.execute_batch("ROLLBACK");
    }
    result
}

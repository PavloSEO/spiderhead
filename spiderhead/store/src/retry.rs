//! Explicit scan.v2 retry/requeue storage; reading a scan never upgrades it.

#![allow(clippy::type_complexity)]

use crate::error::StoreError;
use crate::scan_artifact::{open_scan, ScanError, APPLICATION_ID};
use indexmap::IndexMap;
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Map, Value};
use spiderhead_core::artifacts::{SCAN_V1, SCAN_V2};
use spiderhead_core::filesystem::file_sha256;
use spiderhead_core::sqlite::open_readonly;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

pub const V2_USER_VERSION: i64 = 2;

pub const V2_FORMAT: &str = "scan.v2";

pub const WHERE_FIELDS: &[(&str, &str)] = &[
    ("url", "u.url"),
    ("status_code", "p.status_code"),
    ("content_type", "p.content_type"),
    ("crawl_depth", "p.crawl_depth"),
    ("representation", "p.representation"),
    ("page_ordinal", "p.page_ordinal"),
];

/// Regex source of one `field op value` predicate, anchored at the end of the text.
pub const WHERE: &str = r"\s*([a-z_]+)\s*(=|!=|<=|>=|<|>)\s*(.+?)\s*\Z";

/// Compile a small, value-bound predicate instead of accepting SQL text.
/// Returns the SQL clause, its bound values and the recorded (field, operator, value) entries.
pub fn predicate(
    where_: &str,
) -> Result<
    (
        String,
        Vec<rusqlite::types::Value>,
        Vec<IndexMap<String, Value>>,
    ),
    StoreError,
> {
    let invalid = |m: &str| StoreError::Invalid(m.to_owned());
    if where_.is_empty() || where_.chars().count() > 2048 {
        return Err(invalid(
            "where must be a nonempty predicate under 2048 characters",
        ));
    }
    let and_split = and_regex().ok_or_else(|| invalid("where is not a valid predicate"))?;
    let pieces: Vec<&str> = and_split
        .split(where_)
        .collect::<Result<_, _>>()
        .map_err(|_| invalid("where is not a valid predicate"))?;
    if !(1..=16).contains(&pieces.len()) {
        return Err(invalid(
            "where accepts one to sixteen AND-connected conditions",
        ));
    }
    let mut clauses = Vec::new();
    let mut values = Vec::new();
    let mut recorded = Vec::new();
    for piece in pieces {
        let caps = condition_regex()
            .ok_or_else(|| invalid("where uses an unsupported field or operator"))?
            .captures(piece)
            .ok()
            .flatten()
            .ok_or_else(|| invalid("where uses an unsupported field or operator"))?;
        let field = caps.get(1).map_or("", |m| m.as_str());
        let operator = caps.get(2).map_or("", |m| m.as_str());
        let raw = caps.get(3).map_or("", |m| m.as_str());
        let column = WHERE_FIELDS
            .iter()
            .find(|(name, _)| *name == field)
            .map(|(_, column)| *column)
            .ok_or_else(|| invalid("where uses an unsupported field or operator"))?;
        // Parse as JSON, falling back to an integer for plain decimal text.
        let value: Value = match serde_json::from_str::<Value>(raw) {
            Ok(Value::String(s)) => Value::String(s),
            Ok(Value::Number(n)) if n.is_i64() => Value::from(n.as_i64().unwrap_or(0)),
            Ok(_) => return Err(invalid("where values must be JSON strings or integers")),
            Err(_) if !raw.is_empty() && raw.chars().all(|c| c.is_ascii_digit()) => raw
                .parse::<i64>()
                .map(Value::from)
                .map_err(|_| invalid("where values must be JSON strings or integers"))?,
            Err(_) => return Err(invalid("where values must be JSON strings or integers")),
        };
        let is_int = value.is_i64();
        if matches!(field, "status_code" | "crawl_depth" | "page_ordinal") && !is_int {
            return Err(invalid(&format!("where {field} requires an integer value")));
        }
        if matches!(field, "url" | "content_type" | "representation") && is_int {
            return Err(invalid(&format!(
                "where {field} requires a JSON string value"
            )));
        }
        clauses.push(format!("{column} {operator} ?"));
        values.push(match &value {
            Value::String(s) => rusqlite::types::Value::Text(s.clone()),
            other => rusqlite::types::Value::Integer(other.as_i64().unwrap_or(0)),
        });
        recorded.push(IndexMap::from([
            ("field".to_owned(), Value::from(field)),
            ("operator".to_owned(), Value::from(operator)),
            ("value".to_owned(), value),
        ]));
    }
    Ok((clauses.join(" AND "), values, recorded))
}

/// Full-match pattern for one `field op value` predicate.
fn condition_regex() -> Option<&'static fancy_regex::Regex> {
    static RE: std::sync::OnceLock<Option<fancy_regex::Regex>> = std::sync::OnceLock::new();
    RE.get_or_init(|| fancy_regex::Regex::new(&format!("^{WHERE}")).ok())
        .as_ref()
}

/// Splits a `WHERE` text on case-insensitive `AND`.
fn and_regex() -> Option<&'static fancy_regex::Regex> {
    static RE: std::sync::OnceLock<Option<fancy_regex::Regex>> = std::sync::OnceLock::new();
    RE.get_or_init(|| fancy_regex::Regex::new(r"(?i)\s+AND\s+").ok())
        .as_ref()
}

/// The retry history tables must exist.
pub fn v2_tables(con: &Connection) -> Result<(), StoreError> {
    let mut stmt = con.prepare(
        "SELECT name FROM sqlite_master WHERE type='table' AND name IN ('retry_attempts','retry_transitions')",
    )?;
    let names: std::collections::BTreeSet<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    if names.len() != 2 {
        return Err(StoreError::Invalid(
            "scan.v2 retry history tables are missing".to_owned(),
        ));
    }
    Ok(())
}

fn scan_err(message: impl Into<String>) -> StoreError {
    StoreError::ScanError(ScanError {
        message: message.into(),
    })
}

fn invalid(message: impl Into<String>) -> StoreError {
    StoreError::Invalid(message.into())
}

/// The text of a TEXT cell, or `None` for any other value.
fn sql_text(value: &SqlValue) -> Option<&str> {
    match value {
        SqlValue::Text(t) => Some(t.as_str()),
        _ => None,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// True for a 64-character lowercase hex string.
fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Compact JSON of an object map, with keys in sorted order.
fn dumps_sorted(map: &BTreeMap<String, Value>) -> Result<String, StoreError> {
    Ok(serde_json::to_string(map)?)
}

/// Convert one SQLite cell into its JSON value.
fn sql_json(value: SqlValue) -> Result<Value, StoreError> {
    Ok(match value {
        SqlValue::Null => Value::Null,
        SqlValue::Integer(v) => Value::from(v),
        SqlValue::Real(v) => Value::from(v),
        SqlValue::Text(v) => Value::String(v),
        SqlValue::Blob(_) => {
            return Err(StoreError::Invalid(
                "object of type bytes is not JSON serializable".to_owned(),
            ))
        }
    })
}

/// Rows of a `SELECT` as ordered `(column -> JSON value)` maps, one per row.
fn rows_as_maps(
    con: &Connection,
    sql: &str,
    params: &[SqlValue],
) -> Result<Vec<BTreeMap<String, Value>>, StoreError> {
    let mut stmt = con.prepare(sql)?;
    let names: Vec<String> = stmt
        .column_names()
        .iter()
        .map(|name| name.to_string())
        .collect();
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |row| {
            let mut map = BTreeMap::new();
            for (index, name) in names.iter().enumerate() {
                let value = sql_json(row.get::<_, SqlValue>(index)?)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                map.insert(name.clone(), value);
            }
            Ok(map)
        })?
        .collect::<Result<Vec<_>, rusqlite::Error>>()?;
    Ok(rows)
}

/// Validate the v2 active frontier while preserving v1 backup provenance.
/// Defaults: `require_audit` is false.
pub fn validate_v2(con: &Connection, require_audit: bool) -> Result<(), StoreError> {
    let app: i64 = con.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    if app != APPLICATION_ID {
        return Err(scan_err("foreign application_id"));
    }
    let version: i64 = con.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version != V2_USER_VERSION {
        return Err(scan_err("unsupported scan.v2 user version"));
    }
    v2_tables(con)?;
    let header = con
        .query_row(
            "SELECT format_version,source_kind FROM scan WHERE singleton=1",
            [],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?;
    let valid_header = header.as_ref().is_some_and(|(format, kind)| {
        format == V2_FORMAT && matches!(kind.as_str(), "native" | "reanalysis")
    });
    if !valid_header {
        return Err(scan_err("scan.v2 format header is invalid"));
    }
    let headers: i64 = con.query_row("SELECT COUNT(*) FROM scan", [], |r| r.get(0))?;
    if headers != 1 {
        return Err(scan_err("scan.v2 requires one scan header"));
    }
    if con
        .prepare(
            "SELECT 1 FROM frontier f LEFT JOIN pages p USING(url_id) \
             WHERE (f.state='done') != (p.url_id IS NOT NULL) LIMIT 1",
        )?
        .exists([])?
    {
        return Err(scan_err(
            "scan.v2 active frontier and page population disagree",
        ));
    }
    crate::resource_graph::validate(con)?;
    validate_discovery_ledger(con)?;
    if require_audit
        && !con
            .prepare("SELECT 1 FROM audit WHERE singleton=1")?
            .exists([])?
    {
        return Err(scan_err("scan.v2 has no current audit"));
    }
    for row in rows_as_maps(
        con,
        "SELECT operation,backup_sha256,where_json FROM retry_attempts",
        &[],
    )? {
        let operation = row["operation"].as_str().unwrap_or("");
        let sha = row["backup_sha256"].as_str().unwrap_or("");
        if !matches!(operation, "requeue" | "import_urls") || !is_sha256_hex(sha) {
            return Err(scan_err("scan.v2 retry attempt is invalid"));
        }
        serde_json::from_str::<Value>(row["where_json"].as_str().unwrap_or(""))?;
    }
    for row in rows_as_maps(
        con,
        "SELECT from_frontier_state,to_frontier_state,prior_page_json FROM retry_transitions",
        &[],
    )? {
        let from = row["from_frontier_state"].as_str().unwrap_or("");
        let to = row["to_frontier_state"].as_str().unwrap_or("");
        if !matches!(from, "done" | "external") || to != "queued" {
            return Err(scan_err("scan.v2 retry transition is invalid"));
        }
        if let Some(text) = row["prior_page_json"].as_str() {
            serde_json::from_str::<Value>(text)?;
        }
    }
    Ok(())
}

/// Validate optional v2 discovery evidence without mutating a reader connection.
pub fn validate_discovery_ledger(con: &Connection) -> Result<(), StoreError> {
    let names: std::collections::BTreeSet<String> = con
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    let required = ["discovery_occurrences", "discovery_ledger_coverage"];
    let present: Vec<&&str> = required.iter().filter(|t| names.contains(**t)).collect();
    if present.is_empty() {
        return Ok(());
    }
    if present.len() != required.len() {
        return Err(scan_err("discovery ledger schema is incomplete"));
    }
    const RELATIONS: &[&str] = &[
        "seed",
        "hyperlink",
        "redirect",
        "canonical",
        "alternate",
        "hreflang",
        "x_default",
        "next",
        "prev",
        "refresh",
        "form_action",
        "http_link",
    ];
    const OUTCOMES: &[&str] = &[
        "queued",
        "fetched",
        "excluded",
        "blocked",
        "unresolved",
        "unmeasured",
    ];
    const REPRESENTATIONS: &[&str] = &["static", "rendered", "legacy_fragment", "unmeasured"];
    let mut stmt = con.prepare(
        "SELECT occurrence_key,relation,outcome,representation,raw_value,resolved_value,reason,\
         depth,attributes_json,source_url_id,source_document_id,source_response_id,target_url_id \
         FROM discovery_occurrences",
    )?;
    let occurrences = stmt
        .query_map([], |r| {
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
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (
        occurrence_key,
        relation,
        outcome,
        representation,
        raw_value,
        resolved_value,
        reason,
        depth,
        attributes_json,
        source_url_id,
        source_document_id,
        source_response_id,
        target_url_id,
    ) in occurrences
    {
        let depth_ok = match &depth {
            SqlValue::Null => true,
            SqlValue::Integer(v) => *v >= 0,
            _ => false,
        };
        let attributes_ok = matches!(&attributes_json, SqlValue::Text(t)
            if serde_json::from_str::<Value>(t).is_ok_and(|v| v.is_object()));
        if !matches!(&occurrence_key, SqlValue::Text(t) if !t.is_empty())
            || sql_text(&relation).is_none_or(|v| !RELATIONS.contains(&v))
            || sql_text(&outcome).is_none_or(|v| !OUTCOMES.contains(&v))
            || sql_text(&representation).is_none_or(|v| !REPRESENTATIONS.contains(&v))
            || sql_text(&raw_value).is_none()
            || sql_text(&resolved_value).is_none()
            || sql_text(&reason).is_none()
            || !depth_ok
            || !attributes_ok
        {
            return Err(scan_err("discovery occurrence is invalid"));
        }
        if let SqlValue::Integer(url_id) = source_url_id {
            let found: Option<i64> = con
                .query_row("SELECT 1 FROM urls WHERE url_id=?", [url_id], |r| r.get(0))
                .optional()?;
            if found.is_none() {
                return Err(scan_err("discovery occurrence source URL is missing"));
            }
        }
        if let SqlValue::Integer(document_id) = source_document_id {
            let url_id = match source_url_id {
                SqlValue::Integer(v) => v,
                _ => 0,
            };
            let found: Option<i64> = con
                .query_row(
                    "SELECT 1 FROM documents WHERE document_id=? AND url_id=? AND representation=?",
                    rusqlite::params![document_id, url_id, sql_text(&representation).unwrap_or("")],
                    |r| r.get(0),
                )
                .optional()?;
            if found.is_none() {
                return Err(scan_err("discovery occurrence source document is invalid"));
            }
        }
        if let SqlValue::Integer(response_id) = source_response_id {
            let document_id = match source_document_id {
                SqlValue::Integer(v) => v,
                _ => 0,
            };
            let found: Option<i64> = con
                .query_row(
                    "SELECT 1 FROM documents WHERE document_id=? AND source_response_id=?",
                    rusqlite::params![document_id, response_id],
                    |r| r.get(0),
                )
                .optional()?;
            if found.is_none() {
                return Err(scan_err("discovery occurrence source response is invalid"));
            }
        }
        let target: Option<i64> = con
            .query_row(
                "SELECT url_id FROM urls WHERE url=?",
                [sql_text(&resolved_value).unwrap_or("")],
                |r| r.get(0),
            )
            .optional()?;
        match target_url_id {
            SqlValue::Null => {
                // The recorded identity may legitimately be absent for targets that
                // were unresolved or excluded at capture time (capture-time lookup,
                // not the current catalogue).
                let explained = sql_text(&reason).is_some_and(|r| !r.trim().is_empty());
                let allowed = sql_text(&outcome)
                    .is_some_and(|o| matches!(o, "unresolved" | "unmeasured" | "excluded"));
                if !allowed || !explained {
                    return Err(scan_err(
                        "discovery occurrence has no explained capture-time target identity",
                    ));
                }
            }
            SqlValue::Integer(recorded) if target.is_none() || target != Some(recorded) => {
                return Err(scan_err(
                    "discovery occurrence target identity disagrees with its resolved value",
                ));
            }
            _ => {}
        }
    }
    let mut stmt = con.prepare(
        "SELECT representation,captured,omitted,state,reason,source_document_id \
         FROM discovery_ledger_coverage",
    )?;
    let coverage = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, SqlValue>(0)?,
                r.get::<_, SqlValue>(1)?,
                r.get::<_, SqlValue>(2)?,
                r.get::<_, SqlValue>(3)?,
                r.get::<_, SqlValue>(4)?,
                r.get::<_, SqlValue>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (representation, captured, omitted, state, reason, source_document_id) in coverage {
        let captured_int = matches!(captured, SqlValue::Integer(_));
        let omitted_int = matches!(omitted, SqlValue::Integer(_));
        let captured_v = match captured {
            SqlValue::Integer(v) => v,
            _ => -1,
        };
        let omitted_v = match omitted {
            SqlValue::Integer(v) => v,
            _ => -1,
        };
        let state_v = sql_text(&state).unwrap_or("");
        let reason_v = sql_text(&reason).unwrap_or("");
        let document_exists = matches!(source_document_id, SqlValue::Integer(_)) && {
            let found: Option<i64> = con
                .query_row(
                    "SELECT 1 FROM documents WHERE document_id=? AND representation=?",
                    rusqlite::params![source_document_id, representation],
                    |r| r.get(0),
                )
                .optional()?;
            found.is_some()
        };
        if !matches!(
            sql_text(&representation),
            Some("static" | "rendered" | "legacy_fragment")
        ) || !captured_int
            || !omitted_int
            || captured_v < 0
            || omitted_v < 0
            || captured_v > 2_000
            || !matches!(state_v, "complete" | "partial")
            || sql_text(&reason).is_none()
            || (state_v == "complete" && (omitted_v != 0 || !reason_v.is_empty()))
            || (state_v == "partial" && (omitted_v < 1 || reason_v.is_empty()))
            || !document_exists
        {
            return Err(scan_err("discovery ledger coverage is invalid"));
        }
        let count: i64 = con.query_row(
            "SELECT COUNT(*) FROM discovery_occurrences WHERE source_document_id=? AND representation=?",
            rusqlite::params![source_document_id, representation],
            |r| r.get(0),
        )?;
        if count != captured_v {
            return Err(scan_err(
                "discovery ledger coverage count disagrees with occurrences",
            ));
        }
    }
    Ok(())
}

/// Open the copy with the full artifact validator.
pub fn validate_copy(path: &Path) -> Result<(), StoreError> {
    drop(open_scan(path, false, None)?);
    Ok(())
}

/// Consistent snapshot of `path` at `backup_path`; returns the copy's SHA-256.
///
/// The copy is staged next to the target and published by a hard link: a SQLite backup,
/// `journal_mode=DELETE`, fsync, inspect, then link. There is no portable free-space preflight;
/// a full device still fails the same way through the copy.
pub fn backup(path: &Path, backup_path: &Path) -> Result<String, StoreError> {
    if std::fs::symlink_metadata(backup_path).is_ok() {
        return Err(scan_err("retry requires a new backup_path"));
    }
    let source = std::path::absolute(path)?;
    let con = open_readonly(&source, Duration::from_secs(5), &[])?;
    let snapshot_result = (|| -> Result<(), StoreError> {
        let target = std::path::absolute(backup_path)?;
        let parent = target.parent().unwrap_or_else(|| Path::new("."));
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let temporary = parent.join(format!(
            ".native-snapshot-{}-{nanos}.sqlite",
            std::process::id()
        ));
        std::fs::File::create(&temporary)?;
        let outcome = (|| -> Result<(), StoreError> {
            con.backup(rusqlite::MAIN_DB, &temporary, None)?;
            let copy = Connection::open(&temporary)?;
            copy.execute_batch("PRAGMA journal_mode=DELETE")?;
            copy.close().map_err(|(_, e)| e)?;
            std::fs::File::open(&temporary)?.sync_all()?;
            validate_copy(&temporary)?;
            std::fs::hard_link(&temporary, &target).map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    scan_err(format!(
                        "snapshot target already exists: {}",
                        target.display()
                    ))
                } else {
                    scan_err(format!("cannot snapshot native scan: {e}"))
                }
            })?;
            std::fs::File::open(parent)?.sync_all()?;
            Ok(())
        })();
        let _ = std::fs::remove_file(&temporary);
        outcome
    })();
    drop(con);
    snapshot_result?;
    validate_copy(backup_path)?;
    file_sha256(backup_path).map_err(Into::into)
}

/// Create or adopt the scan.v2 shape on an open writer connection.
pub fn upgrade_to_v2(con: &Connection) -> Result<(), StoreError> {
    let version: i64 = con.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version == V2_USER_VERSION {
        return validate_v2(con, false);
    }
    if version != 1 {
        return Err(scan_err("retry supports scan.v1 or scan.v2 only"));
    }
    let format: Option<String> = con
        .query_row(
            "SELECT format_version FROM scan WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if format.as_deref() != Some(SCAN_V1) {
        return Err(scan_err("explicit retry upgrade requires a scan.v1 header"));
    }
    con.execute_batch(&format!(
        "ALTER TABLE scan RENAME TO scan_v1_upgrade;
         CREATE TABLE scan (singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
         scan_uuid TEXT NOT NULL UNIQUE,
         format_version TEXT NOT NULL CHECK (format_version = '{SCAN_V2}'),
         evidence_version TEXT NOT NULL,writer_version TEXT NOT NULL,writer_revision TEXT NOT NULL,
         runtime_versions_json TEXT NOT NULL,created_at TEXT NOT NULL,finished_at TEXT,
         source_kind TEXT NOT NULL CHECK (source_kind IN ('native','legacy_import','reanalysis')),
         parent_scan_uuid TEXT,start_url TEXT,config_json TEXT NOT NULL,config_fingerprint TEXT NOT NULL,
         lifecycle TEXT NOT NULL CHECK (lifecycle IN ('running','interrupted','finished','failed')),
         finish_reason TEXT NOT NULL,crawl_partial INTEGER NOT NULL CHECK (crawl_partial IN (0,1)),
         corpus_partial INTEGER NOT NULL CHECK (corpus_partial IN (0,1)),
         evidence_revision INTEGER NOT NULL CHECK (evidence_revision >= 0),limitations_json TEXT NOT NULL,
         capabilities_json TEXT NOT NULL,retention_json TEXT NOT NULL,pinned INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0,1)));
         INSERT INTO scan SELECT singleton,scan_uuid,'{SCAN_V2}',evidence_version,writer_version,
         writer_revision,runtime_versions_json,created_at,finished_at,source_kind,parent_scan_uuid,
         start_url,config_json,config_fingerprint,lifecycle,finish_reason,crawl_partial,corpus_partial,
         evidence_revision,limitations_json,capabilities_json,retention_json,pinned FROM scan_v1_upgrade;
         DROP TABLE scan_v1_upgrade;
         CREATE TABLE retry_attempts (attempt_id INTEGER PRIMARY KEY,attempt_uuid TEXT NOT NULL UNIQUE,
         operation TEXT NOT NULL CHECK (operation IN ('requeue','import_urls')),requested_at TEXT NOT NULL,
         where_json TEXT NOT NULL,backup_path TEXT NOT NULL,backup_sha256 TEXT NOT NULL
         CHECK (length(backup_sha256) = 64),source_scan_uuid TEXT);
         CREATE TABLE retry_transitions (attempt_id INTEGER NOT NULL REFERENCES retry_attempts(attempt_id),
         url_id INTEGER NOT NULL,url TEXT NOT NULL,from_frontier_state TEXT NOT NULL,
         to_frontier_state TEXT NOT NULL,prior_page_json TEXT,prior_document_id INTEGER,
         prior_evidence_sha256 TEXT,removed_links INTEGER NOT NULL CHECK (removed_links >= 0),
         removed_forms INTEGER NOT NULL CHECK (removed_forms >= 0),removed_resource_refs INTEGER NOT NULL
         CHECK (removed_resource_refs >= 0),removed_contexts INTEGER NOT NULL CHECK (removed_contexts >= 0),
         PRIMARY KEY (attempt_id,url_id));
         CREATE INDEX retry_transitions_url_id ON retry_transitions(url_id,attempt_id);
         PRAGMA user_version={V2_USER_VERSION};"
    ))?;
    crate::discovery_ledger::ensure_schema(con)?;
    crate::external_checks::ensure_schema(con)
}

/// Open the scan for writing (`mode=rw`, timeout 5s) with the writer pragmas.
pub fn writer(path: &Path) -> Result<Connection, StoreError> {
    let con = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    con.busy_timeout(Duration::from_secs(5))?;
    con.execute_batch("PRAGMA foreign_keys=ON;PRAGMA trusted_schema=OFF;PRAGMA synchronous=FULL")?;
    Ok(con)
}

/// Record one retry attempt; returns its `attempt_id`.
pub fn attempt(
    con: &Connection,
    operation: &str,
    selection: &[BTreeMap<String, Value>],
    backup_path: &Path,
    backup_sha256: &str,
    source_scan_uuid: Option<&str>,
) -> Result<i64, StoreError> {
    // Keys serialize in sorted order (`BTreeMap`).
    let where_json = serde_json::to_string(selection)?;
    con.execute(
        "INSERT INTO retry_attempts(attempt_uuid,operation,requested_at,where_json,backup_path,backup_sha256,source_scan_uuid) \
         VALUES(?,?,?,?,?,?,?)",
        rusqlite::params![
            uuid::Uuid::new_v4().to_string(),
            operation,
            crate::scan_lease::utc()?,
            where_json,
            backup_path.display().to_string(),
            backup_sha256,
            source_scan_uuid,
        ],
    )?;
    Ok(con.last_insert_rowid())
}

/// `(sha256, counts)` of the evidence rows a requeue removes for one page.
pub fn evidence_digest(
    con: &Connection,
    url_id: i64,
    queue_ordinal: i64,
) -> Result<(String, BTreeMap<String, i64>), StoreError> {
    let mut rows = BTreeMap::new();
    rows.insert(
        "links".to_owned(),
        con.query_row(
            "SELECT COUNT(*) FROM links WHERE source_url_id=?",
            [url_id],
            |r| r.get::<_, i64>(0),
        )?,
    );
    rows.insert(
        "forms".to_owned(),
        con.query_row(
            "SELECT COUNT(*) FROM forms WHERE page_url_id=?",
            [url_id],
            |r| r.get::<_, i64>(0),
        )?,
    );
    rows.insert(
        "resource_refs".to_owned(),
        con.query_row(
            "SELECT COUNT(*) FROM resource_refs WHERE page_url_id=?",
            [url_id],
            |r| r.get::<_, i64>(0),
        )?,
    );
    rows.insert(
        "contexts".to_owned(),
        con.query_row(
            "SELECT COUNT(*) FROM context_items WHERE item_key LIKE ? OR (kind='native_commit' AND item_key=?)",
            rusqlite::params![format!("page:{url_id}:%"), queue_ordinal.to_string()],
            |r| r.get::<_, i64>(0),
        )?,
    );
    let encoded = dumps_sorted(
        &rows
            .iter()
            .map(|(k, v)| (k.clone(), Value::from(*v)))
            .collect(),
    )?;
    Ok((sha256_hex(encoded.as_bytes()), rows))
}

/// Upgrade explicitly and requeue selected done pages with durable transitions.
pub fn requeue_scan(
    input_path: &Path,
    where_: &str,
    backup_path: &Path,
    from_scan: Option<&Path>,
) -> Result<BTreeMap<String, Value>, StoreError> {
    let (predicate_sql, values, selection_indexed) = predicate(where_)?;
    let selection: Vec<BTreeMap<String, Value>> = selection_indexed
        .iter()
        .map(|entry| entry.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .collect();
    let path = std::path::absolute(input_path)?;
    let backup_target = std::path::absolute(backup_path)?;
    crate::history::regular(&path)?;
    let mut source_urls: Option<Vec<String>> = None;
    let mut source_scan_uuid: Option<String> = None;
    if let Some(from) = from_scan {
        let source = std::path::absolute(from)?;
        if source == path {
            return Err(invalid(
                "from_scan must differ from the scan being requeued",
            ));
        }
        let source_con = open_scan(&source, false, None)?;
        source_scan_uuid = Some(con_row_string(
            &source_con,
            "SELECT scan_uuid FROM scan WHERE singleton=1",
        )?);
        let urls = {
            let mut stmt = source_con.prepare(&format!(
                "SELECT u.url FROM pages p JOIN urls u USING(url_id) WHERE {predicate_sql} ORDER BY p.page_ordinal"
            ))?;
            let found = stmt
                .query_map(rusqlite::params_from_iter(values.iter()), |r| {
                    r.get::<_, String>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?;
            drop(stmt);
            found
        };
        drop(source_con);
        if urls.is_empty() {
            return Err(scan_err("where matched no pages in from_scan"));
        }
        if urls.len() > 20_000 {
            return Err(scan_err(
                "from_scan selection exceeds the 20,000 URL retry bound",
            ));
        }
        source_urls = Some(urls);
    }
    let _lock = crate::history::hold_writer_lock(&path)?;
    let out = (|| -> Result<BTreeMap<String, Value>, StoreError> {
        let backup_sha256 = backup(&path, &backup_target)?;
        let con = writer(&path)?;
        let inner = (|| -> Result<BTreeMap<String, Value>, StoreError> {
            con.execute_batch("BEGIN IMMEDIATE")?;
            upgrade_to_v2(&con)?;
            let mut sql =
                "SELECT p.*,f.state AS frontier_state,f.queue_ordinal,u.url FROM pages p \
                 JOIN frontier f USING(url_id) JOIN urls u USING(url_id) WHERE f.state='done' AND "
                    .to_owned();
            let mut bound: Vec<SqlValue> = values;
            match &source_urls {
                None => sql.push_str(&predicate_sql),
                Some(urls) => {
                    sql.push_str(&format!(
                        "u.url IN ({})",
                        urls.iter().map(|_| "?").collect::<Vec<_>>().join(",")
                    ));
                    bound.extend(urls.iter().map(|u| SqlValue::Text(u.clone())));
                }
            }
            sql.push_str(" ORDER BY p.page_ordinal");
            let rows = rows_as_maps(&con, &sql, &bound)?;
            if rows.is_empty() {
                return Err(scan_err("where matched no completed pages to requeue"));
            }
            let attempt_id = attempt(
                &con,
                "requeue",
                &selection,
                &backup_target,
                &backup_sha256,
                source_scan_uuid.as_deref(),
            )?;
            crate::url_query_projection::invalidate(&con)?;
            crate::resource_graph::invalidate_pages(
                &con,
                &rows
                    .iter()
                    .map(|row| row["url_id"].as_i64().unwrap_or(0))
                    .collect::<Vec<_>>(),
            )?;
            for row in &rows {
                let url_id = row["url_id"].as_i64().unwrap_or(0);
                let queue_ordinal = row["queue_ordinal"].as_i64().unwrap_or(0);
                let (digest, counts) = evidence_digest(&con, url_id, queue_ordinal)?;
                con.execute(
                    "INSERT INTO retry_transitions VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
                    rusqlite::params![
                        attempt_id,
                        url_id,
                        row["url"].as_str().unwrap_or(""),
                        "done",
                        "queued",
                        dumps_sorted(row)?,
                        row["document_id"].as_i64(),
                        digest,
                        counts["links"],
                        counts["forms"],
                        counts["resource_refs"],
                        counts["contexts"],
                    ],
                )?;
                con.execute("DELETE FROM links WHERE source_url_id=?", [url_id])?;
                con.execute("DELETE FROM forms WHERE page_url_id=?", [url_id])?;
                con.execute("DELETE FROM resource_refs WHERE page_url_id=?", [url_id])?;
                con.execute(
                    "DELETE FROM context_items WHERE item_key LIKE ? OR (kind='native_commit' AND item_key=?)",
                    rusqlite::params![format!("page:{url_id}:%"), queue_ordinal.to_string()],
                )?;
                con.execute("DELETE FROM pages WHERE url_id=?", [url_id])?;
                con.execute(
                    "UPDATE frontier SET state='queued' WHERE url_id=? AND state='done'",
                    [url_id],
                )?;
            }
            let metadata = con.query_row(
                "SELECT capabilities_json,retention_json,source_kind FROM scan WHERE singleton=1",
                [],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )?;
            let mut capabilities: Map<String, Value> = serde_json::from_str(&metadata.0)?;
            let retention: Value = serde_json::from_str(&metadata.1)?;
            let summary = crate::corpus::corpus_summary(&con, &retention)?;
            if let Some(states) = summary.get("capabilities").and_then(Value::as_object) {
                for (k, v) in states {
                    capabilities.insert(k.clone(), v.clone());
                }
            }
            if metadata.2 == "native" {
                capabilities.insert(
                    "offline_reanalysis".to_owned(),
                    serde_json::to_value(crate::native_scan::reanalysis_capability(&con)?)?,
                );
            }
            let evidence_revision: i64 = con.query_row(
                "SELECT (SELECT COUNT(*) FROM pages) + \
                 (SELECT COUNT(*) FROM context_items WHERE kind='resource_commit') + \
                 (SELECT COUNT(*) FROM documents WHERE representation IN ('rendered','legacy_fragment'))",
                [],
                |r| r.get(0),
            )?;
            let sorted: BTreeMap<String, Value> = capabilities.into_iter().collect();
            con.execute(
                "UPDATE scan SET lifecycle='running',finished_at=NULL,finish_reason='retry_requeue',\
                 crawl_partial=1,corpus_partial=?,capabilities_json=?,evidence_revision=? WHERE singleton=1",
                rusqlite::params![
                    summary.get("corpus_partial").and_then(Value::as_bool).unwrap_or(false) as i64,
                    dumps_sorted(&sorted)?,
                    evidence_revision,
                ],
            )?;
            con.execute_batch("DELETE FROM audit")?;
            con.execute_batch("COMMIT")?;
            let mut out = BTreeMap::new();
            out.insert("scan".to_owned(), Value::String(path.display().to_string()));
            out.insert(
                "backup".to_owned(),
                Value::String(backup_target.display().to_string()),
            );
            out.insert("attempt_id".to_owned(), Value::from(attempt_id));
            out.insert("requeued".to_owned(), Value::from(rows.len() as i64));
            Ok(out)
        })();
        if inner.is_err() {
            let _ = con.execute_batch("ROLLBACK");
        }
        drop(con);
        inner
    })();
    out
}

/// `SELECT` a single TEXT cell; NULL or a missing row gives an empty string.
fn con_row_string(con: &Connection, sql: &str) -> Result<String, StoreError> {
    Ok(con
        .query_row(sql, [], |r| r.get::<_, Option<String>>(0))
        .optional()?
        .flatten()
        .unwrap_or_default())
}

/// Import external URL-list input through the target scan's existing admission rules.
pub fn scan_import_urls(
    input_path: &Path,
    urls_file: &Path,
    backup_path: &Path,
) -> Result<BTreeMap<String, Value>, StoreError> {
    let target = std::path::absolute(input_path)?;
    let backup_target = std::path::absolute(backup_path)?;
    let listed_urls = crate::crawl_list_input::read_url_list(urls_file)?;
    if listed_urls.len() > 20_000 {
        return Err(scan_err(
            "external URL list exceeds the 20,000 URL import bound",
        ));
    }
    crate::history::regular(&target)?;
    let _lock = crate::history::hold_writer_lock(&target)?;
    let out = (|| -> Result<BTreeMap<String, Value>, StoreError> {
        let backup_sha256 = backup(&target, &backup_target)?;
        let con = writer(&target)?;
        let inner = (|| -> Result<BTreeMap<String, Value>, StoreError> {
            con.execute_batch("BEGIN IMMEDIATE")?;
            upgrade_to_v2(&con)?;
            let (start_url, config_json): (Option<String>, String) = con.query_row(
                "SELECT start_url,config_json FROM scan WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let config: Value = serde_json::from_str(&config_json)?;
            let scope_map: BTreeMap<String, Value> = config
                .get("scope")
                .and_then(Value::as_object)
                .ok_or_else(|| invalid("scan config has no scope"))?
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            let scope = crate::crawl_records::spider::Scope::from_config(Some(&scope_map));
            let start_host = crate::ledger::urlparts::urlsplit(&start_url.unwrap_or_default())
                .hostname()
                .unwrap_or_default()
                .to_lowercase();
            if start_host.is_empty() {
                return Err(scan_err(
                    "scan has no start host for external URL admission",
                ));
            }
            let urls_file_abs = std::path::absolute(urls_file)?;
            let selection: Vec<BTreeMap<String, Value>> = vec![BTreeMap::from([(
                "urls_file".to_owned(),
                Value::String(urls_file_abs.display().to_string()),
            )])];
            let attempt_id = attempt(
                &con,
                "import_urls",
                &selection,
                &backup_target,
                &backup_sha256,
                None,
            )?;
            let mut next_ordinal: i64 = con.query_row(
                "SELECT COALESCE(MAX(queue_ordinal)+1,0) FROM frontier",
                [],
                |r| r.get(0),
            )?;
            let max_url_length = config
                .pointer("/limits/max_url_length")
                .and_then(Value::as_i64);
            let query_limit = config
                .pointer("/limits/max_query_variants_per_path")
                .and_then(Value::as_i64);
            let (mut added, mut rejected) = (0i64, 0i64);
            for raw_url in &listed_urls {
                let url = crate::crawl_records::spider::strip_fragment(raw_url);
                let reason = scope.rejection(&url, &start_host);
                if !reason.is_empty()
                    || max_url_length
                        .is_some_and(|limit| limit != 0 && url.chars().count() as i64 > limit)
                {
                    rejected += 1;
                    continue;
                }
                let existing: Option<i64> = con
                    .query_row("SELECT url_id FROM urls WHERE url=?", [&url], |r| r.get(0))
                    .optional()?;
                if let Some(url_id) = existing {
                    let queued: Option<i64> = con
                        .query_row("SELECT 1 FROM frontier WHERE url_id=?", [url_id], |r| {
                            r.get(0)
                        })
                        .optional()?;
                    if queued.is_some() {
                        continue;
                    }
                }
                let parts = crate::ledger::urlparts::urlsplit(&url);
                let path_key = if parts.path.is_empty() {
                    "/".to_owned()
                } else {
                    parts.path.clone()
                };
                if !parts.query.is_empty() {
                    let count: i64 = con.query_row(
                        "SELECT COUNT(*) FROM query_variants WHERE path_key=?",
                        [&path_key],
                        |r| r.get(0),
                    )?;
                    let known: Option<i64> = con
                        .query_row(
                            "SELECT 1 FROM query_variants WHERE path_key=? AND query_key=?",
                            rusqlite::params![path_key, parts.query],
                            |r| r.get(0),
                        )
                        .optional()?;
                    if known.is_none()
                        && query_limit.is_some_and(|limit| limit != 0 && count >= limit)
                    {
                        rejected += 1;
                        continue;
                    }
                    con.execute(
                        "INSERT OR IGNORE INTO query_variants(path_key,query_key) VALUES(?,?)",
                        rusqlite::params![path_key, parts.query],
                    )?;
                }
                con.execute("INSERT OR IGNORE INTO urls(url) VALUES(?)", [&url])?;
                let url_id: i64 =
                    con.query_row("SELECT url_id FROM urls WHERE url=?", [&url], |r| r.get(0))?;
                con.execute(
                    "INSERT INTO frontier(url_id,queue_ordinal,depth,state) VALUES(?,?,?,'queued')",
                    rusqlite::params![url_id, next_ordinal, 0],
                )?;
                con.execute(
                    "INSERT INTO retry_transitions VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
                    rusqlite::params![
                        attempt_id,
                        url_id,
                        url,
                        "external",
                        "queued",
                        SqlValue::Null,
                        SqlValue::Null,
                        SqlValue::Null,
                        0,
                        0,
                        0,
                        0,
                    ],
                )?;
                next_ordinal += 1;
                added += 1;
            }
            con.execute_batch(
                "UPDATE scan SET lifecycle='running',finished_at=NULL,finish_reason='retry_import',crawl_partial=1 WHERE singleton=1",
            )?;
            con.execute_batch("DELETE FROM audit")?;
            con.execute_batch("COMMIT")?;
            let mut out = BTreeMap::new();
            out.insert(
                "scan".to_owned(),
                Value::String(target.display().to_string()),
            );
            out.insert(
                "backup".to_owned(),
                Value::String(backup_target.display().to_string()),
            );
            out.insert("attempt_id".to_owned(), Value::from(attempt_id));
            out.insert("imported".to_owned(), Value::from(added));
            out.insert("rejected".to_owned(), Value::from(rejected));
            Ok(out)
        })();
        if inner.is_err() {
            let _ = con.execute_batch("ROLLBACK");
        }
        drop(con);
        inner
    })();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "spiderhead-retry-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A valid scan.v1 artifact built by the crate's own writer, then seeded
    /// with one extra completed page (`/a`) alongside the start URL.
    fn scan_v1(dir: &Path) -> PathBuf {
        use crate::scan_schema::{crawl_fingerprint, NativeScanSchemaMixin};
        use serde_json::json;
        use spiderhead_core::settings_data;

        let path = dir.join("scan.sqlite");
        let mut config = settings_data::defaults().clone();
        config["speed"]["min_delay_seconds"] = json!(0);
        let config: Map<String, Value> = config.as_object().cloned().unwrap();
        let fingerprint = crawl_fingerprint(&config).unwrap();
        let versions: Map<String, Value> = ["python", "sqlite", "httpx", "lxml", "beautifulsoup4"]
            .iter()
            .map(|k| ((*k).to_owned(), json!("test")))
            .collect();
        let mut scan = NativeScanSchemaMixin::create(
            &path,
            "https://example.test/",
            &config,
            Some(&fingerprint),
            "3.0.0",
            &"a".repeat(40),
            &versions,
            &[],
            None,
            &[],
            "scan.v1",
        )
        .unwrap();
        let con = scan.con();
        con.execute_batch(
            "UPDATE scan SET lifecycle='finished' WHERE singleton=1;
             INSERT INTO urls(url) VALUES('https://example.test/'),('https://example.test/a');",
        )
        .unwrap();
        let start_id: i64 = con
            .query_row(
                "SELECT url_id FROM urls WHERE url='https://example.test/'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let a_id: i64 = con
            .query_row(
                "SELECT url_id FROM urls WHERE url='https://example.test/a'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        con.execute_batch(&format!(
            "INSERT INTO frontier VALUES({start_id},0,0,'done'),({a_id},1,0,'done');
             INSERT INTO pages(url_id,page_ordinal,status_code,content_type,size_bytes,redirect_url,title,\
                 meta_description,h1,h1_2,h2,canonical,meta_robots,x_robots,og_title,og_description,\
                 og_image,word_count,crawl_depth,content_encoding,charset,doctype,viewport,\
                 transport_upgraded,head_count,body_count,head_not_first,invalid_head_elements,\
                 outlinks,external_outlinks,jsonld_blocks_found,jsonld_blocks_parsed,error,error_kind,\
                 cache_status,representation,redirect_chain_json,final_url,content_frames,\
                 content_frames_same_origin,hreflang_json,heading_outline_json,body_unavailable) \
                 VALUES({start_id},0,200,'text/html',100,'','','','','','','','','','','','',10,0,'','utf-8','','',\
                 0,1,1,0,'',0,0,0,0,'','','','static','[]','https://example.test/',0,0,'[]','[]',''),\
                 ({a_id},1,200,'text/html',100,'','','','','','','','','','','','',10,0,'','utf-8','','',\
                 0,1,1,0,'',0,0,0,0,'','','','static','[]','https://example.test/a',0,0,'[]','[]','');
             UPDATE scan SET evidence_revision=(SELECT COUNT(*) FROM pages);
             INSERT INTO context_items VALUES
             ('native_commit','0','scan_context.v1',\
             '{{\"digest\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"}}',\
             'complete','atomic page commit'),
             ('native_commit','1','scan_context.v1',\
             '{{\"digest\":\"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\"}}',\
             'complete','atomic page commit');"
        ))
        .unwrap();
        scan.close();
        drop(scan);
        path
    }

    #[test]
    fn upgrade_makes_v2_and_validate_v2_passes() {
        let dir = temp_dir("v2");
        let path = scan_v1(&dir);
        let con = writer(&path).unwrap();
        upgrade_to_v2(&con).unwrap();
        validate_v2(&con, false).unwrap();
        // Second call is idempotent: an already-v2 artifact revalidates.
        upgrade_to_v2(&con).unwrap();
        con.close().unwrap();
    }

    #[test]
    fn requeue_moves_done_pages_back_to_queued() {
        let dir = temp_dir("rq");
        let path = scan_v1(&dir);
        let backup_path = dir.join("backup.sqlite");
        let out = requeue_scan(&path, "status_code = 200", &backup_path, None).unwrap();
        assert_eq!(out["requeued"], json_i64(2));
        assert!(backup_path.exists());
        let con = open_scan(&path, false, None).unwrap();
        let queued: i64 = con
            .query_row(
                "SELECT COUNT(*) FROM frontier WHERE state='queued'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(queued, 2);
        assert!(
            con.query_row("SELECT COUNT(*) FROM pages", [], |r| r.get::<_, i64>(0))
                .unwrap()
                == 0
        );
        let transitions: i64 = con
            .query_row("SELECT COUNT(*) FROM retry_transitions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(transitions, 2);
        validate_v2(&con, false).unwrap();
    }

    #[test]
    fn requeue_rejects_an_empty_selection_and_existing_backup() {
        let dir = temp_dir("rq-empty");
        let path = scan_v1(&dir);
        let backup_path = dir.join("backup.sqlite");
        let err = requeue_scan(&path, "status_code = 404", &backup_path, None).unwrap_err();
        assert!(
            err.to_string().contains("where matched no completed pages"),
            "{err}"
        );
        // The artifact was not modified (transaction rolled back): still v1, page present.
        let con = open_scan(&path, false, None).unwrap();
        let pages: i64 = con
            .query_row("SELECT COUNT(*) FROM pages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(pages, 2);
    }

    #[test]
    fn import_urls_admits_in_scope_and_rejects_external() {
        let dir = temp_dir("imp");
        let path = scan_v1(&dir);
        let urls_file = dir.join("urls.txt");
        std::fs::write(
            &urls_file,
            "https://example.test/c\nhttps://example.test/a\nhttps://other.example/x\n",
        )
        .unwrap();
        let backup_path = dir.join("backup.sqlite");
        let out = scan_import_urls(&path, &urls_file, &backup_path).unwrap();
        // '/c' is new; '/a' is already in frontier; other host is rejected.
        assert_eq!(out["imported"], json_i64(1));
        assert_eq!(out["rejected"], json_i64(1));
        let con = open_scan(&path, false, None).unwrap();
        let queued: i64 = con
            .query_row(
                "SELECT COUNT(*) FROM frontier f JOIN urls u USING(url_id) WHERE f.state='queued' AND u.url='https://example.test/c'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(queued, 1);
        let external: i64 = con
            .query_row(
                "SELECT COUNT(*) FROM retry_transitions WHERE from_frontier_state='external'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(external, 1);
    }

    #[test]
    fn evidence_digest_counts_removed_rows() {
        let dir = temp_dir("digest");
        let path = scan_v1(&dir);
        let con = Connection::open(&path).unwrap();
        con.execute(
            "INSERT INTO context_items VALUES('native_commit','3','v','{}','complete','')",
            [],
        )
        .unwrap();
        let (digest, counts) = evidence_digest(&con, 1, 3).unwrap();
        assert_eq!(digest.len(), 64);
        assert_eq!(counts["links"], 0);
        assert_eq!(counts["contexts"], 1);
    }

    fn json_i64(v: i64) -> Value {
        Value::from(v)
    }
}

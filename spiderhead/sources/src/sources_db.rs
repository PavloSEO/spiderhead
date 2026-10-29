//! One versioned SQLite store that accumulates provider data per project.
//!
//! One table per source keyed by `(resource, date, dimensions…)`; a synced day is replaced
//! atomically (delete + insert in one transaction); a `sync_log` ledger records every fetched
//! `(source, resource, date)` including empty days; each source's lag keeps recent days from
//! being marked synced. Only resource identifiers are stored — tokens never reach the file.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::Value;
use thiserror::Error;

use crate::{pyjson, pystr, pytime};

mod catalog;
pub use catalog::*;
mod periods;
pub use periods::*;

/// `DB_NAME` — the store file inside a project directory.
pub const DB_NAME: &str = "sources.sqlite";
/// `SCHEMA_VERSION` — `sources_meta.schema_version` (a string).
pub const SCHEMA_VERSION: &str = "1";
/// `SCHEMA_KIND` — `sources_meta.kind`.
pub const SCHEMA_KIND: &str = "spiderhead.sources";
/// `RETRIES` — extra attempts after a retryable chunk failure.
pub const RETRIES: usize = 2;
/// `RETRY_PAUSE` — linear backoff step between chunk retries.
pub const RETRY_PAUSE: f64 = 2.0;
/// `MAX_DAYS` — one sync's span cap.
pub const MAX_DAYS: i64 = 800;
/// `MAX_EXPORT_ROWS` — `query` page cap.
pub const MAX_EXPORT_ROWS: i64 = 100_000;

/// Errors this module raises: invalid input, or a driver or filesystem failure.
#[derive(Debug, Error)]
pub enum SourcesError {
    /// Invalid input.
    #[error("{0}")]
    Invalid(String),
    /// A value of the wrong type, e.g. an unhashable `date` used as a key.
    #[error("{0}")]
    Type(String),
    /// A provider row that is not an object.
    #[error("{0}")]
    Attribute(String),
    /// A ledger row names a source that is not registered.
    #[error("{0}")]
    Key(String),
    /// SQLite driver failure, carried as its message.
    #[error("{0}")]
    Sqlite(#[from] rusqlite::Error),
    /// Filesystem failure.
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

impl SourcesError {
    /// Stable error class name reported to callers.
    pub fn python_class(&self) -> &'static str {
        match self {
            SourcesError::Invalid(_) => "ValueError",
            SourcesError::Type(_) => "TypeError",
            SourcesError::Attribute(_) => "AttributeError",
            SourcesError::Key(_) => "KeyError",
            SourcesError::Sqlite(err) => match err {
                // The primary result code selects the error subclass.
                rusqlite::Error::SqliteFailure(inner, _) => match inner.code {
                    rusqlite::ErrorCode::ConstraintViolation => "IntegrityError",
                    rusqlite::ErrorCode::DatabaseCorrupt => "DatabaseError",
                    _ => "OperationalError",
                },
                _ => "sqlite3.Error",
            },
            SourcesError::Io(err) => match err.kind() {
                std::io::ErrorKind::NotFound => "FileNotFoundError",
                std::io::ErrorKind::AlreadyExists => "FileExistsError",
                std::io::ErrorKind::PermissionDenied => "PermissionError",
                _ => "OSError",
            },
        }
    }
}

fn invalid<T>(message: impl Into<String>) -> Result<T, SourcesError> {
    Err(SourcesError::Invalid(message.into()))
}

/// `db_path` — an explicit path or `<project>/sources.sqlite`; exactly one truthy argument.
pub fn db_path(db: Option<&str>, project: Option<&str>) -> Result<PathBuf, SourcesError> {
    let db_given = db.is_some_and(|d| !d.is_empty());
    let project_given = project.is_some_and(|p| !p.is_empty());
    if db_given == project_given {
        return invalid("pass exactly one of db or project");
    }
    if let Some(project) = project.filter(|p| !p.is_empty()) {
        let root = Path::new(project);
        if !root.join("project.json").is_file() {
            return invalid("project must be a seohead project directory with project.json");
        }
        return Ok(root.join(DB_NAME));
    }
    Ok(PathBuf::from(db.unwrap_or_default()))
}

/// `connect` — open the store; `create` bootstraps the identity and schema on a new file.
pub fn connect(path: &Path, create: bool) -> Result<Connection, SourcesError> {
    if !create && !path.is_file() {
        return invalid(format!(
            "no sources database at {}; run sources-sync first",
            py_path_str(path)
        ));
    }
    if create && !path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let _ = fs::OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .mode(0o600)
                .open(path);
        }
        #[cfg(not(unix))]
        let _ = fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(path);
    }
    let connection = if create {
        spiderhead_core::sqlite::open_writer(path, spiderhead_core::sqlite::DEFAULT_TIMEOUT, &[])?
    } else {
        let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        spiderhead_core::sqlite::open_readonly(
            &resolved,
            spiderhead_core::sqlite::DEFAULT_TIMEOUT,
            &[],
        )?
    };
    if create {
        connection.execute_batch("BEGIN IMMEDIATE")?;
    }
    let marker: Option<String> = match connection.query_row(
        "SELECT name FROM sqlite_master WHERE type='table' AND name='sources_meta'",
        [],
        |row| row.get(0),
    ) {
        Ok(name) => Some(name),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(err) => return Err(err.into()),
    };
    if marker.is_some() {
        let identity: std::collections::HashMap<String, String> = connection
            .prepare("SELECT key, value FROM sources_meta")?
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<_, _>>()?;
        let kind = identity.get("kind").map(String::as_str);
        let version = identity.get("schema_version").map(String::as_str);
        if kind != Some(SCHEMA_KIND) || version != Some(SCHEMA_VERSION) {
            if create {
                let _ = connection.execute_batch("ROLLBACK");
            }
            return invalid("unsupported sources database identity or schema version");
        }
        if !create {
            connection.execute_batch("PRAGMA query_only=ON")?;
        } else {
            connection.execute_batch("COMMIT")?;
        }
        return Ok(connection);
    }
    if !create || path.metadata().map(|m| m.len() > 0).unwrap_or(false) {
        if create {
            let _ = connection.execute_batch("ROLLBACK");
        }
        return invalid("existing database is not a versioned sources database; back it up first");
    }
    connection
        .execute_batch("CREATE TABLE sources_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL)")?;
    connection.execute(
        "INSERT INTO sources_meta(key,value) VALUES(?,?)",
        rusqlite::params!["kind", SCHEMA_KIND],
    )?;
    connection.execute(
        "INSERT INTO sources_meta(key,value) VALUES(?,?)",
        rusqlite::params!["schema_version", SCHEMA_VERSION],
    )?;
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS sync_log (source TEXT NOT NULL, resource TEXT NOT NULL, \
         date TEXT NOT NULL, rows INTEGER NOT NULL DEFAULT 0, complete INTEGER NOT NULL DEFAULT 0, \
         fetched_at TEXT, state TEXT NOT NULL, last_attempt_state TEXT, \
         last_attempt_at TEXT, PRIMARY KEY (source, resource, date))",
    )?;
    for (_, spec) in SOURCES {
        let dims: String = spec
            .dimensions
            .iter()
            .map(|name| format!(", {name} TEXT NOT NULL DEFAULT ''"))
            .collect();
        let metrics: String = spec
            .metrics
            .iter()
            .map(|name| format!(", {name} REAL"))
            .collect();
        let mut keys = vec!["resource", "date"];
        keys.extend(spec.dimensions.iter());
        connection.execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS {} (resource TEXT NOT NULL, \
             date TEXT NOT NULL{dims}{metrics}, PRIMARY KEY ({}))",
            spec.table,
            keys.join(", ")
        ))?;
    }
    connection.execute_batch("COMMIT")?;
    Ok(connection)
}

/// Replace each day atomically; malformed provider rows are an invalid-input error.
fn store(
    connection: &Connection,
    source: &str,
    resource: &str,
    days: &[String],
    rows: &[Value],
    complete: bool,
    fetched_at: &str,
) -> Result<i64, SourcesError> {
    let spec = source_spec(source)?;
    let mut columns = vec!["resource", "date"];
    columns.extend(spec.dimensions.iter());
    columns.extend(spec.metrics.iter());
    let mut day_rows: BTreeMap<&str, Vec<Vec<rusqlite::types::Value>>> =
        days.iter().map(|d| (d.as_str(), Vec::new())).collect();
    let mut keys_seen: BTreeMap<&str, std::collections::BTreeSet<Vec<String>>> = days
        .iter()
        .map(|d| (d.as_str(), Default::default()))
        .collect();
    for row in rows {
        // A non-object row or an unhashable `date` is an error that propagates through `sync` unchanged.
        let Some(row) = row.as_object() else {
            return Err(SourcesError::Attribute(format!(
                "'{}' object has no attribute 'get'",
                pystr::type_name(row)
            )));
        };
        let date_value = row.get("date");
        if matches!(date_value, Some(Value::Array(_)) | Some(Value::Object(_))) {
            let name = pystr::type_name(date_value.unwrap_or(&Value::Null));
            return Err(SourcesError::Type(format!(
                "cannot use '{name}' as a dict key (unhashable type: '{name}')"
            )));
        }
        let day = date_value.and_then(Value::as_str).unwrap_or_default();
        if !day_rows.contains_key(day) {
            return invalid("provider row date is outside the requested period");
        }
        let mut key_parts = Vec::new();
        let mut missing = false;
        for name in spec.dimensions {
            match row.get(*name) {
                Some(v) if !v.is_null() => {
                    key_parts.push(pystr::repr_value(v));
                }
                _ => missing = true,
            }
        }
        if missing {
            return invalid("provider row is missing a dimension value");
        }
        if !keys_seen.entry(day).or_default().insert(key_parts.clone()) {
            return invalid("provider returned duplicate normalized dimension keys");
        }
        let mut values = vec![
            rusqlite::types::Value::Text(resource.to_owned()),
            rusqlite::types::Value::Text(day.to_owned()),
        ];
        values.extend(key_parts.into_iter().map(rusqlite::types::Value::Text));
        for name in spec.metrics {
            values.push(match number(row.get(*name))? {
                Some(x) => rusqlite::types::Value::Real(x),
                None => rusqlite::types::Value::Null,
            });
        }
        day_rows.entry(day).or_default().push(values);
    }
    let mut stored = 0i64;
    for day in days {
        let values = &day_rows[day.as_str()];
        let state = if !complete {
            "partial"
        } else if values.is_empty() {
            "empty"
        } else {
            "complete"
        };
        connection.execute_batch("BEGIN IMMEDIATE")?;
        let day_result = (|| -> Result<i64, SourcesError> {
            let prior: Option<i64> = match connection.query_row(
                "SELECT complete FROM sync_log WHERE source=? AND resource=? AND date=?",
                rusqlite::params![source, resource, day],
                |r| r.get(0),
            ) {
                Ok(value) => Some(value),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(err) => return Err(err.into()),
            };
            if prior.is_some_and(|complete_flag| complete_flag != 0) && !complete {
                connection.execute(
                    "UPDATE sync_log SET last_attempt_state=?, last_attempt_at=? \
                     WHERE source=? AND resource=? AND date=?",
                    rusqlite::params![state, fetched_at, source, resource, day],
                )?;
                return Ok(0);
            }
            connection.execute(
                &format!("DELETE FROM {} WHERE resource = ? AND date = ?", spec.table),
                rusqlite::params![resource, day],
            )?;
            let insert = format!(
                "INSERT INTO {} ({}) VALUES ({})",
                spec.table,
                columns.join(", "),
                columns.iter().map(|_| "?").collect::<Vec<_>>().join(",")
            );
            for value_row in values {
                connection.execute(&insert, rusqlite::params_from_iter(value_row.iter()))?;
            }
            connection.execute(
                "INSERT OR REPLACE INTO sync_log \
                 (source,resource,date,rows,complete,fetched_at,state,last_attempt_state,last_attempt_at) \
                 VALUES (?,?,?,?,?,?,?,?,?)",
                rusqlite::params![
                    source,
                    resource,
                    day,
                    values.len() as i64,
                    i64::from(complete),
                    fetched_at,
                    state,
                    state,
                    fetched_at,
                ],
            )?;
            Ok(values.len() as i64)
        })();
        match day_result {
            Ok(count) => {
                connection.execute_batch("COMMIT")?;
                stored += count;
            }
            Err(err) => {
                let _ = connection.execute_batch("ROLLBACK");
                return Err(err);
            }
        }
    }
    Ok(stored)
}

/// `_mark_requested` — seed the ledger rows; lagged days stay uncomplete.
fn mark_requested(
    connection: &Connection,
    source: &str,
    resource: &str,
    days: &[String],
    available_end_iso: &str,
) -> Result<(), SourcesError> {
    connection.execute_batch("BEGIN")?;
    let result = (|| -> Result<(), SourcesError> {
        for day in days {
            let state = if day.as_str() > available_end_iso {
                "lagged"
            } else {
                "pending"
            };
            connection.execute(
                "INSERT OR IGNORE INTO sync_log(source,resource,date,state) VALUES(?,?,?,?)",
                rusqlite::params![source, resource, day, state],
            )?;
            if state == "lagged" {
                connection.execute(
                    "UPDATE sync_log SET state='lagged' WHERE source=? AND resource=? AND date=? \
                     AND complete=0 AND state IN ('lagged','pending')",
                    rusqlite::params![source, resource, day],
                )?;
            } else {
                connection.execute(
                    "UPDATE sync_log SET state='pending' WHERE source=? AND resource=? AND date=? \
                     AND complete=0 AND state='lagged'",
                    rusqlite::params![source, resource, day],
                )?;
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => connection.execute_batch("COMMIT")?,
        Err(err) => {
            let _ = connection.execute_batch("ROLLBACK");
            return Err(err);
        }
    }
    Ok(())
}

/// `_mark_failed` — flag the attempt; completed days keep their state.
fn mark_failed(
    connection: &Connection,
    source: &str,
    resource: &str,
    days: &[String],
    attempt_state: &str,
    at: &str,
) -> Result<(), SourcesError> {
    let attempt_state = match attempt_state {
        "failed" | "no_access" | "invalid_grant" | "insufficient_scope" => attempt_state,
        _ => "failed",
    };
    connection.execute_batch("BEGIN")?;
    let result = (|| -> Result<(), SourcesError> {
        for day in days {
            connection.execute(
                "UPDATE sync_log SET state=CASE WHEN complete=1 THEN state ELSE 'failed' END, \
                 last_attempt_state=?, last_attempt_at=? \
                 WHERE source=? AND resource=? AND date=?",
                rusqlite::params![attempt_state, at, source, resource, day],
            )?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => connection.execute_batch("COMMIT")?,
        Err(err) => {
            let _ = connection.execute_batch("ROLLBACK");
            return Err(err);
        }
    }
    Ok(())
}

/// True when the status is a number equal to `n`, so `429.0` matches `429`.
fn num_eq(value: Option<&Value>, n: i64) -> bool {
    match value {
        Some(Value::Number(v)) => {
            v.as_i64() == Some(n)
                || v.as_u64().map(|u| u == n as u64).unwrap_or(false)
                || v.as_f64() == Some(n as f64)
        }
        Some(Value::Bool(b)) => i64::from(*b) == n,
        _ => false,
    }
}

/// `_failure_state` — only explicit access-failure classes, never provider text.
fn failure_state(result: &Value) -> &'static str {
    let error = if pystr::truthy(result.get("error")) {
        result
            .get("error")
            .map(pystr::repr_value)
            .unwrap_or_default()
    } else {
        String::new()
    }
    .to_lowercase();
    if error.contains("invalid_grant") {
        return "invalid_grant";
    }
    if error.contains("insufficient_scope") || error.contains("insufficient scope") {
        return "insufficient_scope";
    }
    let status = result.get("status");
    if num_eq(status, 401) || num_eq(status, 403) {
        return "no_access";
    }
    "failed"
}

/// Normalize a path to POSIX text: collapse repeated slashes, drop `.` segments, keep `..` verbatim.
fn py_path_str(path: &Path) -> String {
    let text = path.to_string_lossy();
    let double_root = text.starts_with("//") && !text.starts_with("///");
    let mut parts: Vec<&str> = Vec::new();
    for segment in text.split('/') {
        match segment {
            "" | "." => {}
            other => parts.push(other),
        }
    }
    let body = parts.join("/");
    if double_root {
        return format!("//{body}");
    }
    match (text.starts_with('/'), body.is_empty()) {
        (true, true) => "/".to_owned(),
        (true, false) => format!("/{body}"),
        (false, true) => ".".to_owned(),
        (false, false) => body,
    }
}

pub mod fetchers;
mod query_ops;
mod sync_ops;

pub use query_ops::{query, status, QueryArgs};
pub use sync_ops::{sync, Fetcher, SyncArgs, SyncEnv};

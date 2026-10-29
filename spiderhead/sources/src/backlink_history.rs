//! Versioned, local backlink snapshots, kept apart from scan.v1 and provider time series.
//!
//! One private SQLite file (`backlinks.sqlite`) inside a project directory, or an
//! explicit path: a `backlink_meta` marker row plus a `backlink_snapshots` table keyed
//! by `(domain, observed_at, fingerprint)`. Appends are atomic and idempotent — the same
//! `(domain, observed_at, fingerprint)` triple re-saves to the same row.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{pyjson, pystr};

/// `FORMAT` — the history database identity.
pub const FORMAT: &str = "spiderhead.backlink-history.v1";
/// `DB_NAME` — the history file inside a project directory.
pub const DB_NAME: &str = "backlinks.sqlite";
/// `MAX_SNAPSHOT_BYTES` — one serialized snapshot's UTF-8 budget (64 MiB).
pub const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;

/// Errors this module returns; driver failures keep their SQLite or I/O message.
#[derive(Debug, Error)]
pub enum BlhError {
    /// Invalid input.
    #[error("{0}")]
    Invalid(String),
    /// Stored payload JSON is corrupt.
    #[error("{0}")]
    JsonDecode(String),
    /// Value has the wrong type.
    #[error("{0}")]
    TypeErr(String),
    /// A required key is missing; the message names it.
    #[error("{0}")]
    KeyErr(String),
    /// A value SQLite cannot bind.
    #[error("{0}")]
    Interface(String),
    /// Database error; the driver message is kept.
    #[error("{0}")]
    Sqlite(#[from] rusqlite::Error),
    /// Filesystem error.
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

impl BlhError {
    /// Error class name that callers see.
    pub fn python_class(&self) -> &'static str {
        match self {
            BlhError::Invalid(_) => "ValueError",
            BlhError::JsonDecode(_) => "JSONDecodeError",
            BlhError::TypeErr(_) => "TypeError",
            BlhError::KeyErr(_) => "KeyError",
            BlhError::Interface(_) => "InterfaceError",
            BlhError::Sqlite(err) => match err {
                rusqlite::Error::SqliteFailure(code, _) => match code.code {
                    rusqlite::ErrorCode::NotADatabase
                    | rusqlite::ErrorCode::DatabaseCorrupt
                    | rusqlite::ErrorCode::TypeMismatch => "DatabaseError",
                    _ => "OperationalError",
                },
                rusqlite::Error::ToSqlConversionFailure(_) => "InterfaceError",
                _ => "OperationalError",
            },
            BlhError::Io(err) => match err.kind() {
                std::io::ErrorKind::NotFound => "FileNotFoundError",
                std::io::ErrorKind::AlreadyExists => "FileExistsError",
                std::io::ErrorKind::PermissionDenied => "PermissionError",
                _ => "OSError",
            },
        }
    }
}

fn invalid<T>(message: impl Into<String>) -> Result<T, BlhError> {
    Err(BlhError::Invalid(message.into()))
}

fn type_err<T>(message: impl Into<String>) -> Result<T, BlhError> {
    Err(BlhError::TypeErr(message.into()))
}

/// Make a path absolute by prepending the working directory, without resolving links.
fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    }
}

#[cfg(unix)]
fn unix_nlink(meta: &fs::Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::nlink(meta)
}

#[cfg(not(unix))]
fn unix_nlink(_meta: &fs::Metadata) -> u64 {
    1
}

/// Create the database file once with owner-only permissions (0600); a concurrent create
/// that loses the race is tolerated.
fn create_private(path: &Path) -> Result<(), BlhError> {
    if path.symlink_metadata().is_err() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            fs::OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .mode(0o600)
                .open(path)?;
        }
        #[cfg(not(unix))]
        fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(path)?;
    }
    Ok(())
}

/// Resolve the database path from exactly one of `db` or `project`; a project directory
/// must contain `project.json`.
pub fn path_arg(db: Option<&Value>, project: Option<&Value>) -> Result<PathBuf, BlhError> {
    let has_db = db.is_some_and(|v| pystr::truthy(Some(v)));
    let has_project = project.is_some_and(|v| pystr::truthy(Some(v)));
    if has_db == has_project {
        return invalid("pass exactly one of db or project");
    }
    if has_project {
        let raw = project.unwrap_or(&Value::Null);
        let Some(text) = raw.as_str() else {
            return type_err(format!(
                "argument should be a str or an os.PathLike object where __fspath__ \
                 returns a str, not '{}'",
                pystr::type_name(raw)
            ));
        };
        let root = PathBuf::from(text);
        if !root.join("project.json").is_file() {
            return invalid("project requires project.json");
        }
        return Ok(root.join(DB_NAME));
    }
    let raw = db.unwrap_or(&Value::Null);
    Ok(PathBuf::from(pystr::repr_value(raw)))
}

fn table_names(con: &Connection) -> Result<Vec<String>, BlhError> {
    let names = con
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(names)
}

/// Stat without following links; the error text keeps the `[Errno N] ...: '<path>'` shape.
fn lstat_py(path: &Path) -> std::io::Result<std::fs::Metadata> {
    path.symlink_metadata().map_err(|err| {
        let (errno, text) = match err.kind() {
            std::io::ErrorKind::NotFound => (2, "No such file or directory".to_owned()),
            std::io::ErrorKind::PermissionDenied => (13, "Permission denied".to_owned()),
            std::io::ErrorKind::AlreadyExists => (17, "File exists".to_owned()),
            _ => (err.raw_os_error().unwrap_or(5), err.to_string()),
        };
        std::io::Error::new(
            err.kind(),
            format!("[Errno {errno}] {text}: '{}'", path.display()),
        )
    })
}

/// `_connect` — open/verify the history database, creating the schema on a fresh file.
fn connect(path: &Path, create: bool) -> Result<Connection, BlhError> {
    if create {
        create_private(path)?;
    }
    let info = lstat_py(path)?;
    if !info.is_file() || unix_nlink(&info) != 1 {
        return invalid("backlink DB must be a regular unaliased file");
    }
    let con = if create {
        spiderhead_core::sqlite::open_writer(path, spiderhead_core::sqlite::DEFAULT_TIMEOUT, &[])?
    } else {
        spiderhead_core::sqlite::open_readonly(
            &absolute(path),
            spiderhead_core::sqlite::DEFAULT_TIMEOUT,
            &[],
        )?
    };
    con.execute_batch("PRAGMA trusted_schema=OFF")?;
    con.execute_batch(if create { "BEGIN IMMEDIATE" } else { "BEGIN" })?;
    let check = (|| -> Result<(), BlhError> {
        let tables: std::collections::BTreeSet<String> = table_names(&con)?.into_iter().collect();
        if tables.is_empty() && create && info.len() == 0 {
            con.execute_batch(
                "CREATE TABLE backlink_meta(format TEXT PRIMARY KEY);
                 CREATE TABLE backlink_snapshots (id INTEGER PRIMARY KEY, domain TEXT NOT NULL,
                    observed_at TEXT NOT NULL, fingerprint TEXT NOT NULL, payload TEXT NOT NULL,
                    UNIQUE(domain, observed_at, fingerprint));
                 CREATE INDEX backlink_snapshots_domain ON backlink_snapshots(domain,id);",
            )?;
            con.execute("INSERT INTO backlink_meta VALUES (?)", [FORMAT])?;
        } else if tables
            != ["backlink_meta", "backlink_snapshots"]
                .iter()
                .map(|s| s.to_string())
                .collect::<std::collections::BTreeSet<_>>()
        {
            return invalid("existing database is not a backlink history database");
        } else {
            let formats: Vec<String> = con
                .prepare("SELECT format FROM backlink_meta")?
                .query_map([], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            if formats != [FORMAT.to_string()] {
                return invalid("unsupported backlink history format");
            }
        }
        if !create {
            con.execute_batch("PRAGMA query_only=ON")?;
        }
        Ok(())
    })();
    if check.is_err() {
        let _ = con.execute_batch("ROLLBACK");
    }
    check?;
    Ok(con)
}

/// Rejects non-finite numbers. A stored non-finite value is marked as
/// `{"$float": "nan"|"inf"|"-inf"}` and raises the same error as a plain NaN would.
fn strict_dumps(value: &Value) -> Result<String, BlhError> {
    fn find_nonfinite(value: &Value) -> Option<String> {
        match value {
            Value::Number(n) if n.as_f64().is_some_and(|f| !f.is_finite()) => {
                Some("nan".to_owned())
            }
            Value::Array(items) => items.iter().find_map(find_nonfinite),
            Value::Object(map) => {
                if map.len() == 1 {
                    if let Some(tag) = map.get("$float").and_then(Value::as_str) {
                        return Some(tag.to_owned());
                    }
                }
                map.values().find_map(find_nonfinite)
            }
            _ => None,
        }
    }
    if let Some(tag) = find_nonfinite(value) {
        return invalid(format!(
            "Out of range float values are not JSON compliant: {tag}"
        ));
    }
    Ok(pyjson::dumps_opts_compact(value, true, true))
}

/// A JSON value as a SQLite bind parameter; objects and arrays are refused.
fn sql_param(value: &Value, index: usize) -> Result<rusqlite::types::Value, BlhError> {
    use rusqlite::types::Value as Sql;
    Ok(match value {
        Value::Null => Sql::Null,
        Value::Bool(b) => Sql::Integer(i64::from(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Sql::Integer(i)
            } else if let Some(u) = n.as_u64() {
                Sql::Integer(i64::try_from(u).unwrap_or(i64::MAX))
            } else {
                Sql::Real(n.as_f64().unwrap_or_default())
            }
        }
        Value::String(s) => Sql::Text(s.clone()),
        _ => {
            return Err(BlhError::Interface(format!(
                "Error binding parameter {index} - probably unsupported type."
            )));
        }
    })
}

/// Index a row by key; a missing key or a non-object row is an error.
fn member(row: &Value, key: &str) -> Result<Value, BlhError> {
    match row {
        Value::Object(map) => map
            .get(key)
            .cloned()
            .ok_or_else(|| BlhError::KeyErr(pystr::repr_str(key))),
        Value::Array(_) => type_err("list indices must be integers or slices, not str"),
        Value::String(_) => type_err("string indices must be integers, not 'str'"),
        other => type_err(format!(
            "'{}' object is not subscriptable",
            pystr::type_name(other)
        )),
    }
}

/// Append one snapshot atomically. Retries are idempotent, and older snapshots stay
/// unchanged. The row commits before the before/after sets are computed, so a later
/// error still leaves the row stored.
pub fn save_snapshot(
    domain: &Value,
    rows: &Value,
    coverage: &Value,
    domain_edges: &Value,
    db: Option<&Value>,
    project: Option<&Value>,
    observed_at: &Value,
) -> Result<Value, BlhError> {
    let path = path_arg(db, project)?;
    let payload = strict_dumps(&json!({
        "rows": rows,
        "coverage": coverage,
        "domain_edges": domain_edges,
    }))?;
    if payload.len() > MAX_SNAPSHOT_BYTES {
        return invalid("backlink snapshot exceeds 64 MiB");
    }
    let fingerprint = format!("{:x}", Sha256::digest(payload.as_bytes()));
    let con = connect(&path, true)?;
    let prior = con
        .prepare(
            "SELECT id, payload FROM backlink_snapshots WHERE domain=? \
             ORDER BY id DESC LIMIT 1",
        )?
        .query_row([sql_param(domain, 0)?], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .optional()?;
    con.execute(
        "INSERT OR IGNORE INTO backlink_snapshots(domain,observed_at,fingerprint,payload) \
         VALUES(?,?,?,?)",
        rusqlite::params![
            sql_param(domain, 0)?,
            sql_param(observed_at, 1)?,
            fingerprint,
            payload,
        ],
    )?;
    let identifier: Option<i64> = con
        .prepare(
            "SELECT id FROM backlink_snapshots \
             WHERE domain=? AND observed_at=? AND fingerprint=?",
        )?
        .query_row(
            rusqlite::params![
                sql_param(domain, 0)?,
                sql_param(observed_at, 1)?,
                fingerprint
            ],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    con.execute_batch("COMMIT")?;
    let identifier = identifier
        .ok_or_else(|| BlhError::TypeErr("'NoneType' object is not subscriptable".to_owned()))?;
    let before = match &prior {
        Some((_, stored)) => edge_set(&load_rows(stored)?)?,
        None => std::collections::BTreeSet::new(),
    };
    let after = edge_set(rows)?;
    let confirmed_lost = lost_count(rows)?;
    Ok(json!({
        "db": path.to_string_lossy(),
        "snapshot_id": identifier,
        "fingerprint": fingerprint,
        "previous_snapshot_id": prior
            .map(|(id, _)| if id != identifier { json!(id) } else { Value::Null })
            .unwrap_or(Value::Null),
        "newly_observed": after.difference(&before).count(),
        "unobserved_since_previous": before.difference(&after).count(),
        "confirmed_lost": confirmed_lost,
        "absence_is_loss": false,
    }))
}

fn load_rows(payload: &str) -> Result<Value, BlhError> {
    let body: Value = serde_json::from_str(payload).map_err(|e| {
        BlhError::JsonDecode(pyjson::decode_error(payload).unwrap_or_else(|| e.to_string()))
    })?;
    member(&body, "rows")
}

/// Distinct (source, target) link pairs in the snapshot.
fn edge_set(rows: &Value) -> Result<std::collections::BTreeSet<String>, BlhError> {
    let list = pystr::iterate(rows).map_err(|(c, m)| pyerr_class(c, &m))?;
    let mut out = std::collections::BTreeSet::new();
    for row in &list {
        let edge = json!([member(row, "source_url")?, member(row, "target_url")?]);
        out.insert(pyjson::dumps(&edge));
    }
    Ok(out)
}

/// Number of rows whose state is `lost`.
fn lost_count(rows: &Value) -> Result<i64, BlhError> {
    let list = pystr::iterate(rows).map_err(|(c, m)| pyerr_class(c, &m))?;
    let mut count = 0i64;
    for row in &list {
        if member(row, "state")? == json!("lost") {
            count += 1;
        }
    }
    Ok(count)
}

fn pyerr_class(class: &str, message: &str) -> BlhError {
    match class {
        "TypeError" => BlhError::TypeErr(message.to_owned()),
        _ => BlhError::Invalid(message.to_owned()),
    }
}

/// Read snapshot history without creating a database or touching the project.
pub fn snapshot_history(
    db: Option<&Value>,
    project: Option<&Value>,
    domain: Option<&Value>,
    limit: Option<&Value>,
    offset: Option<&Value>,
) -> Result<Value, BlhError> {
    let default_limit = json!(100);
    let default_offset = json!(0);
    let limit = limit.unwrap_or(&default_limit);
    let offset = offset.unwrap_or(&default_offset);
    if !pystr::is_py_int(limit)
        || !(1..=1000).contains(&limit.as_i64().unwrap_or(0))
        || !pystr::is_py_int(offset)
        || offset.as_i64().unwrap_or(-1) < 0
    {
        return invalid("limit must be 1..1000 and offset must be nonnegative");
    }
    let path = path_arg(db, project)?;
    let con = connect(&path, false)?;
    let limit_i = limit.as_i64().unwrap_or(0);
    let offset_i = offset.as_i64().unwrap_or(0);
    let (total, snapshots) = (|| -> Result<(i64, Vec<Value>), BlhError> {
        let domain = domain.filter(|v| !v.is_null());
        let (clause, mut params) = match domain {
            Some(d) => (" WHERE domain=?", vec![sql_param(d, 0)?]),
            None => ("", Vec::new()),
        };
        let total: i64 = con
            .prepare(&format!("SELECT COUNT(*) FROM backlink_snapshots{clause}"))?
            .query_row(rusqlite::params_from_iter(params.iter()), |row| row.get(0))?;
        params.push(rusqlite::types::Value::Integer(limit_i));
        params.push(rusqlite::types::Value::Integer(offset_i));
        let rows = con
            .prepare(&format!(
                "SELECT id,domain,observed_at,fingerprint FROM backlink_snapshots{clause} \
                 ORDER BY id DESC LIMIT ? OFFSET ?"
            ))?
            .query_map(rusqlite::params_from_iter(params.iter()), |row| {
                Ok(json!({
                    "id": row.get::<_, i64>(0)?,
                    "domain": row.get::<_, String>(1)?,
                    "observed_at": row.get::<_, String>(2)?,
                    "fingerprint": row.get::<_, String>(3)?,
                }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok((total, rows))
    })()?;
    Ok(json!({
        "ok": true,
        "db": path.to_string_lossy(),
        "total": total,
        "snapshots": snapshots,
        "has_more": offset_i + limit_i < total,
    }))
}

/// Reads one stored backlink snapshot by its positive integer id.
pub fn read_snapshot(
    snapshot_id: &Value,
    db: Option<&Value>,
    project: Option<&Value>,
) -> Result<Value, BlhError> {
    if !pystr::is_py_int(snapshot_id) || snapshot_id.as_i64().unwrap_or(0) < 1 {
        return invalid("snapshot_id must be a positive integer");
    }
    let id = snapshot_id.as_i64().unwrap_or(0);
    let path = path_arg(db, project)?;
    let con = connect(&path, false)?;
    let row = con
        .prepare("SELECT domain,observed_at,payload FROM backlink_snapshots WHERE id=?")?
        .query_row([id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .optional()?;
    let Some((domain, observed_at, payload)) = row else {
        return invalid("backlink snapshot was not found");
    };
    let body: Value = serde_json::from_str(&payload).map_err(|e| {
        BlhError::JsonDecode(pyjson::decode_error(&payload).unwrap_or_else(|| e.to_string()))
    })?;
    let Some(map) = body.as_object() else {
        return type_err(format!(
            "'{}' object is not a mapping",
            pystr::type_name(&body)
        ));
    };
    let mut out = Map::new();
    out.insert("ok".to_owned(), json!(true));
    out.insert("snapshot_id".to_owned(), json!(id));
    out.insert("domain".to_owned(), json!(domain));
    out.insert("observed_at".to_owned(), json!(observed_at));
    for (key, value) in map {
        out.insert(key.clone(), value.clone());
    }
    Ok(Value::Object(out))
}

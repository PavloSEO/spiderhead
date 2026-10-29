//! The retained log artifact: a local SQLite file holding exact aggregates of imported
//! logs. Its identity and schema are fixed, so a foreign or tampered file is rejected
//! before any of its contents is trusted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{Connection, ToSql};
use serde_json::{Map, Value};

use crate::error::{LogsError, Result};
use crate::format::section as path_section;
use crate::urlsplit;

/// Application id stamped into every log artifact ("SHLG"); differs from scan artifacts.
pub const APPLICATION_ID: i64 = 0x5348_4C47;
/// Longest accepted input line, in characters.
pub const MAX_LINE_CHARS: usize = 65_536;
/// Free disk that must remain while importing.
pub const RESERVE_BYTES: u64 = 10 * 1024 * 1024 * 1024;

/// DDL of a log artifact; the stored text of each object is compared verbatim on open.
pub const SCHEMA: &str = "
PRAGMA user_version=1;
CREATE TABLE metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
CREATE TABLE sources(path TEXT PRIMARY KEY,size INTEGER,mtime_ns INTEGER,sha256 TEXT,
 digest_scope TEXT,format TEXT,lines INTEGER,complete INTEGER);
CREATE TABLE visits(bot TEXT NOT NULL,family TEXT NOT NULL,ip TEXT NOT NULL,url TEXT NOT NULL,
 date TEXT NOT NULL,status INTEGER NOT NULL,section TEXT NOT NULL,visits INTEGER NOT NULL,
 bytes INTEGER NOT NULL,last_visit TEXT,PRIMARY KEY(bot,ip,url,date,status)) WITHOUT ROWID;
CREATE INDEX visits_date ON visits(date);
CREATE INDEX visits_url ON visits(url);
CREATE TABLE bot_ips(bot TEXT NOT NULL,family TEXT NOT NULL,ip TEXT NOT NULL,
 visits INTEGER NOT NULL,last_visit TEXT,PRIMARY KEY(bot,ip)) WITHOUT ROWID;
CREATE TABLE dns_cache(ip TEXT,bot TEXT,payload TEXT,checked_at REAL,PRIMARY KEY(ip,bot));
";

/// Optional extension: every parsed request, one row per log line. `row_hash` is the first
/// 16 bytes (hex) of SHA-256 over the raw line, a NUL and the line's ordinal among equal
/// lines of the same file, so re-importing a file adds nothing while a genuinely repeated
/// request inside one file is kept. Aggregates in `visits` are built from these rows.
pub const REQUESTS_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS requests(row_hash TEXT PRIMARY KEY,ts TEXT,date TEXT NOT NULL,
 ip TEXT NOT NULL,bot TEXT NOT NULL,family TEXT NOT NULL,method TEXT NOT NULL,url TEXT NOT NULL,
 status INTEGER NOT NULL,bytes INTEGER NOT NULL,referer TEXT,user_agent TEXT,source TEXT NOT NULL) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS requests_url ON requests(url);
CREATE INDEX IF NOT EXISTS requests_date ON requests(date);
";

const REQUEST_COLUMNS: [&str; 13] = [
    "row_hash",
    "ts",
    "date",
    "ip",
    "bot",
    "family",
    "method",
    "url",
    "status",
    "bytes",
    "referer",
    "user_agent",
    "source",
];

fn expected_requests_schema() -> &'static BTreeMap<String, String> {
    static CELL: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    CELL.get_or_init(|| {
        let con = Connection::open_in_memory().expect("in-memory database opens");
        con.execute_batch(REQUESTS_SCHEMA).expect("schema is valid");
        schema_of(&con).unwrap_or_default()
    })
}

const COLUMNS: [(&str, &[&str]); 5] = [
    ("metadata", &["key", "value"]),
    (
        "sources",
        &[
            "path",
            "size",
            "mtime_ns",
            "sha256",
            "digest_scope",
            "format",
            "lines",
            "complete",
        ],
    ),
    (
        "visits",
        &[
            "bot",
            "family",
            "ip",
            "url",
            "date",
            "status",
            "section",
            "visits",
            "bytes",
            "last_visit",
        ],
    ),
    ("bot_ips", &["bot", "family", "ip", "visits", "last_visit"]),
    ("dns_cache", &["ip", "bot", "payload", "checked_at"]),
];

fn expected_schema() -> &'static BTreeMap<String, String> {
    static CELL: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    CELL.get_or_init(|| {
        let con = Connection::open_in_memory().expect("in-memory database opens");
        con.execute_batch(SCHEMA).expect("schema is valid");
        schema_of(&con).unwrap_or_default()
    })
}

fn schema_of(con: &Connection) -> rusqlite::Result<BTreeMap<String, String>> {
    let mut stmt =
        con.prepare("SELECT name,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'")?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
    })?;
    let mut out = BTreeMap::new();
    for row in rows {
        let (name, sql) = row?;
        out.insert(name, sql.unwrap_or_default());
    }
    Ok(out)
}

/// Bounds check for a paged-query integer.
pub fn positive(value: i64, name: &str, maximum: i64) -> Result<()> {
    if (1..=maximum).contains(&value) {
        Ok(())
    } else {
        Err(LogsError::value(format!(
            "{name} must be an integer between 1 and {maximum}"
        )))
    }
}

/// Bounds check for a page size and offset.
pub fn paging(limit: i64, offset: i64) -> Result<()> {
    positive(limit, "limit", 500)?;
    if (0..=100_000_000).contains(&offset) {
        Ok(())
    } else {
        Err(LogsError::value(
            "offset must be an integer between 0 and 100000000",
        ))
    }
}

/// First path segment of a URL's path component, `/` for the root.
pub fn url_section(url: &str) -> Result<String> {
    let parts = urlsplit::split(url)?;
    let path = parts.path.trim_matches('/');
    Ok(if path.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", path.split('/').next().unwrap_or(""))
    })
}

/// Whether a stored status, robots meta and X-Robots header allow indexing.
///
/// `None` when the status was never measured. Only the HTTP status and `noindex`/`none`
/// directives count; canonical and robots.txt are deliberately not inferred.
pub fn status_directives_indexable(
    status: Option<i64>,
    meta_robots: &str,
    x_robots: &str,
) -> Option<bool> {
    let status = status?;
    let joined = format!("{meta_robots},{x_robots}").to_lowercase();
    let blocked = joined
        .split(|c: char| crate::text::is_space(c) || matches!(c, ',' | ':' | ';'))
        .any(|d| d == "noindex" || d == "none");
    Some((200..300).contains(&status) && !blocked)
}

/// Regular files under `path` (a file or a directory tree), sorted. Symlinks, special
/// files and over-large trees are rejected.
pub fn collect_files(path: &Path, max_files: usize) -> Result<Vec<PathBuf>> {
    let meta = std::fs::symlink_metadata(path);
    if meta.as_ref().is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(LogsError::value("log source must not be a symlink"));
    }
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    if !path.is_dir() {
        return Err(LogsError::value(
            "log source must be an existing file or directory",
        ));
    }
    let mut files = Vec::new();
    let mut pending = vec![path.to_path_buf()];
    let mut entries = 0usize;
    while let Some(dir) = pending.pop() {
        for child in std::fs::read_dir(&dir)? {
            let child = child?;
            entries += 1;
            if entries > max_files {
                return Err(LogsError::value(
                    "log directory exceeds max_files entries; select a smaller source",
                ));
            }
            let kind = child.file_type()?;
            if kind.is_symlink() {
                return Err(LogsError::value("log directory must not contain symlinks"));
            } else if kind.is_dir() {
                pending.push(child.path());
            } else if kind.is_file() {
                files.push(child.path());
            } else {
                return Err(LogsError::value(
                    "log directory must contain only regular files",
                ));
            }
        }
    }
    if files.is_empty() {
        return Err(LogsError::value("log directory is empty"));
    }
    files.sort();
    Ok(files)
}

/// Newest retained store under a project's `logs` directory (newest mtime wins). A broken
/// newest import is reported by the caller's validation, never skipped for an older one.
pub fn latest_source(project_root: &Path) -> Result<String> {
    let none = || LogsError::value("project has no retained log stores; run logs-import first");
    let directory = project_root.join("logs");
    if std::fs::symlink_metadata(&directory).is_ok_and(|m| m.file_type().is_symlink())
        || !directory.is_dir()
    {
        return Err(none());
    }
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for child in std::fs::read_dir(&directory)? {
        let child = child?;
        let kind = child.file_type()?;
        if kind.is_symlink() || !kind.is_file() {
            return Err(LogsError::value(
                "project logs directory must contain only regular files",
            ));
        }
        if !child.file_name().to_string_lossy().ends_with(".sqlite") {
            continue;
        }
        let modified = child.metadata()?.modified()?;
        if newest.as_ref().is_none_or(|(best, _)| modified > *best) {
            newest = Some((modified, child.path()));
        }
    }
    newest
        .map(|(_, path)| path.to_string_lossy().into_owned())
        .ok_or_else(none)
}

fn is_count(value: Option<&Value>) -> bool {
    value.is_some_and(|v| v.as_i64().is_some_and(|n| n >= 0) || v.as_u64().is_some())
}

/// Open and validate the log artifact read-only. Reading never performs DNS or writes.
pub fn open_log_source(source: &str) -> Result<Connection> {
    let path = std::path::absolute(source)?;
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) || !path.is_file()
    {
        return Err(LogsError::value(
            "log artifact must be an existing regular file",
        ));
    }
    let con = spiderhead_core::sqlite::open_readonly(
        &path,
        Duration::from_secs(5),
        &[
            "PRAGMA trusted_schema=OFF",
            "PRAGMA cache_size=-8192",
            "PRAGMA temp_store=FILE",
        ],
    )?;
    validate(&con)?;
    Ok(con)
}

fn validate(con: &Connection) -> Result<()> {
    let application: i64 = con.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    let version: i64 = con.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if application != APPLICATION_ID || version != 1 {
        return Err(LogsError::value(
            "unsupported log artifact identity or version",
        ));
    }
    let mut stmt = con.prepare("SELECT name FROM sqlite_master WHERE type='table'")?;
    let tables: std::collections::BTreeSet<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut expected: std::collections::BTreeSet<String> =
        COLUMNS.iter().map(|(t, _)| t.to_string()).collect();
    let with_requests = tables.contains("requests");
    if with_requests {
        expected.insert("requests".into());
    }
    if tables != expected {
        return Err(LogsError::value("unsupported log artifact schema"));
    }
    let mut want = expected_schema().clone();
    if with_requests {
        want.extend(expected_requests_schema().clone());
    }
    if schema_of(con)? != want {
        return Err(LogsError::value(
            "unsupported log artifact schema definitions",
        ));
    }
    if with_requests {
        let mut info = con.prepare("PRAGMA table_info(requests)")?;
        let names: Vec<String> = info
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<_>>()?;
        if names != REQUEST_COLUMNS {
            return Err(LogsError::value("unsupported log artifact columns"));
        }
    }
    for (table, columns) in COLUMNS {
        let mut info = con.prepare(&format!("PRAGMA table_info({table})"))?;
        let names: Vec<String> = info
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<_>>()?;
        if names != columns {
            return Err(LogsError::value("unsupported log artifact columns"));
        }
    }
    let header: Option<String> = con
        .query_row("SELECT value FROM metadata WHERE key='import'", [], |r| {
            r.get(0)
        })
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    let header = header
        .filter(|h| h.chars().count() <= 65_536)
        .ok_or_else(|| LogsError::value("log import metadata is missing or oversized"))?;
    let document: Value = serde_json::from_str(&header)
        .map_err(|e| LogsError::value(format!("log import metadata is not JSON: {e}")))?;
    if document.get("format") != Some(&Value::from("logs.v1")) {
        return Err(LogsError::value("unsupported log import metadata"));
    }
    if let Some(target) = document.get("project_target") {
        if !target.as_str().is_some_and(|t| t.chars().count() <= 2048) {
            return Err(LogsError::value("log project origin metadata is invalid"));
        }
    }
    if !document.get("complete").is_some_and(Value::is_boolean)
        || !document
            .get("bot_detection_complete")
            .is_some_and(Value::is_boolean)
    {
        return Err(LogsError::value("log completeness metadata is invalid"));
    }
    let counts = document.get("lines").and_then(Value::as_object);
    let counts_ok = counts.is_some_and(|c| {
        [
            "total",
            "parsed",
            "skipped",
            "unknown_time",
            "unknown_user_agent",
        ]
        .iter()
        .all(|k| is_count(c.get(*k)))
    });
    if !counts_ok {
        return Err(LogsError::value("log line counts are invalid"));
    }
    let period_ok = document
        .get("period")
        .and_then(Value::as_object)
        .is_some_and(|p| {
            p.len() == 2
                && ["from", "to"]
                    .iter()
                    .all(|k| p.get(*k).is_some_and(|v| v.is_null() || v.is_string()))
        });
    if !period_ok {
        return Err(LogsError::value("log period metadata is invalid"));
    }
    Ok(())
}

/// The import metadata document stored in an artifact.
pub fn import_metadata(con: &Connection) -> Result<Value> {
    let text: String = con.query_row("SELECT value FROM metadata WHERE key='import'", [], |r| {
        r.get(0)
    })?;
    serde_json::from_str(&text).map_err(|e| LogsError::value(e.to_string()))
}

/// Convert a result row into a JSON object keyed by column name.
pub fn row_to_json(
    row: &rusqlite::Row<'_>,
    names: &[String],
) -> rusqlite::Result<Map<String, Value>> {
    let mut out = Map::new();
    for (i, name) in names.iter().enumerate() {
        let value = match row.get_ref(i)? {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(n) => Value::from(n),
            ValueRef::Real(f) => Value::from(f),
            ValueRef::Text(t) => Value::from(String::from_utf8_lossy(t).into_owned()),
            ValueRef::Blob(_) => Value::Null,
        };
        out.insert(name.clone(), value);
    }
    Ok(out)
}

/// Run `query` with `params` and return every row as a JSON object.
pub fn query_rows(
    con: &Connection,
    query: &str,
    params: &[SqlValue],
) -> rusqlite::Result<Vec<Map<String, Value>>> {
    let mut stmt = con.prepare(query)?;
    let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let refs: Vec<&dyn ToSql> = params.iter().map(|p| p as &dyn ToSql).collect();
    let rows = stmt.query_map(refs.as_slice(), |r| row_to_json(r, &names))?;
    rows.collect()
}

/// Section of a stored request path (first segment, `/` for the root).
pub fn path_section_of(path: &str) -> String {
    path_section(path)
}

/// Whether the artifact keeps one row per request.
pub fn has_requests(con: &Connection) -> bool {
    con.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name='requests'",
        [],
        |_| Ok(()),
    )
    .is_ok()
}

/// Remove artifacts of `<root>/logs` that never finished importing: files without the
/// import metadata row (or unreadable as SQLite) that were last written more than
/// `max_age` seconds before `now`. Finished artifacts and young files, which may belong to
/// a running import, are never touched. Returns the removed paths.
pub fn cleanup_stale_imports(root: &Path, now: f64, max_age: f64) -> Result<Vec<PathBuf>> {
    let directory = root.join("logs");
    if !directory.is_dir()
        || std::fs::symlink_metadata(&directory)?
            .file_type()
            .is_symlink()
    {
        return Ok(Vec::new());
    }
    let mut removed = Vec::new();
    for child in std::fs::read_dir(&directory)? {
        let child = child?;
        let name = child.file_name().to_string_lossy().into_owned();
        if !child.file_type()?.is_file() || !name.ends_with(".sqlite") {
            continue;
        }
        let modified = child
            .metadata()?
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64());
        if now - modified < max_age {
            continue;
        }
        let path = child.path();
        let finished =
            Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .and_then(|c| {
                    c.query_row("SELECT 1 FROM metadata WHERE key='import'", [], |_| Ok(()))
                })
                .is_ok();
        if !finished {
            std::fs::remove_file(&path)?;
            let _ = std::fs::remove_file(format!("{}-journal", path.display()));
            removed.push(path);
        }
    }
    removed.sort();
    Ok(removed)
}

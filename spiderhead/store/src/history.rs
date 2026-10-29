//! Explicit local scan history operations; no catalog or background cleanup.

use std::fs::{File, Metadata, OpenOptions, TryLockError};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::types::Value as SqlValue;
use rusqlite::{params_from_iter, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use spiderhead_core::common::python_json;
use spiderhead_core::filesystem::{link_identity, path_identity, FileIdentity, StatCompat};
use spiderhead_core::sqlite::open_readonly;

use crate::audit_v2::audit_v2_path;
use crate::error::StoreError;
use crate::link_context::cell_value;
use crate::scan_artifact::{expected, APPLICATION_ID, MAX_RECORD_BYTES, USER_VERSION};

/// Table columns exposed by `inspect_scan`, keyed by table name.
pub const TABLE_COLUMNS: &[(&str, &str)] = &[
    ("pages", "p.*, u.url"),
    ("links", "l.*, source.url AS source, destination.url AS destination"),
    ("forms", "*"),
    ("decisions", "*"),
    ("frontier", "f.*, u.url"),
    ("query_variants", "*"),
    ("context_items", "*"),
    ("responses", "response_id, request_url_id, request_ordinal, status_code, content_type, reported_size_bytes, body_sha256, body_state, body_reason, error, error_kind"),
    ("documents", "document_id, url_id, representation, source_response_id, body_sha256, captured_at, fidelity, body_state, body_reason"),
    ("resource_refs", "*"),
    ("audit", "singleton, schema_version, evidence_revision, analyzer_version, analyzer_revision, created_at, sha256"),
];

/// Metadata records of scans or per-file errors.
pub type Records = Vec<Map<String, Value>>;

/// Warning threshold for the whole history directory (20 GiB).
pub const DEFAULT_HISTORY_WARNING_BYTES: i64 = 20 * 1024 * 1024 * 1024;

/// Byte cap of the metadata assembled for one directory listing.
const CATALOG_BYTE_LIMIT: usize = 64 * 1024 * 1024;
/// Maximum number of scan files one catalog accepts.
const CATALOG_FILE_LIMIT: usize = 10_000;

fn scan_error(message: impl Into<String>) -> StoreError {
    StoreError::Invalid(message.into())
}

/// Civil date from days since 1970-01-01 (proleptic Gregorian).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Days since 1970-01-01 of a civil date.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Canonical 32-hex UUID from a hyphenated or plain string, or `None` when malformed.
fn uuid_hex(text: &str) -> Option<String> {
    let plain: String = text.chars().filter(|c| *c != '-').collect();
    let hyphen_ok = text.len() == 36
        && [8, 13, 18, 23]
            .iter()
            .all(|&i| text.as_bytes().get(i) == Some(&b'-'))
        || text.len() == 32;
    if plain.len() == 32 && hyphen_ok && plain.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(plain.to_ascii_lowercase())
    } else {
        None
    }
}

/// Lowercase host of a URL, with non-alphanumeric runs turned into `-`.
fn host_slug(start_url: &str) -> String {
    let raw = start_url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(start_url);
    let authority = raw.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority
        .rsplit('@')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .to_lowercase();
    let mut out = String::new();
    let mut in_run = false;
    for ch in host.chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '.' || ch == '-' {
            out.push(ch);
            in_run = false;
        } else if !in_run {
            out.push('-');
            in_run = true;
        }
    }
    let trimmed = out.trim_matches(|c| c == '.' || c == '-').to_string();
    if trimmed.is_empty() {
        "scan".into()
    } else {
        trimmed
    }
}

/// File-name stamp of a creation time given as Unix seconds (UTC): `YYYYMMDDTHHMMSSZ`.
fn stamp(epoch_seconds: i64) -> String {
    let days = epoch_seconds.div_euclid(86_400);
    let secs = epoch_seconds.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// Collision-safe scan file name: `<stamp>_<host>_<uuid8>.sqlite`.
pub fn recommended_name(
    start_url: &str,
    scan_uuid: &str,
    created_at: Option<i64>,
) -> Result<String, StoreError> {
    let host = host_slug(start_url);
    let now = created_at.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    });
    let short = uuid_hex(scan_uuid).ok_or_else(|| scan_error("invalid scan UUID"))?;
    Ok(format!("{}_{host}_{}.sqlite", stamp(now), &short[..8]))
}

/// Generated path inside `directory`; refuses a missing directory or an existing name.
pub fn new_scan_path(
    directory: &Path,
    start_url: &str,
    scan_uuid: &str,
) -> Result<PathBuf, StoreError> {
    if !directory.is_dir() {
        return Err(scan_error("selected scans directory does not exist"));
    }
    let path = directory.join(recommended_name(start_url, scan_uuid, None)?);
    if std::fs::symlink_metadata(&path).is_ok() {
        return Err(scan_error("generated scan path already exists"));
    }
    Ok(path)
}

/// Writer-lock file that guards a scan (`<name>.writer.lock`).
pub fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".writer.lock");
    path.with_file_name(name)
}

/// Regular, unaliased file metadata (no symlink follow).
pub fn regular(path: &Path) -> Result<Metadata, StoreError> {
    let info = std::fs::symlink_metadata(path).map_err(|e| {
        // Callers hand in the caller-relative path; the diagnostic names it
        // absolute, like the original's `Path.absolute()` before `lstat`.
        let shown = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        scan_error(format!(
            "scan file is unavailable: {}",
            spiderhead_core::common::python_os_error(&e, Some(&shown))
        ))
    })?;
    let unaliased = link_identity(path).map(|id| id.2 == 1).unwrap_or(false);
    if !info.file_type().is_file() || !unaliased {
        return Err(scan_error(
            "scan path must be a regular unaliased SQLite file",
        ));
    }
    Ok(info)
}

/// dev+inode equality of two identity snapshots.
fn same_identity(a: FileIdentity, b: FileIdentity) -> bool {
    (a.0, a.1) == (b.0, b.1)
}

/// Open the writer lock file, refusing symlinks and aliased files.
fn open_lock(path: &Path) -> Result<File, StoreError> {
    let lock = lock_path(path);
    if std::fs::symlink_metadata(&lock).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(scan_error("scan writer lock path must not be a symlink"));
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock)?;
    let info = file.metadata()?;
    let unaliased = spiderhead_core::filesystem::file_identity(&file)
        .map(|id| id.2 == 1)
        .unwrap_or(false);
    if !info.file_type().is_file() || !unaliased {
        return Err(scan_error("scan lock must be a regular unaliased file"));
    }
    Ok(file)
}

/// True when another process holds the writer lock of this scan.
pub fn lock_is_active(path: &Path) -> Result<bool, StoreError> {
    let lock = lock_path(path);
    if std::fs::symlink_metadata(&lock).is_err() {
        return Ok(false);
    }
    let file = open_lock(path)?;
    match file.try_lock() {
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(e)) => Err(e.into()),
        Ok(()) => {
            file.unlock()?;
            Ok(false)
        }
    }
}

/// Take the exclusive writer lock; the lock is released when the returned file is dropped.
pub fn hold_writer_lock(path: &Path) -> Result<File, StoreError> {
    let file = open_lock(path)?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(_) => Err(scan_error("scan has an active writer")),
    }
}

/// Open a scan read-only with identity, schema and version checks; no migration.
pub fn metadata_connection(path: &Path, allow_native_v2: bool) -> Result<Connection, StoreError> {
    regular(path)?;
    let before = link_identity(path)?;
    let con = open_readonly(
        &path.canonicalize().unwrap_or_else(|_| path.to_path_buf()),
        Duration::from_secs(5),
        &[
            "PRAGMA trusted_schema=OFF",
            "PRAGMA query_only=ON",
            "PRAGMA cache_size=-8192",
            "PRAGMA temp_store=FILE",
        ],
    )?;
    let _deadline = crate::link_context::arm_deadline(&con, Duration::from_secs(5));
    let app: i64 = con.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    if app != APPLICATION_ID {
        return Err(scan_error("foreign application_id"));
    }
    let version: i64 = con.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version == 2 && allow_native_v2 {
        return Err(scan_error(
            "native scan v2 opening is not yet available in this build",
        ));
    }
    if version != USER_VERSION {
        return Err(scan_error(
            "unsupported scan user_version; no automatic migration",
        ));
    }
    let expected_schema = expected()?;
    let objects = crate::scan_artifact::objects(&con)?;
    if objects != expected_schema.objects {
        return Err(scan_error("scan.v1 schema differs"));
    }
    let format: Option<String> = con
        .query_row(
            "SELECT format_version FROM scan WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let header_rows: i64 = con.query_row("SELECT COUNT(*) FROM scan", [], |r| r.get(0))?;
    if format.as_deref() != Some("scan.v1") || header_rows != 1 {
        return Err(scan_error("scan.v1 requires one matching format header"));
    }
    regular(path)?;
    if !same_identity(before, link_identity(path)?) {
        return Err(scan_error("scan identity changed while opening"));
    }
    Ok(con)
}

/// Parse an ISO 8601 timestamp with a UTC offset into Unix microseconds.
///
/// RFC 3339 timestamp with `Z` accepted; a timestamp without an offset is an error.
fn parse_timestamp_micros(text: &str) -> Result<i64, StoreError> {
    let bad = || scan_error("scan timestamp must include its UTC offset");
    let text = text.trim().replace('Z', "+00:00");
    // Fixed-offset byte slices below require an all-ASCII timestamp.
    if !text.is_ascii() {
        return Err(bad());
    }
    let bytes = text.as_bytes();
    if bytes.len() < 19
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !(bytes[10] == b'T' || bytes[10] == b' ')
    {
        return Err(bad());
    }
    let num = |s: &str| s.parse::<i64>().map_err(|_| bad());
    let year = num(&text[0..4])?;
    let month = num(&text[5..7])? as u32;
    let day = num(&text[8..10])? as u32;
    let hour = num(&text[11..13])?;
    let minute = num(&text[14..16])?;
    let second = num(&text[17..19])?;
    let mut rest = &text[19..];
    let mut micros = 0i64;
    if let Some(frac) = rest.strip_prefix('.') {
        let digits: String = frac.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() || digits.len() > 6 {
            return Err(bad());
        }
        micros = format!("{digits:0<6}").parse::<i64>().map_err(|_| bad())?;
        rest = &frac[digits.len()..];
    }
    let offset_seconds = if rest.is_empty() {
        return Err(bad());
    } else {
        let sign = match rest.as_bytes()[0] {
            b'+' => 1,
            b'-' => -1,
            _ => return Err(bad()),
        };
        let body = rest[1..].replace(':', "");
        if body.len() != 4 {
            return Err(bad());
        }
        sign * (num(&body[0..2])? * 3600 + num(&body[2..4])? * 60)
    };
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return Err(bad());
    }
    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset_seconds;
    Ok(seconds * 1_000_000 + micros)
}

/// Finished time in Unix microseconds; an absent time sorts first.
pub fn finished_at(value: Option<&str>) -> Result<i64, StoreError> {
    match value {
        None | Some("") => Ok(i64::MIN),
        Some(text) => parse_timestamp_micros(text),
    }
}

/// Header row of a scan: the metadata columns checked for shape and state.
pub fn read_scan(path: &Path) -> Result<Map<String, Value>, StoreError> {
    const COLUMNS: &[&str] = &[
        "scan_uuid",
        "lifecycle",
        "finish_reason",
        "pinned",
        "crawl_partial",
        "corpus_partial",
        "start_url",
        "config_fingerprint",
        "finished_at",
        "created_at",
        "retention_json",
        "writer_version",
        "writer_revision",
        "source_kind",
        "parent_scan_uuid",
        "evidence_revision",
        "capabilities_json",
        "format_version",
    ];
    let con = metadata_connection(path, false)?;
    let size_expr = COLUMNS
        .iter()
        .map(|c| format!("COALESCE(length(CAST({c} AS BLOB)),0)"))
        .collect::<Vec<_>>()
        .join("+");
    let size: Option<i64> = con
        .query_row(
            &format!("SELECT {size_expr} FROM scan WHERE singleton=1"),
            [],
            |r| r.get(0),
        )
        .optional()?;
    match size {
        Some(bytes) if bytes <= MAX_RECORD_BYTES => {}
        _ => {
            return Err(scan_error(
                "scan header is absent or exceeds metadata byte limit",
            ))
        }
    }
    let mut statement = con.prepare(&format!(
        "SELECT {} FROM scan WHERE singleton=1",
        COLUMNS.join(",")
    ))?;
    let row = statement
        .query_row([], |r| {
            COLUMNS
                .iter()
                .enumerate()
                .map(|(i, name)| Ok(((*name).to_string(), cell_value(r.get_ref(i)?))))
                .collect::<Result<Map<String, Value>, rusqlite::Error>>()
        })
        .map_err(|_| scan_error("invalid scan metadata: header row is absent"))?;
    let valid_uuid = row["scan_uuid"].as_str().and_then(uuid_hex).is_some();
    let lifecycle_ok = matches!(
        row["lifecycle"].as_str(),
        Some("running" | "interrupted" | "finished" | "failed")
    );
    let flags_ok = matches!(row["pinned"].as_i64(), Some(0 | 1))
        && matches!(row["crawl_partial"].as_i64(), Some(0 | 1));
    if !valid_uuid
        || row["format_version"].as_str() != Some("scan.v1")
        || !flags_ok
        || !lifecycle_ok
    {
        return Err(scan_error("invalid scan metadata: invalid scan state"));
    }
    let finished = row["finished_at"].as_str().map(str::to_owned);
    finished_at(finished.as_deref())
        .map_err(|e| scan_error(format!("invalid scan metadata: {e}")))?;
    Ok(row)
}

/// Full history record of one scan: identity, size, state and warning threshold.
pub fn metadata(path: &Path) -> Result<Map<String, Value>, StoreError> {
    let row = read_scan(path)?;
    let state = regular(path)?;
    let state_id = link_identity(path)?;
    let start = row["start_url"].as_str().unwrap_or("").to_string();
    let retention: Value = serde_json::from_str(row["retention_json"].as_str().unwrap_or(""))
        .map_err(|e| scan_error(format!("invalid scan history metadata: {e}")))?;
    let warning = retention
        .get("history_warning_bytes")
        .and_then(Value::as_i64)
        .filter(|v| *v >= 0)
        .ok_or_else(|| scan_error("invalid scan history metadata: invalid history warning size"))?;
    let capabilities: Value = serde_json::from_str(row["capabilities_json"].as_str().unwrap_or(""))
        .map_err(|e| scan_error(format!("invalid scan history metadata: {e}")))?;
    let mut companions: i64 = 0;
    for suffix in ["-wal", "-shm"] {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(suffix);
        let companion = path.with_file_name(name);
        if std::fs::symlink_metadata(&companion).is_ok() {
            companions += regular(&companion)?.len() as i64;
        }
    }
    let audit_companion = audit_v2_path(path);
    let audit_state = if std::fs::symlink_metadata(&audit_companion).is_ok() {
        Some(regular(&audit_companion)?)
    } else {
        None
    };
    let audit_bytes = audit_state.as_ref().map_or(0, |m| m.len() as i64);
    let host = {
        let raw = start.split_once("://").map(|(_, r)| r).unwrap_or(&start);
        raw.split(['/', '?', '#'])
            .next()
            .unwrap_or("")
            .rsplit('@')
            .next()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("")
            .to_lowercase()
    };
    let mut out = Map::new();
    out.insert("uuid".into(), row["scan_uuid"].clone());
    out.insert(
        "path".into(),
        json!(path
            .canonicalize()
            .unwrap_or_else(|_| path.to_path_buf())
            .display()
            .to_string()),
    );
    out.insert(
        "inode".into(),
        json!({"device": state_id.0, "inode": state_id.1}),
    );
    out.insert(
        "mtime_ns".into(),
        json!(state.mtime() * 1_000_000_000 + state.mtime_nsec()),
    );
    out.insert("bytes".into(), json!(state.len()));
    out.insert(
        "disk_bytes".into(),
        json!(state.len() as i64 + companions + audit_bytes),
    );
    out.insert("audit_bytes".into(), json!(audit_bytes));
    out.insert(
        "audit_inode".into(),
        match audit_state {
            Some(_) => {
                let m = link_identity(&audit_companion)?;
                json!({"device": m.0, "inode": m.1})
            }
            None => Value::Null,
        },
    );
    out.insert("lifecycle".into(), row["lifecycle"].clone());
    out.insert("finish_reason".into(), row["finish_reason"].clone());
    out.insert("pinned".into(), json!(row["pinned"].as_i64() == Some(1)));
    out.insert(
        "crawl_partial".into(),
        json!(row["crawl_partial"].as_i64() == Some(1)),
    );
    out.insert(
        "corpus_partial".into(),
        json!(row["corpus_partial"].as_i64() == Some(1)),
    );
    out.insert("start_url".into(), json!(start));
    out.insert("host".into(), json!(host));
    out.insert(
        "config_fingerprint".into(),
        row["config_fingerprint"].clone(),
    );
    out.insert("created_at".into(), row["created_at"].clone());
    out.insert("finished_at".into(), row["finished_at"].clone());
    out.insert("writer_version".into(), row["writer_version"].clone());
    out.insert("writer_revision".into(), row["writer_revision"].clone());
    out.insert("source_kind".into(), row["source_kind"].clone());
    out.insert("parent_scan_uuid".into(), row["parent_scan_uuid"].clone());
    out.insert("evidence_revision".into(), row["evidence_revision"].clone());
    out.insert("capabilities".into(), capabilities);
    out.insert("history_warning_bytes".into(), json!(warning));
    Ok(out)
}

/// Resolve and check a scans directory.
pub fn directory(directory: &Path) -> Result<PathBuf, StoreError> {
    let root = directory
        .canonicalize()
        .map_err(|_| scan_error("selected scans directory does not exist"))?;
    if !root.is_dir() {
        return Err(scan_error("selected scans directory does not exist"));
    }
    Ok(root)
}

/// Scan metadata of every `*.sqlite` file in a directory, newest first, with per-file errors.
pub fn catalog(directory_path: &Path) -> Result<(Records, Records), StoreError> {
    let mut root = directory(directory_path)?;
    if root.file_name().is_none_or(|n| n != "scans")
        && root.join("project.json").is_file()
        && root.join("scans").is_dir()
    {
        root = root.join("scans");
    }
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(&root)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            name.ends_with(".sqlite")
                && !name.ends_with(".audit-v2.sqlite")
                && !name.starts_with(".native-scan-")
                && !name.starts_with(".origin-pacing")
        })
        .collect();
    candidates.sort();
    let mut items: Vec<Map<String, Value>> = Vec::new();
    let mut errors: Vec<Map<String, Value>> = Vec::new();
    let mut used = 0usize;
    for (index, path) in candidates.iter().enumerate() {
        if index >= CATALOG_FILE_LIMIT {
            return Err(scan_error(
                "scan directory exceeds the 10,000-file history limit",
            ));
        }
        match metadata(path) {
            Err(error) => {
                let mut entry = Map::new();
                entry.insert("path".into(), json!(path.display().to_string()));
                let reason: String = error.to_string().chars().take(2048).collect();
                entry.insert("reason".into(), json!(reason));
                used += python_json(&Value::Object(entry.clone())).len();
                if used > CATALOG_BYTE_LIMIT {
                    return Err(scan_error("scan history metadata exceeds 64 MiB"));
                }
                errors.push(entry);
            }
            Ok(item) => {
                used += python_json(&Value::Object(item.clone())).len();
                if used > CATALOG_BYTE_LIMIT {
                    return Err(scan_error("scan history metadata exceeds 64 MiB"));
                }
                items.push(item);
            }
        }
    }
    items.sort_by(|a, b| {
        let key = |m: &Map<String, Value>| {
            let time = m["finished_at"]
                .as_str()
                .filter(|s| !s.is_empty())
                .or(m["created_at"].as_str());
            (
                finished_at(time).unwrap_or(i64::MIN),
                m["uuid"].as_str().unwrap_or("").to_string(),
            )
        };
        key(b).cmp(&key(a))
    });
    Ok((items, errors))
}

/// Validate paging arguments: offset >= 0 and limit in 1..=1000.
pub fn pagination(offset: i64, limit: i64) -> Result<(), StoreError> {
    if offset < 0 || !(1..=1000).contains(&limit) {
        return Err(scan_error(
            "offset must be nonnegative and limit must be 1..1000",
        ));
    }
    Ok(())
}

/// One page of the history directory with its aggregate size and warning state.
pub fn list_scans(
    directory_path: &Path,
    offset: i64,
    limit: i64,
) -> Result<Map<String, Value>, StoreError> {
    pagination(offset, limit)?;
    let (items, errors) = catalog(directory_path)?;
    let history_bytes: i64 = items
        .iter()
        .map(|i| i["disk_bytes"].as_i64().unwrap_or(0))
        .sum();
    // A zero threshold means "disabled"; disabled per-scan policies do not select the shared
    // warning threshold.
    let thresholds: Vec<i64> = items
        .iter()
        .filter_map(|i| i["history_warning_bytes"].as_i64())
        .filter(|v| *v != 0)
        .collect();
    let warning = match thresholds.iter().min() {
        Some(min) => *min,
        None if !items.is_empty() => 0,
        None => DEFAULT_HISTORY_WARNING_BYTES,
    };
    let start = offset as usize;
    let end = (offset as usize)
        .saturating_add(limit as usize)
        .min(items.len());
    let page: Vec<Value> = if start < end {
        items[start..end]
            .iter()
            .cloned()
            .map(Value::Object)
            .collect()
    } else {
        Vec::new()
    };
    let mut out = Map::new();
    out.insert("total".into(), json!(items.len()));
    out.insert("offset".into(), json!(offset));
    out.insert("items".into(), Value::Array(page));
    out.insert(
        "has_more".into(),
        json!(((offset + limit) as usize) < items.len()),
    );
    out.insert(
        "errors".into(),
        Value::Array(errors.into_iter().map(Value::Object).collect()),
    );
    out.insert("history_bytes".into(), json!(history_bytes));
    out.insert("history_warning_bytes".into(), json!(warning));
    out.insert(
        "history_warning".into(),
        json!(warning > 0 && history_bytes >= warning),
    );
    Ok(out)
}

/// Columns of one scan table, with pagination and a per-row byte guard.
pub fn inspect_scan(
    path: &Path,
    table: &str,
    offset: i64,
    limit: i64,
    max_bytes: i64,
    columns: Option<&[String]>,
    total: bool,
) -> Result<Map<String, Value>, StoreError> {
    let Some((_, select_list)) = TABLE_COLUMNS.iter().find(|(name, _)| *name == table) else {
        return Err(scan_error("inspection table is not allowed"));
    };
    pagination(offset, limit)?;
    if !(1..=MAX_RECORD_BYTES).contains(&max_bytes) {
        return Err(scan_error("max_bytes must be within 1..8 MiB"));
    }
    let con = metadata_connection(
        &path.canonicalize().unwrap_or_else(|_| path.to_path_buf()),
        true,
    )?;
    let base = match table {
        "pages" => "SELECT p.*,u.url FROM pages p JOIN urls u USING(url_id) ORDER BY p.page_ordinal".to_string(),
        "links" => "SELECT l.*,source.url AS source,destination.url AS destination FROM links l JOIN urls source ON source.url_id=l.source_url_id JOIN urls destination ON destination.url_id=l.destination_url_id ORDER BY l.link_id".to_string(),
        "frontier" => "SELECT f.*,u.url FROM frontier f JOIN urls u USING(url_id) ORDER BY f.queue_ordinal".to_string(),
        _ => {
            let mut statement = con.prepare(&format!("PRAGMA table_info({table})"))?;
            let mut primary: Vec<(i64, String)> = statement
                .query_map([], |r| Ok((r.get::<_, i64>(5)?, r.get::<_, String>(1)?)))?
                .collect::<Result<_, _>>()?;
            primary.retain(|(pk, _)| *pk > 0);
            primary.sort();
            let order = primary
                .iter()
                .map(|(_, name)| format!("\"{name}\""))
                .collect::<Vec<_>>()
                .join(",");
            format!("SELECT {select_list} FROM {table} ORDER BY {order}")
        }
    };
    let statement = con.prepare(&format!("{base} LIMIT 0"))?;
    let mut names: Vec<String> = statement
        .column_names()
        .iter()
        .map(|s| s.to_string())
        .collect();
    drop(statement);
    if let Some(filter) = columns {
        let unknown = filter.iter().any(|name| !names.contains(name));
        let mut distinct = filter.to_vec();
        distinct.sort();
        distinct.dedup();
        if filter.is_empty() || unknown || distinct.len() != filter.len() {
            return Err(scan_error(
                "columns must be distinct names of the inspected table",
            ));
        }
        names = filter.to_vec();
    }
    let quoted: Vec<String> = names
        .iter()
        .map(|n| format!("\"{}\"", n.replace('"', "\"\"")))
        .collect();
    let size = quoted
        .iter()
        .map(|c| format!("COALESCE(length(CAST({c} AS BLOB)),0)"))
        .collect::<Vec<_>>()
        .join("+");
    let query = format!(
        "WITH selected AS ({base} LIMIT ?1 OFFSET ?2), sized AS (SELECT *,({size}) AS _row_bytes FROM selected) \
         SELECT _row_bytes,{} FROM sized",
        quoted
            .iter()
            .enumerate()
            .map(|(i, c)| format!("CASE WHEN _row_bytes<=?{} THEN {c} END", i + 3))
            .collect::<Vec<_>>()
            .join(",")
    );
    let total_rows: Option<i64> = if total {
        Some(con.query_row(&format!("SELECT COUNT(*) FROM ({base})"), [], |r| r.get(0))?)
    } else {
        None
    };
    let mut bindings: Vec<SqlValue> = vec![SqlValue::Integer(limit + 1), SqlValue::Integer(offset)];
    bindings.extend(names.iter().map(|_| SqlValue::Integer(max_bytes)));
    let mut statement = con.prepare(&query)?;
    let mut rows_iter = statement.query(params_from_iter(bindings.iter()))?;
    let mut rows: Vec<Value> = Vec::new();
    let mut used: usize = 0;
    let mut has_more = false;
    let mut truncated = false;
    let mut index: i64 = 0;
    while let Some(row) = rows_iter.next()? {
        if index == limit {
            has_more = true;
            break;
        }
        index += 1;
        let row_bytes: i64 = row.get(0)?;
        if row_bytes > max_bytes {
            has_more = true;
            truncated = true;
            break;
        }
        let mut item = Map::new();
        for (i, name) in names.iter().enumerate() {
            item.insert(name.clone(), cell_value(row.get_ref(i + 1)?));
        }
        let count = python_json(&Value::Object(item.clone())).len();
        if used + count > max_bytes as usize {
            has_more = true;
            truncated = true;
            break;
        }
        rows.push(Value::Object(item));
        used += count;
    }
    let mut out = Map::new();
    out.insert("table".into(), json!(table));
    out.insert("offset".into(), json!(offset));
    out.insert("total".into(), total_rows.map_or(Value::Null, |t| json!(t)));
    out.insert("columns".into(), json!(names));
    let next_offset = offset + rows.len() as i64;
    out.insert("rows".into(), Value::Array(rows));
    out.insert("bytes".into(), json!(used));
    out.insert("truncated".into(), json!(truncated));
    out.insert("has_more".into(), json!(has_more));
    out.insert("next_offset".into(), json!(next_offset));
    Ok(out)
}

/// Snapshot the scan and any audit.v2 companion as a validated pair.
pub fn snapshot_scan(path: &Path, destination: &Path) -> Result<String, StoreError> {
    let source = path.to_path_buf();
    regular(&source)?;
    let destination = if destination.is_dir() {
        let header = read_scan(&source)?;
        new_scan_path(
            destination,
            header["start_url"].as_str().unwrap_or(""),
            header["scan_uuid"].as_str().unwrap_or(""),
        )?
    } else {
        destination.to_path_buf()
    };
    let audit_source = audit_v2_path(&source);
    let audit_target = audit_v2_path(&destination);
    if std::fs::symlink_metadata(&audit_target).is_ok() {
        return Err(scan_error(format!(
            "snapshot audit companion already exists: {}",
            audit_target.display()
        )));
    }
    let has_audit_v2 = std::fs::symlink_metadata(&audit_source).is_ok();
    drop(metadata_connection(&source, false)?);
    let stamp_name = format!(
        ".audit-pair-snapshot-{}-{}.sqlite",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let parent = destination.parent().unwrap_or(Path::new("."));
    let staged_scan = parent.join(stamp_name);
    let staged_audit = audit_v2_path(&staged_scan);
    let mut published_audit = false;
    let outcome = (|| -> Result<String, StoreError> {
        let con = open_readonly(&source, Duration::from_secs(5), &[])?;
        con.execute(
            "VACUUM INTO ?1",
            rusqlite::params![staged_scan.display().to_string()],
        )?;
        drop(con);
        drop(metadata_connection(&staged_scan, false)?);
        if has_audit_v2 {
            let mut dest = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&staged_audit)?;
            let mut src = File::open(&audit_source)?;
            let mut buffer = vec![0u8; 1024 * 1024];
            loop {
                let read = src.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                dest.write_all(&buffer[..read])?;
            }
            dest.sync_all()?;
            std::fs::hard_link(&staged_audit, &audit_target)?;
            published_audit = true;
        }
        std::fs::hard_link(&staged_scan, &destination).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                scan_error(format!(
                    "snapshot target already exists: {}",
                    destination.display()
                ))
            } else {
                e.into()
            }
        })?;
        Ok(destination.display().to_string())
    })();
    let _ = std::fs::remove_file(&staged_scan);
    let _ = std::fs::remove_file(&staged_audit);
    if outcome.is_err() && published_audit && std::fs::symlink_metadata(&destination).is_err() {
        let _ = std::fs::remove_file(&audit_target);
    }
    outcome
}

/// Set the pinned flag of a scan under its writer lock.
pub fn pin_scan(path: &Path, pinned: bool) -> Result<(), StoreError> {
    let scan_path = path.to_path_buf();
    regular(&scan_path)?;
    let before = link_identity(&scan_path)?;
    let _lock = hold_writer_lock(&scan_path)?;
    drop(metadata_connection(&scan_path, false)?);
    regular(&scan_path)?;
    if !same_identity(before, link_identity(&scan_path)?) {
        return Err(scan_error("scan identity changed before pin update"));
    }
    let con = Connection::open(&scan_path)?;
    con.busy_timeout(Duration::from_secs(5))?;
    con.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA synchronous=FULL;")?;
    let mode: String = con.query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0))?;
    if mode.to_lowercase() != "delete" {
        return Err(scan_error(
            "pin requires a checkpointed DELETE-journal artifact",
        ));
    }
    con.execute_batch("BEGIN IMMEDIATE")?;
    let updated = con.execute(
        "UPDATE scan SET pinned=?1 WHERE singleton=1",
        rusqlite::params![i64::from(pinned)],
    );
    match updated {
        Ok(_) => {
            con.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(error) => {
            let _ = con.execute_batch("ROLLBACK");
            Err(error.into())
        }
    }
}

/// Retention parameters: 0..=36500 days and 0..=10000 newest scans.
fn check_policy(older_than_days: i64, keep_newest: i64) -> Result<(), StoreError> {
    if !(0..=36500).contains(&older_than_days) || !(0..=10000).contains(&keep_newest) {
        return Err(scan_error(
            "retention needs 0..36500 days and 0..10000 newest scans",
        ));
    }
    Ok(())
}

/// Scans older than the cutoff that may be removed, keeping the newest `keep_newest` per
/// (host, config fingerprint) group. Only finished, unpinned, complete scans qualify.
pub fn eligible(
    items: &[Map<String, Value>],
    older_than_days: i64,
    keep_newest: i64,
) -> Result<Vec<Map<String, Value>>, StoreError> {
    check_policy(older_than_days, keep_newest)?;
    let now_micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0);
    let cutoff = now_micros - older_than_days * 86_400 * 1_000_000;
    let mut groups: std::collections::BTreeMap<(String, String), Vec<&Map<String, Value>>> =
        std::collections::BTreeMap::new();
    for item in items {
        let key = (
            item["host"].as_str().unwrap_or("").to_string(),
            item["config_fingerprint"]
                .as_str()
                .unwrap_or("")
                .to_string(),
        );
        groups.entry(key).or_default().push(item);
    }
    let mut candidates = Vec::new();
    for mut group in groups.into_values() {
        group.sort_by(|a, b| {
            let key = |m: &Map<String, Value>| {
                let time = m["finished_at"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .or(m["created_at"].as_str());
                (
                    finished_at(time).unwrap_or(i64::MIN),
                    m["uuid"].as_str().unwrap_or("").to_string(),
                )
            };
            key(b).cmp(&key(a))
        });
        for item in group.into_iter().skip(keep_newest as usize) {
            let finished = item["finished_at"].as_str().filter(|s| !s.is_empty());
            let old_enough = match finished {
                Some(text) => finished_at(Some(text))? <= cutoff,
                None => false,
            };
            let disk = item["disk_bytes"].as_i64().unwrap_or(-1);
            let expected_disk =
                item["bytes"].as_i64().unwrap_or(0) + item["audit_bytes"].as_i64().unwrap_or(0);
            if item["lifecycle"].as_str() == Some("finished")
                && finished.is_some()
                && item["pinned"].as_bool() == Some(false)
                && item["crawl_partial"].as_bool() == Some(false)
                && item["corpus_partial"].as_bool() == Some(false)
                && disk == expected_disk
                && old_enough
            {
                candidates.push(item.clone());
            }
        }
    }
    candidates.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    Ok(candidates)
}

/// Stable SHA-256 over a plan without its `digest` field, using sorted-key JSON.
pub fn digest(plan: &Map<String, Value>) -> String {
    let body: Map<String, Value> = plan
        .iter()
        .filter(|(k, _)| k.as_str() != "digest")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    format!(
        "{:x}",
        Sha256::digest(python_json(&Value::Object(body)).as_bytes())
    )
}

/// Preview of retention candidates; the returned plan carries its own digest.
pub fn prune_preview(
    directory_path: &Path,
    older_than_days: i64,
    keep_newest: i64,
) -> Result<Map<String, Value>, StoreError> {
    check_policy(older_than_days, keep_newest)?;
    let root = directory(directory_path)?;
    let info = path_identity(&root)?;
    let (items, errors) = catalog(&root)?;
    let mut candidates = Vec::new();
    for item in eligible(&items, older_than_days, keep_newest)? {
        if !lock_is_active(Path::new(item["path"].as_str().unwrap_or("")))? {
            candidates.push(item);
        }
    }
    let mut payload = Map::new();
    payload.insert("directory".into(), json!(root.display().to_string()));
    payload.insert(
        "directory_inode".into(),
        json!({"device": info.0, "inode": info.1}),
    );
    payload.insert("older_than_days".into(), json!(older_than_days));
    payload.insert("keep_newest".into(), json!(keep_newest));
    payload.insert(
        "candidates".into(),
        Value::Array(candidates.into_iter().map(Value::Object).collect()),
    );
    payload.insert(
        "errors".into(),
        Value::Array(errors.into_iter().map(Value::Object).collect()),
    );
    payload.insert(
        "warning".into(),
        json!("Deleting a scan can remove a useful comparison baseline."),
    );
    let digest_value = digest(&payload);
    payload.insert("digest".into(), json!(digest_value));
    Ok(payload)
}

/// Delete the reviewed candidates of a preview plan, re-checking identity and eligibility.
/// Returns the removed scan paths.
pub fn prune_apply(
    directory_path: &Path,
    plan: &Map<String, Value>,
) -> Result<Vec<String>, StoreError> {
    let root = directory(directory_path)?;
    let expected_keys = [
        "directory",
        "directory_inode",
        "older_than_days",
        "keep_newest",
        "candidates",
        "errors",
        "warning",
        "digest",
    ];
    let mut keys: Vec<&str> = plan.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let mut expected_sorted = expected_keys.to_vec();
    expected_sorted.sort_unstable();
    if keys != expected_sorted
        || digest(plan) != plan["digest"].as_str().unwrap_or("")
        || root.display().to_string() != plan["directory"].as_str().unwrap_or("")
    {
        return Err(scan_error(
            "prune plan is stale or belongs to another directory",
        ));
    }
    let state = path_identity(&root)?;
    if plan["directory_inode"] != json!({"device": state.0, "inode": state.1}) {
        return Err(scan_error("prune directory identity changed"));
    }
    let older = plan["older_than_days"].as_i64().unwrap_or(-1);
    let keep = plan["keep_newest"].as_i64().unwrap_or(-1);
    check_policy(older, keep)?;
    let candidates = plan["candidates"]
        .as_array()
        .filter(|c| c.len() <= 10000)
        .ok_or_else(|| scan_error("invalid prune selection"))?;
    let mut held: Vec<File> = Vec::new();
    let mut validated: Vec<(PathBuf, Map<String, Value>)> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let outcome = (|| -> Result<Vec<String>, StoreError> {
        for expected in candidates {
            let expected = expected
                .as_object()
                .ok_or_else(|| scan_error("invalid prune selection"))?;
            let path = PathBuf::from(expected["path"].as_str().unwrap_or(""));
            if path.parent() != Some(root.as_path()) || !seen.insert(path.clone()) {
                return Err(scan_error(
                    "prune candidate escapes its directory or is duplicated",
                ));
            }
            regular(&path)?;
            let info = link_identity(&path)?;
            held.push(hold_writer_lock(&path)?);
            let audit_path = audit_v2_path(&path);
            if !expected["audit_inode"].is_null() {
                held.push(hold_writer_lock(&audit_path)?);
                let audit_info = link_identity(&audit_path)?;
                if json!({"device": audit_info.0, "inode": audit_info.1}) != expected["audit_inode"]
                {
                    return Err(scan_error("prune audit companion identity changed"));
                }
            } else if std::fs::symlink_metadata(&audit_path).is_ok() {
                return Err(scan_error("prune candidate gained an audit companion"));
            }
            if json!({"device": info.0, "inode": info.1}) != expected["inode"] {
                return Err(scan_error("prune candidate identity changed"));
            }
            let current = metadata(&path)?;
            if Value::Object(current.clone()) != Value::Object(expected.clone()) {
                return Err(scan_error("prune candidate metadata changed"));
            }
            validated.push((path, current));
        }
        let (items, _) = catalog(&root)?;
        let eligible_paths: std::collections::BTreeSet<String> = eligible(&items, older, keep)?
            .into_iter()
            .filter_map(|i| i["path"].as_str().map(str::to_owned))
            .collect();
        if validated
            .iter()
            .any(|(path, _)| !eligible_paths.contains(&path.display().to_string()))
        {
            return Err(scan_error(
                "prune selection is no longer eligible under its retention policy",
            ));
        }
        for (path, expected) in &validated {
            if Value::Object(metadata(path)?) != Value::Object(expected.clone()) {
                return Err(scan_error("prune candidate changed before deletion"));
            }
        }
        let mut removed = Vec::new();
        for (path, expected) in &validated {
            let audit_path = audit_v2_path(path);
            let mut hold_path: Option<PathBuf> = None;
            if !expected["audit_inode"].is_null() {
                let name = format!(
                    ".{}.prune-hold-{}",
                    audit_path.file_name().unwrap_or_default().to_string_lossy(),
                    uuid_like()
                );
                let hold = root.join(name);
                std::fs::hard_link(&audit_path, &hold)?;
                if let Err(exc) = std::fs::remove_file(&audit_path) {
                    let _ = std::fs::remove_file(&hold);
                    return Err(exc.into());
                }
                hold_path = Some(hold);
            }
            if let Err(exc) = std::fs::remove_file(path) {
                if let Some(hold) = &hold_path {
                    let _ = std::fs::hard_link(hold, &audit_path);
                    let _ = std::fs::remove_file(hold);
                }
                return Err(exc.into());
            }
            if let Some(hold) = hold_path {
                std::fs::remove_file(&hold).map_err(|e| {
                    scan_error(format!(
                        "scan was pruned but its audit companion recovery copy remains at {}: {e}",
                        hold.display()
                    ))
                })?;
            }
            removed.push(path.display().to_string());
        }
        if !removed.is_empty() {
            File::open(&root)?.sync_all()?;
        }
        Ok(removed)
    })();
    drop(held);
    outcome
}

/// Process-unique token for temporary hold names.
fn uuid_like() -> String {
    format!(
        "{:x}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        std::process::id()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recommended_name_matches_reference_shape() {
        // 2026-10-27T12:34:56Z
        let epoch = days_from_civil(2026, 10, 27) * 86_400 + 12 * 3600 + 34 * 60 + 56;
        let name = recommended_name(
            "https://www.Example.com:8443/path",
            "0123456789ab4cde8f0123456789abcd",
            Some(epoch),
        )
        .expect("name");
        assert_eq!(name, "20261027T123456Z_www.example.com_01234567.sqlite");
    }

    #[test]
    fn stamp_round_trips_civil_dates() {
        for (y, m, d) in [(1970, 1, 1), (2000, 2, 29), (2026, 10, 27)] {
            assert_eq!(civil_from_days(days_from_civil(y, m, d)), (y, m, d));
        }
    }

    #[test]
    fn finished_at_requires_offset_and_orders_utc() {
        let a = finished_at(Some("2026-10-27T12:00:00+03:00")).expect("a");
        let b = finished_at(Some("2026-10-27T09:00:00Z")).expect("b");
        assert_eq!(a, b);
        assert!(finished_at(Some("2026-10-27T12:00:00")).is_err());
        assert_eq!(finished_at(None).expect("none"), i64::MIN);
    }

    #[test]
    fn pagination_bounds() {
        assert!(pagination(0, 1).is_ok());
        assert!(pagination(0, 1000).is_ok());
        assert!(pagination(-1, 1).is_err());
        assert!(pagination(0, 1001).is_err());
    }

    #[test]
    fn lock_path_appends_suffix() {
        assert_eq!(
            lock_path(Path::new("/x/scan.sqlite")),
            PathBuf::from("/x/scan.sqlite.writer.lock")
        );
    }
}

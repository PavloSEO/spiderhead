//! Versioned, ordered SQLite storage for audits too large for the scan.v1 JSON slot.
//!
//! The writer lives here; the validating reader is in `audit_v2/reader.rs`.
//! Both render JSON byte for byte as the stored format requires (`audit_v2/pyjson.rs`).

mod pyjson;
mod reader;

pub(crate) use pyjson::float_repr;
pub use reader::{AuditGroupMembers, AuditV2Reader};

use crate::error::StoreError;
use crate::scan_artifact::open_scan_mode;
use indexmap::IndexMap;
use pyjson::dumps;
use rusqlite::{params, Connection};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// SQLite `application_id` ("AUDV").
pub const APPLICATION_ID: i64 = 1096107094;

/// Current storage revision (`PRAGMA user_version`); revision 2 is still readable.
pub const USER_VERSION: i64 = 3;

/// Format name stored in `audit_meta`.
pub const FORMAT_VERSION: &str = "audit.v2";

/// Header key replacing a collection array.
pub const COLLECTION_MARKER: &str = "$audit_v2_collection";

/// Group row key replacing its `urls` member array.
pub const GROUP_MEMBERS_MARKER: &str = "$audit_v2_group_members";

/// Largest stored header (64 MiB).
pub const MAX_HEADER_BYTES: i64 = 67108864;

/// Largest stored row or group member (8 MiB).
pub const MAX_ITEM_BYTES: i64 = 8388608;

/// DDL statements, in creation order.
pub const SCHEMA: &[&str] = &[
    r"CREATE TABLE audit_meta (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    format_version TEXT NOT NULL CHECK (format_version = 'audit.v2'),
    binding_json TEXT NOT NULL,
    header_json TEXT NOT NULL,
    sha256 TEXT NOT NULL
)",
    r"CREATE TABLE collections (
    pointer TEXT PRIMARY KEY,
    item_count INTEGER NOT NULL CHECK (item_count >= 0)
)",
    r"CREATE TABLE items (
    pointer TEXT NOT NULL REFERENCES collections(pointer),
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    value_json TEXT NOT NULL,
    PRIMARY KEY (pointer, ordinal)
)",
    r"CREATE TABLE group_members (
    pointer TEXT NOT NULL CHECK (pointer = '/groups'),
    group_ordinal INTEGER NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    value_json TEXT NOT NULL,
    PRIMARY KEY (group_ordinal, ordinal),
    FOREIGN KEY (pointer, group_ordinal) REFERENCES items(pointer, ordinal)
)",
];

/// `(type, name, sql)` rows expected in `sqlite_master` for revision 3, sorted.
pub const EXPECTED_SCHEMA: &[&[&str]] = &[
    &[
        "table",
        "audit_meta",
        r"CREATE TABLE audit_meta (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    format_version TEXT NOT NULL CHECK (format_version = 'audit.v2'),
    binding_json TEXT NOT NULL,
    header_json TEXT NOT NULL,
    sha256 TEXT NOT NULL
)",
    ],
    &[
        "table",
        "collections",
        r"CREATE TABLE collections (
    pointer TEXT PRIMARY KEY,
    item_count INTEGER NOT NULL CHECK (item_count >= 0)
)",
    ],
    &[
        "table",
        "group_members",
        r"CREATE TABLE group_members (
    pointer TEXT NOT NULL CHECK (pointer = '/groups'),
    group_ordinal INTEGER NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    value_json TEXT NOT NULL,
    PRIMARY KEY (group_ordinal, ordinal),
    FOREIGN KEY (pointer, group_ordinal) REFERENCES items(pointer, ordinal)
)",
    ],
    &[
        "table",
        "items",
        r"CREATE TABLE items (
    pointer TEXT NOT NULL REFERENCES collections(pointer),
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    value_json TEXT NOT NULL,
    PRIMARY KEY (pointer, ordinal)
)",
    ],
];

/// Same for revision 2 (no `group_members`).
pub const V2_SCHEMA: &[&[&str]] = &[
    &[
        "table",
        "audit_meta",
        r"CREATE TABLE audit_meta (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    format_version TEXT NOT NULL CHECK (format_version = 'audit.v2'),
    binding_json TEXT NOT NULL,
    header_json TEXT NOT NULL,
    sha256 TEXT NOT NULL
)",
    ],
    &[
        "table",
        "collections",
        r"CREATE TABLE collections (
    pointer TEXT PRIMARY KEY,
    item_count INTEGER NOT NULL CHECK (item_count >= 0)
)",
    ],
    &[
        "table",
        "items",
        r"CREATE TABLE items (
    pointer TEXT NOT NULL REFERENCES collections(pointer),
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    value_json TEXT NOT NULL,
    PRIMARY KEY (pointer, ordinal)
)",
    ],
];

/// An audit.v2 companion is invalid, inconsistent, or exceeds an explicit boundary.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct AuditV2Error {
    /// Human-readable reason.
    pub message: String,
}

/// Integrity validation or streaming exceeded the explicit read budget.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct AuditReadTimeout {
    /// Human-readable reason.
    pub message: String,
}

/// Build the `AuditV2Error` variant of [`StoreError`].
pub(crate) fn fail(message: impl Into<String>) -> StoreError {
    StoreError::AuditV2Error(AuditV2Error {
        message: message.into(),
    })
}

/// Return the deterministic companion path for a saved scan.
pub fn audit_v2_path(scan_path: &Path) -> PathBuf {
    let mut name = scan_path.file_name().unwrap_or_default().to_os_string();
    name.push(".audit-v2.sqlite");
    scan_path.with_file_name(name)
}

/// Canonical JSON: sorted keys, compact separators, non-ASCII kept.
pub fn canonical(value: &Value) -> Result<String, StoreError> {
    Ok(dumps(value, true))
}

/// Split an absolute JSON pointer into unescaped tokens.
pub fn pointer_parts(pointer: &str) -> Result<Vec<String>, StoreError> {
    let Some(rest) = pointer.strip_prefix('/') else {
        return Err(fail(
            "audit.v2 collection keys must be absolute JSON pointers",
        ));
    };
    let mut parts = Vec::new();
    for token in rest.split('/') {
        let bytes = token.as_bytes();
        let bad_escape = bytes
            .iter()
            .enumerate()
            .any(|(i, b)| *b == b'~' && !matches!(bytes.get(i + 1), Some(b'0' | b'1')));
        if bad_escape {
            return Err(fail("audit.v2 collection pointer has invalid escaping"));
        }
        parts.push(token.replace("~1", "/").replace("~0", "~"));
    }
    Ok(parts)
}

/// List index text: optional sign, digits with single underscores, blanks around.
fn py_index(token: &str) -> Option<i64> {
    let t = token.trim_matches(crate::bodies::py_isspace);
    let (neg, digits) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let valid = !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit() || c == '_')
        && !digits.starts_with('_')
        && !digits.ends_with('_')
        && !digits.contains("__");
    let n: i64 = valid.then(|| digits.replace('_', "").parse().ok())??;
    Some(if neg { -n } else { n })
}

fn list_slot(len: usize, token: &str) -> Option<usize> {
    let i = py_index(token)?;
    let i = if i < 0 { i + len as i64 } else { i };
    (0..len as i64).contains(&i).then_some(i as usize)
}

fn missing(pointer: &str) -> StoreError {
    fail(format!(
        "audit.v2 collection path does not exist: {pointer}"
    ))
}

/// Read-only walk to the value named by `pointer`.
pub fn get_pointer<'a>(root: &'a Value, pointer: &str) -> Result<&'a Value, StoreError> {
    let mut node = root;
    for part in pointer_parts(pointer)? {
        node = match node {
            Value::Array(items) => list_slot(items.len(), &part).map(|i| &items[i]),
            Value::Object(map) => map.get(&part),
            _ => None,
        }
        .ok_or_else(|| missing(pointer))?;
    }
    Ok(node)
}

/// Replace (or, for objects, create) the member named by `pointer`.
pub fn set_pointer(root: &mut Value, pointer: &str, value: Value) -> Result<(), StoreError> {
    let parts = pointer_parts(pointer)?;
    let (last, walk) = parts.split_last().ok_or_else(|| missing(pointer))?;
    let mut node = root;
    for part in walk {
        node = match node {
            Value::Array(items) => list_slot(items.len(), part).map(|i| &mut items[i]),
            Value::Object(map) => map.get_mut(part),
            _ => None,
        }
        .ok_or_else(|| missing(pointer))?;
    }
    match node {
        Value::Array(items) => {
            let i = list_slot(items.len(), last).ok_or_else(|| missing(pointer))?;
            items[i] = value;
        }
        Value::Object(map) => {
            map.insert(last.clone(), value);
        }
        _ => return Err(missing(pointer)),
    }
    Ok(())
}

/// Pointers named by collection markers anywhere in `value`.
pub fn collection_markers(value: &Value) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    match value {
        Value::Object(map) => {
            let marker_only = map.len() == 1 && map.contains_key(COLLECTION_MARKER);
            let counted =
                map.len() == 2 && map.contains_key(COLLECTION_MARKER) && map.contains_key("count");
            if marker_only || counted {
                if let Some(Value::String(p)) = map.get(COLLECTION_MARKER) {
                    found.insert(p.clone());
                }
            }
            for child in map.values() {
                found.extend(collection_markers(child));
            }
        }
        Value::Array(items) => items
            .iter()
            .for_each(|c| found.extend(collection_markers(c))),
        _ => {}
    }
    found
}

/// Whether any collection marker occurs in `value`.
pub fn contains_marker(value: &Value) -> bool {
    !collection_markers(value).is_empty()
}

/// Whether the reserved group-member marker key occurs anywhere in `value`.
pub fn contains_group_marker(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.contains_key(GROUP_MEMBERS_MARKER) || map.values().any(contains_group_marker)
        }
        Value::Array(items) => items.iter().any(contains_group_marker),
        _ => false,
    }
}

/// Check `binding` against the scan header and return the scan's own identity.
pub fn bind_to_scan(
    scan_path: &Path,
    binding: &Map<String, Value>,
    light: bool,
) -> Result<Map<String, Value>, StoreError> {
    const KEYS: [&str; 4] = [
        "scan_uuid",
        "evidence_revision",
        "analyzer_version",
        "analyzer_revision",
    ];
    if binding.len() != KEYS.len() || !KEYS.iter().all(|k| binding.contains_key(*k)) {
        return Err(fail(
            "audit.v2 binding must name scan UUID, evidence revision and analyzer identity",
        ));
    }
    let cannot =
        |e: &dyn std::fmt::Display| fail(format!("cannot validate audit.v2 scan binding: {e}"));
    let con = open_scan_mode(scan_path, false, None, light)
        .map_err(|e| cannot(&e))?
        .0;
    let actual: Map<String, Value> = con
        .query_row(
            "SELECT scan_uuid, evidence_revision, writer_version, writer_revision FROM scan WHERE singleton=1",
            [],
            |r| {
                let text = |i| r.get::<_, rusqlite::types::Value>(i).map(sql_json);
                let mut m = Map::new();
                m.insert("scan_uuid".into(), text(0)?);
                m.insert("evidence_revision".into(), text(1)?);
                m.insert("analyzer_version".into(), text(2)?);
                m.insert("analyzer_revision".into(), text(3)?);
                Ok(m)
            },
        )
        .map_err(|e| cannot(&e))?;
    if canonical(&Value::Object(binding.clone()))? != canonical(&Value::Object(actual.clone()))? {
        return Err(fail(
            "audit.v2 binding disagrees with its native scan identity",
        ));
    }
    Ok(actual)
}

fn sql_json(v: rusqlite::types::Value) -> Value {
    use rusqlite::types::Value as S;
    match v {
        S::Null => Value::Null,
        S::Integer(i) => Value::from(i),
        S::Real(f) => Value::from(f),
        S::Text(t) => Value::from(t),
        S::Blob(_) => Value::Null,
    }
}

fn put_len_prefixed(digest: &mut Sha256, text: &str) {
    digest.update((text.len() as u64).to_be_bytes());
    digest.update(text.as_bytes());
}

/// Start the content hash over format identity, binding and header.
pub fn digest_start(binding_json: &str, header_json: &str, version: i64) -> Sha256 {
    let mut digest = Sha256::new();
    let identity = if version == 2 {
        FORMAT_VERSION.to_owned()
    } else {
        format!("{FORMAT_VERSION}:storage-{version}")
    };
    for text in [identity.as_str(), binding_json, header_json] {
        put_len_prefixed(&mut digest, text);
    }
    digest
}

/// Feed one row (pointer, ordinal, stored JSON text) into the content hash.
pub fn digest_item(digest: &mut Sha256, pointer: &str, ordinal: i64, value_json: &str) {
    for text in [pointer, ordinal.to_string().as_str(), value_json] {
        put_len_prefixed(digest, text);
    }
}

fn too_big(text: &str) -> bool {
    text.len() as i64 > MAX_ITEM_BYTES
}

/// Insert one `/groups` row, moving its `urls` array into `group_members`.
fn store_group_row(
    con: &Connection,
    row: &mut Map<String, Value>,
    group_ordinal: i64,
) -> Result<(), StoreError> {
    let Some(Value::Array(urls)) = row.get("urls") else {
        return Ok(());
    };
    let urls = urls.clone();
    // The urls key is assigned in place, so it keeps its position in the row.
    let slot = row.get_mut("urls");
    for (ordinal, member) in urls.iter().enumerate() {
        if contains_group_marker(member) {
            return Err(fail("audit.v2 input uses a reserved group marker"));
        }
        let member_json = canonical(member)?;
        if too_big(&member_json) {
            return Err(fail("audit.v2 group member exceeds 8 MiB"));
        }
        con.execute(
            "INSERT INTO group_members VALUES('/groups',?,?,?)",
            params![group_ordinal, ordinal as i64, member_json],
        )?;
    }
    let mut marker = Map::new();
    marker.insert(GROUP_MEMBERS_MARKER.into(), Value::from(group_ordinal));
    marker.insert("count".into(), Value::from(urls.len() as i64));
    if let Some(slot) = slot {
        *slot = Value::Object(marker);
    }
    Ok(())
}

/// Atomically write a versioned companion from ordered JSON-pointer collections.
///
/// Each collection path must identify a list in `header`. The list is replaced by a
/// count-bearing marker in the stored header; rows remain in ordinal order. `collections` keeps
/// the caller's order (an `IndexMap`).
pub fn write_audit_v2(
    scan_path: &Path,
    header: &Map<String, Value>,
    collections: &IndexMap<String, Vec<Value>>,
    binding: &Map<String, Value>,
) -> Result<PathBuf, StoreError> {
    let path = std::path::absolute(scan_path)?;
    let companion = audit_v2_path(&path);
    let meta = fs::symlink_metadata(&path);
    if !meta.as_ref().is_ok_and(|m| m.is_file()) {
        return Err(fail("audit.v2 requires an existing regular native scan"));
    }
    let validated_binding = bind_to_scan(&path, binding, false)?;
    if collections.is_empty() {
        return Err(fail(
            "audit.v2 needs an object header and at least one collection",
        ));
    }
    let mut stored_header: Value =
        serde_json::from_str(&canonical(&Value::Object(header.clone()))?)?;
    if contains_marker(&stored_header) || contains_group_marker(&stored_header) {
        return Err(fail("audit.v2 header uses a reserved collection marker"));
    }
    for pointer in collections.keys() {
        pointer_parts(pointer)?;
        if !get_pointer(&stored_header, pointer)?.is_array() {
            return Err(fail(format!(
                "audit.v2 collection path must point to an array: {pointer}"
            )));
        }
        let mut marker = Map::new();
        marker.insert(COLLECTION_MARKER.into(), Value::from(pointer.clone()));
        set_pointer(&mut stored_header, pointer, Value::Object(marker))?;
    }
    let binding_json = canonical(&Value::Object(validated_binding))?;
    if canonical(&stored_header)?.len() as i64 > MAX_HEADER_BYTES {
        return Err(fail("audit.v2 header exceeds its explicit 64 MiB limit"));
    }

    let mut lock_name = companion.file_name().unwrap_or_default().to_os_string();
    lock_name.push(".writer.lock");
    let lock_path = companion.with_file_name(lock_name);
    if fs::symlink_metadata(&lock_path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(fail(
            "audit.v2 companion already has a writer: writer lock path must not be a symlink",
        ));
    }
    let lock = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&lock_path)?;
    lock.try_lock()
        .map_err(|e| fail(format!("audit.v2 companion already has a writer: {e}")))?;
    if fs::symlink_metadata(&companion).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(fail("audit.v2 companion path must not be a symlink"));
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(companion.file_name().unwrap_or_default());
    tmp_name.push(format!(".{}-{nanos}.tmp", std::process::id()));
    let temporary = companion.with_file_name(tmp_name);
    let result = build_companion(&temporary, &mut stored_header, collections, &binding_json)
        .and_then(|()| {
            fs::OpenOptions::new()
                .write(true)
                .open(&temporary)?
                .sync_all()?;
            fs::rename(&temporary, &companion)?;
            if let Some(dir) = companion.parent() {
                fs::File::open(dir)?.sync_all()?;
            }
            Ok(())
        });
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    let _ = lock.unlock();
    result.map(|()| companion)
}

/// Create the SQLite file at `temporary` with all rows, the final header and the content hash.
fn build_companion(
    temporary: &Path,
    stored_header: &mut Value,
    collections: &IndexMap<String, Vec<Value>>,
    binding_json: &str,
) -> Result<(), StoreError> {
    let con = Connection::open(temporary)?;
    con.execute_batch(&format!(
        "PRAGMA trusted_schema=OFF; PRAGMA foreign_keys=OFF; PRAGMA synchronous=FULL; PRAGMA journal_mode=DELETE; \
         PRAGMA application_id={APPLICATION_ID}; PRAGMA user_version={USER_VERSION};"
    ))?;
    for statement in SCHEMA {
        con.execute_batch(statement)?;
    }
    con.execute_batch("BEGIN IMMEDIATE")?;
    let outcome = fill_companion(&con, stored_header, collections, binding_json);
    match outcome {
        Ok(()) => {
            con.execute_batch("COMMIT")?;
            let quick: String = con.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
            if quick != "ok" {
                return Err(fail("audit.v2 companion failed SQLite quick_check"));
            }
            Ok(())
        }
        Err(e) => {
            let _ = con.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

fn fill_companion(
    con: &Connection,
    stored_header: &mut Value,
    collections: &IndexMap<String, Vec<Value>>,
    binding_json: &str,
) -> Result<(), StoreError> {
    let mut counts: Vec<(&String, i64)> = Vec::new();
    for (pointer, rows) in collections {
        con.execute(
            "INSERT INTO collections(pointer,item_count) VALUES (?,0)",
            [pointer],
        )?;
        let mut count: i64 = 0;
        for source in rows {
            if contains_group_marker(source) {
                return Err(fail("audit.v2 input uses a reserved group marker"));
            }
            let mut row = source.clone();
            if pointer == "/groups" {
                if let Value::Object(map) = &mut row {
                    store_group_row(con, map, count)?;
                }
            }
            let raw = dumps(&row, false);
            if too_big(&raw) {
                return Err(fail(format!(
                    "audit.v2 row exceeds 8 MiB at {pointer}[{count}]"
                )));
            }
            con.execute(
                "INSERT INTO items(pointer,ordinal,value_json) VALUES (?,?,?)",
                params![pointer, count, raw],
            )?;
            count += 1;
        }
        con.execute(
            "UPDATE collections SET item_count=? WHERE pointer=?",
            params![count, pointer],
        )?;
        counts.push((pointer, count));
    }
    for (pointer, count) in &counts {
        let mut marker = Map::new();
        marker.insert(COLLECTION_MARKER.into(), Value::from((*pointer).clone()));
        marker.insert("count".into(), Value::from(*count));
        set_pointer(stored_header, pointer, Value::Object(marker))?;
    }
    let header_json = canonical(stored_header)?;
    if header_json.len() as i64 > MAX_HEADER_BYTES {
        return Err(fail("audit.v2 header exceeds its explicit 64 MiB limit"));
    }
    let mut digest = digest_start(binding_json, &header_json, USER_VERSION);
    let mut sorted: Vec<&String> = collections.keys().collect();
    sorted.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    for pointer in sorted {
        let mut stmt =
            con.prepare("SELECT ordinal,value_json FROM items WHERE pointer=? ORDER BY ordinal")?;
        let mut rows = stmt.query([pointer])?;
        while let Some(r) = rows.next()? {
            digest_item(&mut digest, pointer, r.get(0)?, &r.get::<_, String>(1)?);
        }
    }
    let mut stmt = con.prepare(
        "SELECT group_ordinal,ordinal,value_json FROM group_members ORDER BY group_ordinal,ordinal",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        let group: i64 = r.get(0)?;
        digest_item(
            &mut digest,
            &format!("@group-members/{group}"),
            r.get(1)?,
            &r.get::<_, String>(2)?,
        );
    }
    let sha: String = digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    con.execute(
        "INSERT INTO audit_meta VALUES (1,?,?,?,?)",
        params![FORMAT_VERSION, binding_json, header_json, sha],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;

//! LRU-cached retained-scan evidence reads and revision hashing

use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use indexmap::IndexMap;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::error::{io, value, ProjectsResult};
use crate::pyjson;

/// Bytes retained from the end of a scan log page.
pub const LOG_BYTES: u64 = 32 * 1024;
const EVIDENCE_CACHE_LIMIT: usize = 32;
const FINDINGS_CACHE_LIMIT: usize = 8;
const FINDINGS_CACHE_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Hashable cache key: every component is canonically encoded so heterogeneous
/// values (paths, stat states, JSON scalars) compare by their encoded form.
pub type CacheKey = Vec<String>;

type Cache = IndexMap<CacheKey, Value>;

static EVIDENCE_CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(|| Mutex::new(IndexMap::new()));
static FINDINGS_CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(|| Mutex::new(IndexMap::new()));

/// Shared evidence cache handle.
pub fn evidence_cache() -> &'static Mutex<Cache> {
    &EVIDENCE_CACHE
}

/// Shared findings cache handle.
pub fn findings_cache() -> &'static Mutex<Cache> {
    &FINDINGS_CACHE
}

/// Maximum evidence cache entries before LRU eviction.
pub fn evidence_cache_limit() -> usize {
    EVIDENCE_CACHE_LIMIT
}

/// Maximum findings cache entries before LRU eviction.
pub fn findings_cache_limit() -> usize {
    FINDINGS_CACHE_LIMIT
}

/// Byte budget for the findings cache (8 MiB).
pub fn findings_cache_max_bytes() -> usize {
    FINDINGS_CACHE_MAX_BYTES
}

fn volatile_key(key: &str) -> bool {
    matches!(
        key,
        "observed_at" | "age_seconds" | "current_rate_per_second"
    )
}

/// Tail of `<root>/log.md`, bounded to `LOG_BYTES`.
pub fn log(root: &Path) -> ProjectsResult<Value> {
    use std::io::{Read, Seek, SeekFrom};
    let path = root.join("log.md");
    let size = io(std::fs::metadata(&path))?.len();
    let mut file = io(std::fs::File::open(&path))?;
    io(file.seek(SeekFrom::Start(size.saturating_sub(LOG_BYTES))))?;
    let mut bytes = Vec::new();
    io(file.take(LOG_BYTES).read_to_end(&mut bytes))?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Ok(serde_json::json!({
        "text": text,
        "truncated": size > LOG_BYTES,
        "bytes": size,
    }))
}

/// Recursively drop volatile revision keys.
pub fn stable_projection(item: &Value) -> Value {
    match item {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(key, _)| !volatile_key(key))
                .map(|(key, value)| (key.clone(), stable_projection(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(stable_projection).collect()),
        _ => item.clone(),
    }
}

/// SHA-256 revision envelope answering `not_modified` on a match.
pub fn revisioned(payload: &Value, since_revision: Option<&str>) -> ProjectsResult<Value> {
    if let Some(token) = since_revision {
        let valid = token.len() == 64
            && token
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if !valid {
            return Err(value(
                "since_revision must be a 64-character lowercase SHA-256 token",
            ));
        }
    }
    let stable = stable_projection(payload);
    let encoded = pyjson::dumps_compact(&stable);
    let revision = format!("{:x}", Sha256::digest(encoded.as_bytes()));
    if since_revision == Some(revision.as_str()) {
        return Ok(serde_json::json!({
            "ok": true,
            "revision": revision,
            "not_modified": true,
        }));
    }
    let mut result = match payload {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };
    result.insert("revision".into(), Value::String(revision));
    result.insert("not_modified".into(), Value::Bool(false));
    Ok(Value::Object(result))
}

/// `(st_dev, st_ino, st_mtime_ns, st_size)` or `None` when the path is gone
pub fn stat_state(path: &Path) -> Option<(u64, u64, i64, i64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let info = std::fs::metadata(path).ok()?;
        Some((
            info.dev(),
            info.ino(),
            info.mtime_nsec(),
            info.size() as i64,
        ))
    }
    #[cfg(not(unix))]
    {
        let info = std::fs::metadata(path).ok()?;
        let mtime = info
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        Some((0, 0, mtime, info.len() as i64))
    }
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn encode_json(item: &Value) -> String {
    pyjson::dumps_compact(item)
}

fn encode_stat(state: Option<(u64, u64, i64, i64)>) -> String {
    match state {
        Some((dev, ino, mtime, size)) => format!("[{dev},{ino},{mtime},{size}]"),
        None => "null".to_string(),
    }
}

/// Identity key for one evidence projection: artifact bytes (including WAL/SHM
/// and the audit companion) plus retained scan configuration.
pub fn evidence_cache_key(row: &Map<String, Value>) -> ProjectsResult<CacheKey> {
    let path = row
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| value("retained scan row has no path"))?;
    let source = PathBuf::from(path);
    let resolved = canonical(&source);
    let wal = resolved.with_file_name(format!(
        "{}-wal",
        resolved
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));
    let shm = resolved.with_file_name(format!(
        "{}-shm",
        resolved
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));
    let audit = spiderhead_store::audit_v2::audit_v2_path(&resolved);
    let fields = [
        "uuid",
        "lifecycle",
        "finish_reason",
        "crawl_partial",
        "corpus_partial",
        "evidence_revision",
        "config_fingerprint",
        "format_version",
        "source_kind",
    ];
    let mut key = vec![
        resolved.to_string_lossy().into_owned(),
        encode_stat(stat_state(&source)),
        encode_stat(stat_state(&wal)),
        encode_stat(stat_state(&shm)),
        encode_stat(stat_state(&audit)),
    ];
    key.extend(
        fields
            .iter()
            .map(|field| encode_json(row.get(*field).unwrap_or(&Value::Null))),
    );
    Ok(key)
}

/// Row fields the recompute policy classifies transitions on.
pub fn evidence_state(row: &Map<String, Value>, segments: Option<&[Value]>) -> Value {
    let segments = segments
        .map(|list| Value::Array(list.to_vec()))
        .unwrap_or_else(|| Value::Array(Vec::new()));
    serde_json::json!({
        "lifecycle": row.get("lifecycle").cloned().unwrap_or(Value::Null),
        "config_fingerprint": row.get("config_fingerprint").cloned().unwrap_or(Value::Null),
        "evidence_revision": row.get("evidence_revision").cloned().unwrap_or(Value::Null),
        "segments": segments,
    })
}

/// Cached read honoring LRU order; `None` on a miss.
pub fn lru_get(cache: &Mutex<Cache>, key: &CacheKey) -> Option<Value> {
    let mut cache = cache.lock().ok()?;
    let entry = cache.get(key).cloned();
    if entry.is_some() {
        cache.shift_remove(key);
        if let Some(stored) = entry.clone() {
            cache.insert(key.clone(), stored);
        }
    }
    entry
}

/// Store a cached entry, evicting least recently used keys past `limit`.
pub fn lru_put(cache: &Mutex<Cache>, limit: usize, key: CacheKey, entry: Value) {
    if let Ok(mut cache) = cache.lock() {
        cache.shift_remove(&key);
        cache.insert(key, entry);
        while cache.len() > limit {
            cache.shift_remove_index(0);
        }
    }
}

/// Project-relative retained-artifact reference, never a host path
pub fn relative_artifact(root: &Path, path: &str) -> Option<String> {
    let target = canonical(Path::new(path));
    let base = canonical(root);
    target
        .strip_prefix(&base)
        .ok()
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
}

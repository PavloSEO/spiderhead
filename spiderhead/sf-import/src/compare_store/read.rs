//! Bounded paging over a saved comparison stream, backed by a process-local
//! disk index built once per immutable package and reused across pages.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::Connection;
use serde_json::{Map, Value};
use sha2::Digest as _;
use spiderhead_store::evidence_contract::Obj;

use crate::compare::base::CompareError;

use super::export::{iter_compare_rows, INDEX_BATCH};
use super::shared::{py_json, BUCKETS, SQLITE_MAX_INTEGER};

/// One buffered index row: the sort keys plus the serialized document.
type IndexRow = (String, String, String, String, String, String, i64, String);

/// One comparison stream indexed into a temporary SQLite page table. The
/// connection is query-only after the build, so later pages cannot corrupt the
/// package on disk.
pub struct ReadIndex {
    con: Connection,
    _temp_dir: tempfile::TempDir,
    name: String,
}

impl ReadIndex {
    /// Validate the stream end to end and index it; any stream error aborts
    /// the build and removes the temporary directory.
    fn build(manifest_path: &Path, name: &str) -> Result<Self, CompareError> {
        let temp_dir = tempfile::Builder::new()
            .prefix("spiderhead-compare-index-")
            .tempdir()
            .map_err(|e| CompareError(e.to_string()))?;
        let con = Connection::open(temp_dir.path().join("page.sqlite"))
            .map_err(|e| CompareError(e.to_string()))?;
        con.execute_batch(
            "PRAGMA temp_store=FILE;
             PRAGMA cache_size=-8192;
             CREATE TABLE rows (sort_url TEXT,sort_check TEXT,sort_severity TEXT,
             segment TEXT,change_key TEXT,url_fold TEXT,tie_key INTEGER,document TEXT);",
        )
        .map_err(|e| CompareError(e.to_string()))?;
        let mut insert = con
            .prepare("INSERT INTO rows VALUES (?,?,?,?,?,?,?,?)")
            .map_err(|e| CompareError(e.to_string()))?;
        let mut batch: Vec<IndexRow> = Vec::new();
        for (ordinal, row) in iter_compare_rows(manifest_path, name).enumerate() {
            let row = row?;
            let row = row
                .as_object()
                .ok_or_else(|| CompareError("comparison file contains a non-object row".into()))?;
            let (url, check, severity) = sort_fields(name, row);
            let url_fold = py_casefold(&url);
            batch.push((
                url_fold.clone(),
                py_casefold(&check),
                py_casefold(&severity),
                scalar_text(row.get("segment")),
                scalar_text(row.get("change")),
                url_fold,
                ordinal as i64,
                py_json(&Value::Object(row.clone())),
            ));
            if batch.len() as i64 >= INDEX_BATCH {
                flush_batch(&con, &mut insert, &mut batch)?;
            }
        }
        if !batch.is_empty() {
            flush_batch(&con, &mut insert, &mut batch)?;
        }
        con.execute_batch(
            "CREATE INDEX rows_url_order ON rows(sort_url,tie_key);
             CREATE INDEX rows_check_order ON rows(sort_check,tie_key);
             CREATE INDEX rows_severity_order ON rows(sort_severity,tie_key);
             CREATE INDEX rows_segment_change ON rows(segment,change_key);",
        )
        .map_err(|e| CompareError(e.to_string()))?;
        con.execute_batch("PRAGMA query_only=ON")
            .map_err(|e| CompareError(e.to_string()))?;
        drop(insert);
        Ok(Self {
            con,
            _temp_dir: temp_dir,
            name: name.to_owned(),
        })
    }

    /// Read one bounded page under the sort and filters, never exceeding
    /// `max_bytes` of rendered rows.
    #[allow(clippy::too_many_arguments)]
    fn page(
        &mut self,
        offset: i64,
        limit: i64,
        sort: &str,
        direction: &str,
        contains: Option<&str>,
        segment: Option<&str>,
        change: Option<&str>,
        max_bytes: i64,
    ) -> Result<Obj, CompareError> {
        let mut clauses: Vec<String> = Vec::new();
        let mut params: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(segment) = segment {
            clauses.push("segment=?".into());
            params.push(segment.to_owned().into());
        }
        if let Some(change) = change {
            clauses.push("change_key=?".into());
            params.push(change.to_owned().into());
        }
        if let Some(contains) = contains {
            clauses.push("instr(url_fold,?)>0".into());
            params.push(py_casefold(contains).into());
        }
        let where_clause = if clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", clauses.join(" AND "))
        };
        let order = if direction == "asc" { "ASC" } else { "DESC" };
        let sort_column = match sort {
            "url" => "sort_url",
            "check" => "sort_check",
            _ => "sort_severity",
        };
        let total: i64 = self
            .con
            .query_row(
                &format!("SELECT COUNT(*) FROM rows{where_clause}"),
                rusqlite::params_from_iter(params.iter()),
                |row| row.get(0),
            )
            .map_err(|e| CompareError(e.to_string()))?;
        let mut statement = self
            .con
            .prepare(&format!(
                "SELECT document FROM rows{where_clause} \
                 ORDER BY {sort_column} {order},tie_key ASC LIMIT ? OFFSET ?"
            ))
            .map_err(|e| CompareError(e.to_string()))?;
        let mut query_params = params.clone();
        query_params.push(rusqlite::types::Value::Integer(limit));
        query_params.push(rusqlite::types::Value::Integer(offset));
        let mut rows_query = statement
            .query(rusqlite::params_from_iter(query_params.iter()))
            .map_err(|e| CompareError(e.to_string()))?;
        let mut rows: Vec<Value> = Vec::new();
        let mut size: i64 = 0;
        while let Some(row) = rows_query.next().map_err(|e| CompareError(e.to_string()))? {
            let document: String = row.get(0).map_err(|e| CompareError(e.to_string()))?;
            let decoded: Value =
                serde_json::from_str(&document).map_err(|e| CompareError(e.to_string()))?;
            let encoded = py_json(&decoded).len() as i64 + 1;
            if size + encoded > max_bytes {
                if rows.is_empty() {
                    return Err(CompareError("one comparison row exceeds max_bytes".into()));
                }
                break;
            }
            size += encoded;
            rows.push(decoded);
        }
        let mut result = Obj::new();
        result.insert("ok".into(), Value::Bool(true));
        result.insert("format".into(), Value::String("compare.v2.page".into()));
        result.insert("file".into(), Value::String(self.name.clone()));
        result.insert("total".into(), Value::from(total));
        result.insert("offset".into(), Value::from(offset));
        result.insert("returned".into(), Value::from(rows.len() as i64));
        result.insert(
            "has_more".into(),
            Value::Bool(offset + (rows.len() as i64) < total),
        );
        result.insert("sort".into(), Value::String(sort.to_owned()));
        result.insert("direction".into(), Value::String(direction.to_owned()));
        if let Some(segment) = segment {
            result.insert("segment".into(), Value::String(segment.to_owned()));
        }
        if let Some(change) = change {
            result.insert("change".into(), Value::String(change.to_owned()));
        }
        result.insert("rows".into(), Value::Array(rows));
        Ok(result)
    }
}

/// Sort keys of one stream row: the segment-change rows sort by their merged
/// before/after issue, everything else by itself.
fn sort_fields(name: &str, row: &Map<String, Value>) -> (String, String, String) {
    if name == "segment_changes" && row.contains_key("before") {
        let after = row.get("after").filter(|v| crate::pyvalue::truthy(v));
        let before = row.get("before").filter(|v| crate::pyvalue::truthy(v));
        return (
            merged_field(after, before, "target_url"),
            merged_field(after, before, "check"),
            merged_field(after, before, "severity"),
        );
    }
    // The merged issue is the row's issue, else its after-state, else the row;
    // falsy or non-object lookups fall through like an absent field.
    let issue = if name == "segment_changes" {
        row.get("issue")
            .filter(|v| crate::pyvalue::truthy(v))
            .and_then(Value::as_object)
            .or_else(|| {
                row.get("after")
                    .filter(|v| crate::pyvalue::truthy(v))
                    .and_then(Value::as_object)
            })
            .unwrap_or(row)
    } else {
        row
    };
    (
        field_or(issue, "target_url", "url"),
        field_or(issue, "check", "check"),
        plain_field(issue, "severity"),
    )
}

/// First truthy `key` across after-then-before, else empty.
fn merged_field(after: Option<&Value>, before: Option<&Value>, key: &str) -> String {
    plain_field_opt(
        after
            .and_then(|v| v.get(key))
            .filter(|v| crate::pyvalue::truthy(v))
            .or_else(|| {
                before
                    .and_then(|v| v.get(key))
                    .filter(|v| crate::pyvalue::truthy(v))
            }),
    )
}

/// `value.get(a) or value.get(b) or ""` for one object.
fn field_or(value: &Map<String, Value>, primary: &str, fallback: &str) -> String {
    plain_field_opt(
        value
            .get(primary)
            .filter(|v| crate::pyvalue::truthy(v))
            .or_else(|| value.get(fallback).filter(|v| crate::pyvalue::truthy(v))),
    )
}

fn plain_field(value: &Map<String, Value>, key: &str) -> String {
    plain_field_opt(value.get(key))
}

/// reference `str(coerced)` of a truthy-selected field, empty when absent.
fn plain_field_opt(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        _ => String::new(),
    }
}

/// Missing keys store the empty string; non-string scalars store JSON text so
/// they never match a string filter, like SQLite would keep them unmatchable.
fn scalar_text(value: Option<&Value>) -> String {
    match value {
        None => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => py_json(other),
    }
}

/// Unicode case folding; plain lowercasing covers the URL/check/severity
/// alphabets this column sorts and filters on.
fn py_casefold(text: &str) -> String {
    text.to_lowercase()
}

fn flush_batch(
    con: &Connection,
    insert: &mut rusqlite::Statement<'_>,
    batch: &mut Vec<IndexRow>,
) -> Result<(), CompareError> {
    con.execute_batch("BEGIN")
        .map_err(|e| CompareError(e.to_string()))?;
    for row in batch.iter() {
        insert
            .execute(rusqlite::params![
                row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7
            ])
            .map_err(|e| CompareError(e.to_string()))?;
    }
    batch.clear();
    con.execute_batch("COMMIT")
        .map_err(|e| CompareError(e.to_string()))?;
    Ok(())
}

type FileSignature = (u64, u64, u64, i64, i64);

/// Everything one cached index depends on: manifest identity plus the exact
/// bytes and stat signatures of both the manifest and the data file.
#[derive(Clone, PartialEq)]
struct CacheKey(PathBuf, String, String, FileSignature, FileSignature);

/// At most one retained index; a second stream replaces it.
static READ_INDEX_CACHE: Mutex<Option<(CacheKey, ReadIndex)>> = Mutex::new(None);

fn cache_guard() -> std::sync::MutexGuard<'static, Option<(CacheKey, ReadIndex)>> {
    match READ_INDEX_CACHE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    }
}

fn signature(info: &std::fs::Metadata) -> FileSignature {
    use std::os::unix::fs::MetadataExt;
    (
        info.dev(),
        info.ino(),
        info.size(),
        info.mtime() * 1_000_000_000 + info.mtime_nsec(),
        info.ctime() * 1_000_000_000 + info.ctime_nsec(),
    )
}

/// Read a bounded page; the disk index for this immutable package is built
/// once and retained across pages in this process.
#[allow(clippy::too_many_arguments)]
pub fn read_compare_page(
    manifest_path: &Path,
    name: &str,
    offset: i64,
    limit: i64,
    sort: &str,
    direction: &str,
    contains: Option<&str>,
    segment: Option<&str>,
    change: Option<&str>,
    max_bytes: i64,
) -> Result<Obj, CompareError> {
    if !(0..=SQLITE_MAX_INTEGER).contains(&offset) {
        return Err(CompareError(
            "offset must be an integer from 0 through 9223372036854775807".into(),
        ));
    }
    if !(1..=500).contains(&limit) {
        return Err(CompareError("limit must be an integer 1..500".into()));
    }
    if !matches!(sort, "url" | "check" | "severity") {
        return Err(CompareError("sort must be url, check, or severity".into()));
    }
    if !matches!(direction, "asc" | "desc") {
        return Err(CompareError("direction must be asc or desc".into()));
    }
    if !(4096..=8 * 1024 * 1024).contains(&max_bytes) {
        return Err(CompareError("max_bytes must be 4096..8388608".into()));
    }
    if contains.is_some_and(|text| text.chars().count() > 2048) {
        return Err(CompareError(
            "contains must be at most 2048 characters".into(),
        ));
    }
    if segment.is_some_and(|text| text.chars().count() > 256) {
        return Err(CompareError(
            "segment must be at most 256 characters".into(),
        ));
    }
    if let Some(change) = change {
        if !BUCKETS.contains(&change) {
            return Err(CompareError(
                "change must be entered, left, appeared, disappeared, or unchanged".into(),
            ));
        }
    }
    if name != "segment_changes" && (segment.is_some() || change.is_some()) {
        return Err(CompareError(
            "segment and change filters require file=segment_changes".into(),
        ));
    }
    let manifest_path = absolute(manifest_path);
    if manifest_path.is_symlink() {
        return Err(CompareError(
            "comparison manifest must not be a symbolic link".into(),
        ));
    }
    let raw_manifest = std::fs::read(&manifest_path).map_err(|_| {
        CompareError(format!(
            "could not read comparison manifest for {}",
            crate::pyvalue::py_repr(name)
        ))
    })?;
    let manifest: Value = serde_json::from_slice(&raw_manifest).map_err(|_| {
        CompareError(format!(
            "could not read comparison manifest for {}",
            crate::pyvalue::py_repr(name)
        ))
    })?;
    let entry = manifest
        .get("schema_version")
        .filter(|version| version.as_str() == Some(super::COMPARE_V2))
        .and(manifest.get("files"))
        .and_then(Value::as_object)
        .and_then(|files| files.get(name))
        .ok_or_else(|| {
            CompareError(format!(
                "comparison manifest does not contain file {}",
                crate::pyvalue::py_repr(name)
            ))
        })?;
    let entry = entry
        .as_object()
        .ok_or_else(|| CompareError("comparison file entry is malformed".into()))?;
    let relative = entry
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| CompareError("comparison file path must be a local filename".into()))?;
    if Path::new(relative).file_name() != Some(std::ffi::OsStr::new(relative))
        || relative == "."
        || relative == ".."
    {
        return Err(CompareError(
            "comparison file path must be a local filename".into(),
        ));
    }
    let data_path = manifest_path
        .parent()
        .unwrap_or(Path::new("."))
        .join(relative);
    let file_info = std::fs::symlink_metadata(&data_path).map_err(|_| {
        CompareError(format!(
            "comparison data file is unavailable for {}",
            crate::pyvalue::py_repr(name)
        ))
    })?;
    let manifest_info = std::fs::metadata(&manifest_path).map_err(|_| {
        CompareError(format!(
            "comparison data file is unavailable for {}",
            crate::pyvalue::py_repr(name)
        ))
    })?;
    if file_info.is_symlink() {
        return Err(CompareError(
            "comparison file must not be a symbolic link".into(),
        ));
    }
    let manifest_sha = hex_sha256(&raw_manifest);
    let key = CacheKey(
        manifest_path.clone(),
        name.to_owned(),
        manifest_sha,
        signature(&file_info),
        signature(&manifest_info),
    );

    let mut guard = cache_guard();
    if let Some((cached_key, index)) = guard.as_mut() {
        if *cached_key == key {
            return index.page(
                offset, limit, sort, direction, contains, segment, change, max_bytes,
            );
        }
    }
    // Replace, not add: a second stream evicts the retained one, whose stale
    // entries for this path would otherwise survive a package rewrite.
    *guard = None;
    let index = ReadIndex::build(&manifest_path, name)?;
    let mut built = (key, index);
    let page = built.1.page(
        offset, limit, sort, direction, contains, segment, change, max_bytes,
    );
    *guard = Some(built);
    page
}

fn hex_sha256(bytes: &[u8]) -> String {
    super::shared::hex(&sha2::Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use super::super::export::export_one;

    fn build_package(dir: &Path) -> PathBuf {
        let rows: Vec<Value> = (0..5)
            .map(|index| {
                json!({
                    "check": if index % 2 == 0 { "missing_title" } else { "broken_link" },
                    "target_url": format!("https://example.com/page-{index}"),
                    "severity": "warning",
                    "change": "entered",
                    "segment": "docs",
                })
            })
            .collect();
        let entered = export_one(
            dir,
            "entered",
            &mut rows.clone().into_iter().map(Ok),
            "none",
            None,
            None,
        )
        .expect("export");
        let changes: Vec<Value> = rows
            .iter()
            .map(|issue| json!({"change": "entered", "segment": "docs", "before": null, "issue": issue}))
            .collect();
        let segment_changes = export_one(
            dir,
            "segment_changes",
            &mut changes.into_iter().map(Ok),
            "none",
            None,
            None,
        )
        .expect("export");
        let manifest = dir.join("compare.json");
        std::fs::write(
            &manifest,
            serde_json::to_string(&json!({
                "schema_version": super::super::COMPARE_V2,
                "files": {"entered": entered, "segment_changes": segment_changes},
            }))
            .expect("json"),
        )
        .expect("write");
        manifest
    }

    #[test]
    fn paging_is_bounded_and_ordered() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = build_package(dir.path());
        let first = read_compare_page(
            &manifest, "entered", 0, 2, "url", "asc", None, None, None, 1_048_576,
        )
        .expect("page");
        assert_eq!(first["total"], json!(5));
        assert_eq!(first["returned"], json!(2));
        assert_eq!(first["has_more"], json!(true));
        let second = read_compare_page(
            &manifest, "entered", 4, 2, "url", "asc", None, None, None, 1_048_576,
        )
        .expect("page");
        assert_eq!(second["returned"], json!(1));
        assert_eq!(second["has_more"], json!(false));
        // The cached index serves repeat pages from the same package.
        let cached = read_compare_page(
            &manifest, "entered", 0, 5, "url", "asc", None, None, None, 1_048_576,
        )
        .expect("cached page");
        assert_eq!(cached["returned"], json!(5));
    }

    #[test]
    fn filters_narrow_the_page() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = build_package(dir.path());
        let filtered = read_compare_page(
            &manifest,
            "entered",
            0,
            10,
            "check",
            "asc",
            Some("page-1"),
            None,
            None,
            1_048_576,
        )
        .expect("page");
        assert_eq!(filtered["total"], json!(1));
        let segment_page = read_compare_page(
            &manifest,
            "segment_changes",
            0,
            10,
            "url",
            "desc",
            None,
            Some("docs"),
            Some("entered"),
            1_048_576,
        )
        .expect("page");
        assert_eq!(segment_page["total"], json!(5));
        assert_eq!(segment_page["segment"], json!("docs"));
        assert_eq!(segment_page["change"], json!("entered"));
        let refused = read_compare_page(
            &manifest,
            "entered",
            0,
            10,
            "url",
            "asc",
            None,
            Some("docs"),
            None,
            1_048_576,
        );
        assert_eq!(
            refused.err().map(|e| e.0),
            Some("segment and change filters require file=segment_changes".into())
        );
    }

    #[test]
    fn validation_is_ordered() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = build_package(dir.path());
        assert_eq!(
            read_compare_page(
                &manifest, "entered", -1, 2, "url", "asc", None, None, None, 1_048_576
            )
            .err()
            .map(|e| e.0),
            Some("offset must be an integer from 0 through 9223372036854775807".into())
        );
        assert_eq!(
            read_compare_page(
                &manifest, "entered", 0, 501, "url", "asc", None, None, None, 1_048_576
            )
            .err()
            .map(|e| e.0),
            Some("limit must be an integer 1..500".into())
        );
        assert_eq!(
            read_compare_page(
                &manifest, "entered", 0, 2, "target", "asc", None, None, None, 1_048_576
            )
            .err()
            .map(|e| e.0),
            Some("sort must be url, check, or severity".into())
        );
        assert_eq!(
            read_compare_page(&manifest, "entered", 0, 2, "url", "up", None, None, None, 1_048_576)
                .err()
                .map(|e| e.0),
            Some("direction must be asc or desc".into())
        );
        assert_eq!(
            read_compare_page(&manifest, "entered", 0, 2, "url", "asc", None, None, None, 100)
                .err()
                .map(|e| e.0),
            Some("max_bytes must be 4096..8388608".into())
        );
        assert_eq!(
            read_compare_page(
                &manifest,
                "entered",
                0,
                2,
                "url",
                "asc",
                Some("x"),
                None,
                None,
                1_048_576
            )
            .err()
            .map(|e| e.0),
            None,
            "contains up to 2048 characters is fine"
        );
        assert_eq!(
            read_compare_page(
                &manifest,
                "entered",
                0,
                2,
                "url",
                "asc",
                None,
                None,
                Some("other"),
                1_048_576
            )
            .err()
            .map(|e| e.0),
            Some("change must be entered, left, appeared, disappeared, or unchanged".into())
        );
    }

    #[test]
    fn oversized_first_row_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let payload = "x".repeat(5000);
        let rows = vec![json!({"check": "big", "target_url": payload, "change": "entered"})];
        let entry = export_one(
            dir.path(),
            "entered",
            &mut rows.into_iter().map(Ok),
            "none",
            None,
            None,
        )
        .expect("export");
        let manifest = dir.path().join("compare.json");
        std::fs::write(
            &manifest,
            serde_json::to_string(&json!({
                "schema_version": super::super::COMPARE_V2,
                "files": {"entered": entry},
            }))
            .expect("json"),
        )
        .expect("write");
        let error = read_compare_page(
            &manifest, "entered", 0, 5, "url", "asc", None, None, None, 4096,
        );
        assert_eq!(
            error.err().map(|e| e.0),
            Some("one comparison row exceeds max_bytes".into())
        );
    }
}

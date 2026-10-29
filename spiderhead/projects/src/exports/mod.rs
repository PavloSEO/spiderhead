//! Project dataset export jobs.
//!
//! Bounded task/scan-comparison exports with durable progress and history. A job is a JSON
//! record plus an advisory lock under `reports/.export-jobs/`; artifacts are written to a
//! `.partial` temporary, checksummed, and published by hard-link.
//!
//! The compare-row reader and `free_bytes` are local copies of store-crate helpers
//! that are not exported yet.

use spiderhead_core::filesystem::{NoFollow, PrivateMode};
use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::bindings::py_str;
use crate::error::{py_exception_name, py_named, ProjectsError, ProjectsResult};
use crate::locks;
use crate::pyjson;
use crate::pytime::zulu;

mod matches_artifact;
mod nested_field;
mod validate_export;
use matches_artifact::*;
pub use nested_field::*;
pub use validate_export::*;

/// Export history document name.
pub const HISTORY_NAME: &str = "exports.json";
/// Export history format.
pub const HISTORY_FORMAT: &str = "spiderhead.project-exports.v1";
/// Directory holding per-job files and locks.
pub const JOB_DIR: &str = "reports/.export-jobs";
const MAX_EXPORT_ROWS: i64 = 10_000_000;
const MAX_EXPORT_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const XLSX_CELL_LIMIT: usize = 32_767;
const XLSX_ROW_LIMIT: i64 = 1_048_575;
const JOB_FILE_BYTES: u64 = 64 * 1024;
const MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
const FREE_SPACE_RESERVE: i64 = 10 * 1024 * 1024 * 1024;

/// Live worker identities; presence means the worker is alive (it removes itself last).
static ACTIVE: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

const TASK_COLUMNS: [&str; 19] = [
    "task_id",
    "title",
    "kind",
    "source_hash",
    "operation",
    "state",
    "attempt_status",
    "assignee",
    "description",
    "acceptance_criterion",
    "finding_ref",
    "method_ref",
    "competitors",
    "affected_url_count",
    "affected_url_state",
    "affected_url_reason",
    "actor",
    "complete",
    "reason",
];
const COMPARE_COLUMNS: [&str; 5] = ["category", "check", "severity", "target_url", "row_json"];
const TASK_COMMENT_COLUMNS: [&str; 6] = [
    "task_id",
    "comment_ordinal",
    "created_at",
    "actor",
    "text",
    "reference",
];
const TASK_HISTORY_COLUMNS: [&str; 8] = [
    "task_id",
    "event_type",
    "event_ordinal",
    "occurred_at",
    "revision",
    "actor",
    "status",
    "entry_json",
];

fn export_error(message: &str) -> ProjectsError {
    py_named("ProjectExportError", message)
}

fn compare_error(message: &str) -> ProjectsError {
    py_named("CompareError", message)
}

/// The internal `_ExportCancelled` sentinel; never reaches public callers.
fn cancelled() -> ProjectsError {
    py_named("__ExportCancelled", "export cancelled by the caller")
}

fn is_cancelled(error: &ProjectsError) -> bool {
    matches!(error, ProjectsError::Py { name, .. } if *name == "__ExportCancelled")
}

fn key_error(key: &str) -> ProjectsError {
    py_named("KeyError", format!("'{key}'"))
}

/// Type name of one JSON value, spelled as recorded in error messages.
fn py_typename(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// `pathlib.PurePath.name` on a POSIX string (leading-dot and `..` rules included).
fn py_path_name(text: &str) -> String {
    let trimmed = text.trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    let last = trimmed.rsplit('/').next().unwrap_or_default();
    if last == "." || last == ".." {
        String::new()
    } else {
        last.to_string()
    }
}

/// `pathlib.PurePath.suffix` on a POSIX string.
fn py_suffix(text: &str) -> String {
    let name = py_path_name(text);
    let bytes = name.as_bytes();
    match name.rfind('.') {
        Some(i) if i > 0 && i < bytes.len() - 1 => name[i..].to_string(),
        _ => String::new(),
    }
}

/// `str.casefold()` — full Unicode case folding.
fn casefold(text: &str) -> String {
    icu_casemap::CaseMapper::new().fold_string(text).to_string()
}

/// `Path.is_symlink()` plus `os.path.lexists()` as one pair.
fn path_flags(path: &Path) -> (bool, bool) {
    match path.symlink_metadata() {
        Ok(meta) => (true, meta.file_type().is_symlink()),
        Err(_) => (false, false),
    }
}

fn lexists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

/// Read a JSON scalar as an integer: bools, integers, truncated floats, numeric strings.
fn py_int(v: &Value) -> ProjectsResult<i64> {
    match v {
        Value::Bool(b) => Ok(i64::from(*b)),
        Value::Number(n) => Ok(n.as_i64().or_else(|| n.as_f64().map(|f| f.trunc() as i64)).unwrap_or(0)),
        Value::String(s) => s.trim().parse::<i64>().map_err(|_| {
            py_named(
                "TypeError",
                "int() argument must be a string, a bytes-like object or a real number, not 'NoneType'",
            )
        }),
        _ => Err(py_named(
            "TypeError",
            format!(
                "int() argument must be a string, a bytes-like object or a real number, not '{}'",
                py_typename(v)
            ),
        )),
    }
}

/// `document["items"]` with `KeyError`/`AttributeError` parity.
fn document_items(document: &Value) -> ProjectsResult<&Map<String, Value>> {
    if !document.is_object() {
        return Err(py_named(
            "TypeError",
            format!("'{}' object is not subscriptable", py_typename(document)),
        ));
    }
    match document.get("items") {
        None => Err(key_error("items")),
        Some(items) => items.as_object().ok_or_else(|| {
            py_named(
                "AttributeError",
                format!("'{}' object has no attribute 'items'", py_typename(items)),
            )
        }),
    }
}

/// `obj[key]` with `KeyError` parity.
fn obj_get<'a>(obj: &'a Value, key: &str) -> ProjectsResult<&'a Value> {
    if !obj.is_object() {
        return Err(py_named(
            "TypeError",
            format!("'{}' object is not subscriptable", py_typename(obj)),
        ));
    }
    obj.get(key).ok_or_else(|| key_error(key))
}

/// `os.open(path, O_CREAT|O_EXCL|O_WRONLY, 0o600)`.
fn open_temporary(path: &Path) -> ProjectsResult<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .private_mode(0o600)
        .open(path)
        .map_err(ProjectsError::Io)
}

/// `shutil.disk_usage(dir).free` — free bytes on the volume holding `dir`.
fn free_bytes(dir: &Path) -> std::io::Result<i64> {
    spiderhead_core::filesystem::free_space(dir).map(|v| i64::try_from(v).unwrap_or(i64::MAX))
}

/// One `csv.writer(lineterminator="\n")` row: QUOTE_MINIMAL escaping.
fn csv_line(values: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        if value
            .bytes()
            .any(|b| matches!(b, b',' | b'"' | b'\r' | b'\n'))
        {
            out.push(b'"');
            for chunk in value.split('"') {
                out.extend_from_slice(chunk.as_bytes());
                out.extend_from_slice(b"\"\"");
            }
            out.truncate(out.len() - 2);
            out.push(b'"');
        } else {
            out.extend_from_slice(value.as_bytes());
        }
    }
    out.push(b'\n');
    out
}

/// `_safe_cell`: stringify one cell with the spreadsheet-injection guard.
fn safe_cell(value: Option<&Value>) -> String {
    let text = match value {
        None | Some(Value::Null) => return String::new(),
        Some(v @ (Value::Object(_) | Value::Array(_))) => pyjson::dumps_compact(v),
        Some(other) => py_str(other),
    };
    if text.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{text}")
    } else {
        text
    }
}

/// `_job_path`: validated path of the job record (or `.lock`/`.cancel` via caller).
fn job_path(root: &Path, job_id: &str, create: bool) -> ProjectsResult<PathBuf> {
    let normalized = uuid::Uuid::parse_str(job_id)
        .map(|u| u.to_string())
        .map_err(|_| export_error("job_id must be a UUID"))?;
    let reports = root.join("reports");
    if reports.is_symlink() || !reports.is_dir() {
        return Err(export_error(
            "project reports directory is unavailable or unsafe",
        ));
    }
    let directory = root.join(JOB_DIR);
    if directory.is_symlink() || (directory.exists() && !directory.is_dir()) {
        return Err(export_error("unsafe export-job directory"));
    }
    if create {
        std::fs::DirBuilder::new()
            .recursive(true)
            .private_mode(0o700)
            .create(&directory)
            .map_err(ProjectsError::Io)?;
    } else if !directory.is_dir() {
        return Err(export_error("export job is unavailable"));
    }
    Ok(directory.join(format!("{normalized}.json")))
}

/// `_acquire_job_lock`: a portable advisory lock so status/retries see live workers.
fn acquire_job_lock(
    root: &Path,
    job_id: &str,
    create: bool,
) -> ProjectsResult<(Option<std::fs::File>, bool)> {
    let job = job_path(root, job_id, create)?;
    let path = job.with_extension("lock");
    if !create && !path.exists() {
        return Ok((None, false));
    }
    if path.is_symlink() {
        return Err(export_error("unsafe export lock path"));
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .no_follow()
        .private_mode(0o600)
        .open(&path)
        .map_err(ProjectsError::Io)?;
    match locks::lock_exclusive(&file) {
        Ok(()) => Ok((Some(file), false)),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok((None, true)),
        Err(e) => Err(ProjectsError::Io(e)),
    }
}

/// `_release_job_lock`: unlock then close; close alone would also release `flock`.
fn release_job_lock(file: Option<std::fs::File>) {
    if let Some(file) = file {
        let _ = locks::unlock(&file);
    }
}

/// `has_active_jobs`: fail closed when an export worker still owns any job lock.
pub fn has_active_jobs(root: &Path) -> ProjectsResult<bool> {
    let directory = root.join(JOB_DIR);
    if !directory.exists() && !directory.is_symlink() {
        return Ok(false);
    }
    if directory.is_symlink() || !directory.is_dir() {
        return Err(export_error("unsafe export-job directory"));
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&directory)
        .map_err(ProjectsError::Io)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(".lock"))
        })
        .collect();
    paths.sort();
    if paths.len() > 10_000 {
        return Err(export_error(
            "export-job history exceeds its inspection limit",
        ));
    }
    for path in paths {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if path.is_symlink() || py_suffix(&name) != ".lock" {
            return Err(export_error("unsafe export lock path"));
        }
        let stem = name.strip_suffix(".lock").unwrap_or(&name);
        let identity = uuid::Uuid::parse_str(stem)
            .map(|u| u.to_string())
            .map_err(|_| export_error("invalid export lock path"))?;
        let (descriptor, busy) = acquire_job_lock(root, &identity, false)?;
        if busy {
            return Ok(true);
        }
        release_job_lock(descriptor);
    }
    Ok(false)
}

/// `_read_job`: parse and validate one retained job record.
fn read_job(root: &Path, job_id: &str) -> ProjectsResult<Value> {
    let path = job_path(root, job_id, false)?;
    if path.is_symlink()
        || !path.is_file()
        || path.metadata().map(|m| m.len()).unwrap_or(u64::MAX) > JOB_FILE_BYTES
    {
        return Err(export_error("export job is unavailable"));
    }
    let text = std::fs::read_to_string(&path).map_err(ProjectsError::Io)?;
    let value: Value = serde_json::from_str(&text).map_err(ProjectsError::Json)?;
    let normalized = uuid::Uuid::parse_str(job_id)
        .map(|u| u.to_string())
        .map_err(|_| export_error("job_id must be a UUID"))?;
    if !value.is_object()
        || !spiderhead_core::formats::matches(
            value["record_format"].as_str(),
            "spiderhead.project-export-job.v1",
        )
        || value["job_id"].as_str() != Some(normalized.as_str())
    {
        return Err(export_error("export job has an invalid record"));
    }
    Ok(value)
}

/// `_write_job`: atomically publish the job record.
fn write_job(root: &Path, job: &Value) -> ProjectsResult<()> {
    let path = job_path(root, job["job_id"].as_str().unwrap_or_default(), true)?;
    let content = pyjson::dumps_indent2(job) + "\n";
    spiderhead_core::filesystem::atomic_write_bytes(&path, content.as_bytes(), None, None, true)
        .map_err(ProjectsError::Io)
}

/// `_check_cancel`: a `.cancel` marker aborts a live worker.
fn check_cancel(root: &Path, job: &Value) -> ProjectsResult<()> {
    let cancel =
        job_path(root, job["job_id"].as_str().unwrap_or_default(), false)?.with_extension("cancel");
    let (exists, symlink) = path_flags(&cancel);
    if exists && !symlink && cancel.is_file() {
        return Err(cancelled());
    }
    Ok(())
}

/// `_artifact_paths`: the publish target and its `.partial` temporary.
fn artifact_paths(root: &Path, job: &Value) -> ProjectsResult<(PathBuf, PathBuf)> {
    let output = job.get("output");
    let job_id = uuid::Uuid::parse_str(job["job_id"].as_str().unwrap_or_default())
        .map(|u| u.to_string())
        .unwrap_or_default();
    let expected_temporary = format!(".{job_id}.partial");
    let temporary_name = job
        .get("temporary")
        .and_then(|v| v.as_str())
        .unwrap_or(&expected_temporary)
        .to_string();
    let output_ok = output.is_some_and(|o| o.is_string())
        && output
            .and_then(|o| o.as_str())
            .is_some_and(|o| py_path_name(o) == o && !o.starts_with('.'));
    let temp_ok =
        py_path_name(&temporary_name) == temporary_name && temporary_name == expected_temporary;
    if !output_ok || !temp_ok {
        return Err(export_error("export job has unsafe output paths"));
    }
    let reports = root.join("reports");
    if reports.is_symlink() || !reports.is_dir() {
        return Err(export_error(
            "project reports directory is unavailable or unsafe",
        ));
    }
    Ok((
        reports.join(output.and_then(|o| o.as_str()).unwrap_or_default()),
        reports.join(&temporary_name),
    ))
}

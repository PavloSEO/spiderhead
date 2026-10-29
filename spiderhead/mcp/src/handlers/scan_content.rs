//! Retained content search: write a bounded NDJSON package from a finished
//! scan, and page through a written package without rescanning evidence.

use std::io::{BufRead, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use spiderhead_store::content_search::{search_scan, SearchOptions, MAX_SERIALIZED_RECORD_BYTES};

use crate::error::ToolError;

use super::{failed, need_str, opt, opt_bool, opt_str};

const MAX_PACKAGE_BYTES: i64 = 512 * 1024 * 1024;
const MAX_MANIFEST_BYTES: i64 = 1024 * 1024;
const INDEX_ENTRY: i64 = 40;
const FORMAT: &str = "spiderhead.retained-content-search.v1";

fn refuse(message: &str) -> ToolError {
    ToolError::Failed(message.to_owned())
}

fn io_error(error: std::io::Error) -> ToolError {
    ToolError::Failed(error.to_string())
}

/// `sort_keys` + two-space indent text of a manifest, with a trailing newline.
fn sorted(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            Value::Object(
                keys.into_iter()
                    .map(|k| (k.clone(), sorted(&map[k])))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

/// `scan_content_search`: search one finished scan and publish a local package.
pub fn scan_content_search(args: &Value) -> Result<Value, ToolError> {
    const ACCEPTED: [&str; 11] = [
        "input_path",
        "query",
        "out_dir",
        "scope",
        "mode",
        "representation",
        "selector",
        "case_sensitive",
        "include_snippets",
        "kind",
        "url",
    ];
    let args = super::kwargs(args);
    let input = need_str(&args, "input_path", "scan-content-search", &ACCEPTED)?;
    let query = need_str(&args, "query", "scan-content-search", &ACCEPTED)?;
    let out_dir = need_str(&args, "out_dir", "scan-content-search", &ACCEPTED)?;
    if out_dir.is_empty() {
        return Err(refuse(
            "out_dir required: a new local content-search package directory",
        ));
    }
    let destination = PathBuf::from(out_dir);
    let parent = destination
        .parent()
        .map(|p| {
            if p.as_os_str().is_empty() {
                Path::new(".")
            } else {
                p
            }
        })
        .unwrap_or(Path::new("."))
        .to_path_buf();
    if std::fs::symlink_metadata(&destination).is_ok() {
        return Err(refuse(
            "content-search out_dir must be a new non-symlink path",
        ));
    }
    let parent_ok = std::fs::symlink_metadata(&parent).is_ok_and(|m| m.is_dir());
    if !parent_ok {
        return Err(refuse(
            "content-search output parent must be an existing non-symlink directory",
        ));
    }
    let name = destination
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stage = parent.join(format!(
        ".{name}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir(&stage).map_err(io_error)?;
    let published = write_package(&args, input, query, &destination, &stage);
    if published.is_err() {
        let _ = std::fs::remove_dir_all(&stage);
    }
    published
}

fn write_package(
    args: &Map<String, Value>,
    input: &str,
    query: &str,
    destination: &Path,
    stage: &Path,
) -> Result<Value, ToolError> {
    let include_snippets = opt_bool(args, "include_snippets").unwrap_or(false);
    let url = opt_str(args, "url").map(str::to_owned);
    let options = SearchOptions {
        scope: opt_str(args, "scope").unwrap_or("raw_html").to_owned(),
        mode: opt_str(args, "mode").unwrap_or("contains").to_owned(),
        representations: vec![opt_str(args, "representation")
            .unwrap_or("static")
            .to_owned()],
        selector: opt_str(args, "selector").map(str::to_owned),
        case_sensitive: opt_bool(args, "case_sensitive").unwrap_or(false),
        allow_active: false,
        include_snippets,
        kind: opt_str(args, "kind").unwrap_or("literal").to_owned(),
        url: url.clone(),
    };
    let records_path = stage.join("records.ndjson");
    let index_path = stage.join("records.idx");
    let mut records = std::fs::File::create(&records_path).map_err(io_error)?;
    let mut index = std::fs::File::create(&index_path).map_err(io_error)?;
    let (mut count, mut record_bytes, mut limit_hit) = (0i64, 0i64, false);
    let mut digest = Sha256::new();
    let mut failure: Option<ToolError> = None;
    let mut emit = |record: &Map<String, Value>| -> Option<bool> {
        let mut encoded = match serde_json::to_vec(record) {
            Ok(bytes) => bytes,
            Err(error) => {
                failure = Some(ToolError::Failed(error.to_string()));
                return Some(false);
            }
        };
        encoded.push(b'\n');
        if encoded.len() as i64 > MAX_SERIALIZED_RECORD_BYTES {
            failure = Some(refuse(
                "content-search record exceeds the 64 KiB serialized record limit",
            ));
            return Some(false);
        }
        let projected = record_bytes + encoded.len() as i64 + (count + 1) * INDEX_ENTRY;
        if projected + MAX_MANIFEST_BYTES > MAX_PACKAGE_BYTES {
            limit_hit = true;
            return Some(false);
        }
        let written = index
            .write_all(&(record_bytes as u64).to_be_bytes())
            .and_then(|()| index.write_all(&Sha256::digest(&encoded)))
            .and_then(|()| records.write_all(&encoded));
        if let Err(error) = written {
            failure = Some(io_error(error));
            return Some(false);
        }
        digest.update(&encoded);
        record_bytes += encoded.len() as i64;
        count += 1;
        None
    };
    let searched = search_scan(Path::new(input), query, &options, Some(&mut emit));
    if let Some(error) = failure {
        return Err(error);
    }
    let mut summary = searched.map_err(failed)?;
    if limit_hit {
        summary.insert("search_completed".into(), json!(false));
        summary.insert("ok".into(), json!(false));
        summary.insert("absence_confirmed".into(), json!(false));
        if let Some(Value::Object(coverage)) = summary.get_mut("coverage") {
            coverage.insert("package_output_truncated".into(), json!(true));
            coverage.insert(
                "package_output_limit_bytes".into(),
                json!(MAX_PACKAGE_BYTES),
            );
            coverage.insert(
                "stream_error".into(),
                json!("derived package byte limit reached"),
            );
            let mut reasons: Vec<Value> = coverage
                .get("partial_reasons")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|r| {
                    r.as_str()
                        != Some(
                            "record consumer stopped before all selected documents were emitted",
                        )
                })
                .collect();
            reasons.push(json!(
                "derived package byte limit reached before all selected documents were emitted"
            ));
            coverage.insert("partial_reasons".into(), Value::Array(reasons));
        }
    }
    let field = |key: &str| summary.get(key).cloned().unwrap_or(Value::Null);
    let sha: String = digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let manifest = json!({
        "format": FORMAT,
        "search_completed": field("search_completed"),
        "source": field("source"),
        "scope": field("scope"),
        "mode": field("mode"),
        "kind": field("kind"),
        "representations": field("representations"),
        "coverage": field("coverage"),
        "url_filter": url,
        "absence_confirmed": field("absence_confirmed"),
        "records": {
            "file": "records.ndjson",
            "index": "records.idx",
            "count": count,
            "bytes": record_bytes,
            "sha256": sha,
            "index_entry_bytes": INDEX_ENTRY,
            "record_byte_limit": MAX_SERIALIZED_RECORD_BYTES,
        },
        "package_byte_limit": MAX_PACKAGE_BYTES,
        "snippets": if include_snippets { "included" } else { "omitted" },
        "network": false,
    });
    let mut manifest_text = spiderhead_core::common::python_json_pretty(&sorted(&manifest));
    manifest_text.push('\n');
    if manifest_text.len() as i64 > MAX_MANIFEST_BYTES {
        return Err(refuse("content-search package manifest exceeds 1 MiB"));
    }
    let package_bytes = record_bytes + count * INDEX_ENTRY + manifest_text.len() as i64;
    if package_bytes > MAX_PACKAGE_BYTES {
        return Err(refuse("content-search package exceeds its byte limit"));
    }
    std::fs::write(stage.join("manifest.json"), manifest_text.as_bytes()).map_err(io_error)?;
    if std::fs::symlink_metadata(destination).is_ok() {
        return Err(refuse(
            "content-search out_dir appeared while the package was being written",
        ));
    }
    std::fs::rename(stage, destination).map_err(io_error)?;
    let coverage_state = summary["coverage"]["state"].clone();
    let completed = summary["search_completed"] == json!(true);
    let status = if completed && coverage_state == json!("complete") {
        "complete"
    } else if completed {
        "partial"
    } else {
        "incomplete"
    };
    Ok(json!({
        "ok": true,
        "status": status,
        "format": FORMAT,
        "out_dir": destination.to_string_lossy(),
        "manifest": destination.join("manifest.json").to_string_lossy(),
        "records": count,
        "package_bytes": package_bytes,
        "source": field("source"),
        "coverage": field("coverage"),
        "absence_confirmed": field("absence_confirmed"),
        "search_completed": field("search_completed"),
        "capabilities": {"retained_content_search": true},
    }))
}

/// One checked package: its manifest, record stream and index.
struct Package {
    manifest: Value,
    source: Value,
    count: i64,
    records_path: PathBuf,
    index_path: PathBuf,
}

fn regular(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file())
}

fn open_package(package: &str) -> Result<Package, ToolError> {
    let root = Path::new(package);
    let manifest_path = root.join("manifest.json");
    let root_ok = std::fs::symlink_metadata(root).is_ok_and(|m| m.is_dir());
    if !root_ok || !regular(&manifest_path) {
        return Err(refuse(
            "content-search package must be a regular local package directory",
        ));
    }
    let manifest_size = std::fs::metadata(&manifest_path).map_err(io_error)?.len() as i64;
    if manifest_size > MAX_MANIFEST_BYTES {
        return Err(refuse("content-search package manifest exceeds 1 MiB"));
    }
    let invalid = || refuse("content-search package manifest is invalid");
    let manifest: Value = std::fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .ok_or_else(invalid)?;
    let records = manifest.get("records").filter(|r| r.is_object());
    let int = |key: &str| {
        records
            .and_then(|r| r.get(key))
            .and_then(Value::as_i64)
            .filter(|n| *n >= 0)
    };
    let name = |key: &str| {
        records
            .and_then(|r| r.get(key))
            .and_then(Value::as_str)
            .filter(|n| {
                Path::new(n)
                    .file_name()
                    .is_some_and(|f| f.to_str() == Some(n))
            })
    };
    let format_ok = spiderhead_core::formats::matches(manifest["format"].as_str(), FORMAT);
    let limit_ok = records
        .and_then(|r| r.get("record_byte_limit"))
        .is_none_or(|v| v.as_i64() == Some(MAX_SERIALIZED_RECORD_BYTES));
    let (Some(count), Some(bytes), Some(file), Some(index)) =
        (int("count"), int("bytes"), name("file"), name("index"))
    else {
        return Err(invalid());
    };
    let entry_ok = records
        .and_then(|r| r.get("index_entry_bytes"))
        .and_then(Value::as_i64)
        == Some(INDEX_ENTRY);
    if !format_ok || !entry_ok || !limit_ok {
        return Err(invalid());
    }
    let records_path = root.join(file);
    let index_path = root.join(index);
    if !regular(&records_path) || !regular(&index_path) {
        return Err(refuse("content-search package records are unavailable"));
    }
    let index_len = std::fs::metadata(&index_path).map_err(io_error)?.len() as i64;
    let records_len = std::fs::metadata(&records_path).map_err(io_error)?.len() as i64;
    if index_len != count * INDEX_ENTRY || records_len != bytes {
        return Err(refuse("content-search package index length is invalid"));
    }
    if manifest_size + index_len + records_len > MAX_PACKAGE_BYTES {
        return Err(refuse("content-search package exceeds its byte limit"));
    }
    let source = manifest.get("source").filter(|s| s.is_object()).cloned();
    let Some(source) = source else {
        return Err(refuse("content-search package source identity is invalid"));
    };
    if count > 0 && (!source["scan_uuid"].is_string() || !source["evidence_revision"].is_i64()) {
        return Err(refuse("content-search package source identity is invalid"));
    }
    Ok(Package {
        manifest,
        source,
        count,
        records_path,
        index_path,
    })
}

fn checked_row(line: &[u8], digest: &[u8], source: &Value) -> Result<Value, ToolError> {
    if line.len() as i64 > MAX_SERIALIZED_RECORD_BYTES || !line.ends_with(b"\n") {
        return Err(refuse(
            "content-search package record is truncated or exceeds 64 KiB",
        ));
    }
    if Sha256::digest(line).as_slice() != digest {
        return Err(refuse("content-search package record integrity is invalid"));
    }
    let row: Value = serde_json::from_slice(line)
        .ok()
        .filter(Value::is_object)
        .ok_or_else(|| refuse("content-search package record is invalid"))?;
    if row.get("scan_uuid") != source.get("scan_uuid")
        || row.get("evidence_revision") != source.get("evidence_revision")
    {
        return Err(refuse(
            "content-search package record source identity disagrees",
        ));
    }
    Ok(row)
}

fn read_line(stream: &mut std::io::BufReader<std::fs::File>) -> Result<Vec<u8>, ToolError> {
    let mut line = Vec::new();
    stream
        .by_ref()
        .take(MAX_SERIALIZED_RECORD_BYTES as u64 + 1)
        .read_until(b'\n', &mut line)
        .map_err(io_error)?;
    if line.is_empty() {
        return Err(refuse("content-search package records are truncated"));
    }
    Ok(line)
}

/// `scan_content_search_page`: up to 100 indexed derived records of a package.
pub fn scan_content_search_page(args: &Value) -> Result<Value, ToolError> {
    const ACCEPTED: [&str; 5] = ["package", "offset", "limit", "status", "status_code"];
    let args = super::kwargs(args);
    let package = need_str(&args, "package", "scan-content-search-page", &ACCEPTED)?;
    let offset = opt(&args, "offset").map_or(Some(0), Value::as_i64);
    let limit = opt(&args, "limit").map_or(Some(100), Value::as_i64);
    let (Some(offset), Some(limit)) = (offset, limit) else {
        return Err(refuse(
            "offset must be nonnegative and limit must be 1..100",
        ));
    };
    if offset < 0 || !(1..=100).contains(&limit) {
        return Err(refuse(
            "offset must be nonnegative and limit must be 1..100",
        ));
    }
    let status = opt_str(&args, "status");
    if status.is_some_and(|s| !["matched", "not_matched", "unavailable"].contains(&s)) {
        return Err(refuse(
            "status must be matched, not_matched, unavailable, or omitted",
        ));
    }
    let status_code = match opt(&args, "status_code") {
        None => None,
        Some(v) => match v.as_i64() {
            Some(n) if (100..=599).contains(&n) => Some(n),
            _ => return Err(refuse("status_code must be an integer 100..599 or omitted")),
        },
    };
    let package = open_package(package)?;
    if status.is_some() || status_code.is_some() {
        return filtered_page(&package, offset, limit, status, status_code);
    }
    let format = package.manifest["format"].clone();
    if offset >= package.count {
        return Ok(json!({
            "ok": true,
            "format": format,
            "source": package.source,
            "offset": offset,
            "records": [],
            "has_more": false,
            "next_offset": offset,
        }));
    }
    let mut index = std::fs::File::open(&package.index_path).map_err(io_error)?;
    let mut stream =
        std::io::BufReader::new(std::fs::File::open(&package.records_path).map_err(io_error)?);
    let truncated = || refuse("content-search package index is truncated");
    let mut marker = [0u8; INDEX_ENTRY as usize];
    index
        .seek(SeekFrom::Start((offset * INDEX_ENTRY) as u64))
        .map_err(io_error)?;
    index.read_exact(&mut marker).map_err(|_| truncated())?;
    let start = u64::from_be_bytes(marker[..8].try_into().map_err(|_| truncated())?);
    stream.seek(SeekFrom::Start(start)).map_err(io_error)?;
    let mut rows = Vec::new();
    for _ in 0..limit {
        let line = read_line(&mut stream)?;
        let entry = offset + rows.len() as i64;
        index
            .seek(SeekFrom::Start((entry * INDEX_ENTRY + 8) as u64))
            .map_err(io_error)?;
        let mut expected = [0u8; 32];
        index.read_exact(&mut expected).map_err(|_| truncated())?;
        rows.push(checked_row(&line, &expected, &package.source)?);
        if offset + rows.len() as i64 >= package.count {
            break;
        }
    }
    let next_offset = offset + rows.len() as i64;
    Ok(json!({
        "ok": true,
        "format": format,
        "source": package.source,
        "offset": offset,
        "records": rows,
        "has_more": next_offset < package.count,
        "next_offset": next_offset,
    }))
}

/// A linear pass over every record (no matched index), re-verifying each.
fn filtered_page(
    package: &Package,
    offset: i64,
    limit: i64,
    status: Option<&str>,
    status_code: Option<i64>,
) -> Result<Value, ToolError> {
    let mut index =
        std::io::BufReader::new(std::fs::File::open(&package.index_path).map_err(io_error)?);
    let mut stream =
        std::io::BufReader::new(std::fs::File::open(&package.records_path).map_err(io_error)?);
    let (mut rows, mut seen, mut has_more) = (Vec::new(), 0i64, false);
    for _ in 0..package.count {
        let mut marker = [0u8; INDEX_ENTRY as usize];
        index
            .read_exact(&mut marker)
            .map_err(|_| refuse("content-search package index is truncated"))?;
        let start = u64::from_be_bytes(marker[..8].try_into().unwrap_or([0; 8]));
        stream.seek(SeekFrom::Start(start)).map_err(io_error)?;
        let line = read_line(&mut stream)?;
        let row = checked_row(&line, &marker[8..], &package.source)?;
        let wanted = status.is_none_or(|s| row.get("status").and_then(Value::as_str) == Some(s))
            && status_code
                .is_none_or(|c| row.get("status_code").and_then(Value::as_i64) == Some(c));
        if !wanted {
            continue;
        }
        if seen < offset {
            seen += 1;
            continue;
        }
        if rows.len() as i64 == limit {
            has_more = true;
            break;
        }
        rows.push(row);
        seen += 1;
    }
    Ok(json!({
        "ok": true,
        "format": package.manifest["format"],
        "source": package.manifest["source"],
        "filtered": true,
        "filter": {"status": status, "status_code": status_code},
        "offset": offset,
        "records": rows,
        "has_more": has_more,
        "next_offset": offset + rows.len() as i64,
    }))
}

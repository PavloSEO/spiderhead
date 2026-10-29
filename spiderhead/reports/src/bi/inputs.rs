//! Ingest and validate the offline inputs: saved scan databases, audit
//! documents, and provider evidence-join artifacts.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::{json, Map, Value};

use spiderhead_store::audit_v2::{audit_v2_path, AuditV2Reader};

use super::schema::{
    canonical_json, coverage_row, digest, page_url, redact_setting, BIExportError, Groups,
    ProviderInput, RunInput, JOIN_FORMAT, MAX_AUDIT_BYTES, MAX_CELL_BYTES, MAX_FINDINGS,
    MAX_LINK_OCCURRENCES, MAX_PAGES, MAX_PROJECTION_INDEX_BYTES, MAX_PROVIDER_JOIN_BYTES,
    MAX_PROVIDER_ROWS_PER_SOURCE, NORMALIZED_FORMAT,
};

fn err(message: impl Into<String>) -> BIExportError {
    BIExportError(message.into())
}

/// serde_json keeps the last value for duplicate keys; the loader must reject them.
/// Duplicate keys are detected during the JSON load.
fn load_json_bytes(raw: &[u8], label: &str) -> Result<Value, BIExportError> {
    // Strip a leading byte-order mark before parsing.
    let text = if raw.starts_with(b"\xef\xbb\xbf") {
        String::from_utf8_lossy(&raw[3..]).into_owned()
    } else {
        match std::str::from_utf8(raw) {
            Ok(s) => s.to_owned(),
            Err(e) => return Err(err(format!("{label} is not valid UTF-8 JSON: {e}"))),
        }
    };
    let value: Value = serde_json::from_str(&text)
        .map_err(|e| err(format!("{label} is not valid UTF-8 JSON: {e}")))?;
    if !value.is_object() {
        return Err(err(format!("{label} must be a JSON object")));
    }
    Ok(value)
}

/// Bounded, change-checked JSON file read.
pub fn read_json_path(
    path_value: &Value,
    label: &str,
    max_bytes: u64,
) -> Result<(Value, Vec<u8>), BIExportError> {
    let path_str = match path_value {
        Value::String(s) if !s.trim().is_empty() => s.clone(),
        _ => return Err(err(format!("{label} must be a non-empty local file path"))),
    };
    let path = Path::new(&path_str);
    let meta_before = fs::symlink_metadata(path)
        .map_err(|e| err(format!("cannot read {label}: {}", io_message(&e))))?;
    if meta_before.is_symlink() || !meta_before.is_file() {
        return Err(err(format!("{label} must be a regular non-symlink file")));
    }
    if meta_before.len() > max_bytes {
        return Err(err(format!(
            "{label} exceeds the {max_bytes}-byte input bound"
        )));
    }
    let raw =
        fs::read(path).map_err(|e| err(format!("cannot read {label}: {}", io_message(&e))))?;
    let meta_after = fs::symlink_metadata(path)
        .map_err(|e| err(format!("cannot read {label}: {}", io_message(&e))))?;
    if raw.len() as u64 > max_bytes {
        return Err(err(format!(
            "{label} exceeds the {max_bytes}-byte input bound"
        )));
    }
    if meta_before.len() != meta_after.len()
        || meta_before.modified().ok() != meta_after.modified().ok()
        || raw.len() as u64 != meta_after.len()
    {
        return Err(err(format!("{label} changed while it was read")));
    }
    Ok((load_json_bytes(&raw, label)?, raw))
}

fn io_message(error: &std::io::Error) -> String {
    error.to_string()
}

/// Bounded, change-checked content hash of a file.
pub fn sha256_path(
    path: &Path,
    max_bytes: u64,
    label: &str,
) -> Result<(String, u64), BIExportError> {
    let meta_before = fs::symlink_metadata(path)
        .map_err(|e| err(format!("cannot hash {label}: {}", io_message(&e))))?;
    if meta_before.is_symlink() || !meta_before.is_file() {
        return Err(err(format!("{label} must be a regular non-symlink file")));
    }
    if meta_before.len() > max_bytes {
        return Err(err(format!(
            "{label} exceeds the {max_bytes}-byte input bound"
        )));
    }
    let raw =
        fs::read(path).map_err(|e| err(format!("cannot hash {label}: {}", io_message(&e))))?;
    if raw.len() as u64 > max_bytes {
        return Err(err(format!(
            "{label} exceeds the {max_bytes}-byte input bound"
        )));
    }
    let meta_after = fs::symlink_metadata(path)
        .map_err(|e| err(format!("cannot hash {label}: {}", io_message(&e))))?;
    if meta_before.len() != meta_after.len()
        || meta_before.modified().ok() != meta_after.modified().ok()
        || raw.len() as u64 != meta_after.len()
    {
        return Err(err(format!("{label} changed while it was read")));
    }
    Ok((digest(&raw), raw.len() as u64))
}

/// Detect and validate the audit document kind.
pub fn audit_kind(document: &Value) -> Result<&'static str, BIExportError> {
    let (kind, error) = crate::document::detect_kind(document);
    if let Some(error) = error {
        return Err(err(error));
    }
    let Some(kind) = kind else {
        return Err(err(
            "audit does not match a supported site-audit/1 or SF audit.json contract",
        ));
    };
    if kind == "sf-audit" {
        spiderhead_formats::audit_schema::validate_audit_document(document).map_err(|e| {
            err(format!(
                "audit failed the saved audit schema validation: {e}"
            ))
        })?;
    } else if document.get("schema").and_then(Value::as_str) != Some("spiderhead.site-audit/1")
        || !matches!(document.get("pages"), Some(Value::Array(_)))
        || !matches!(document.get("findings"), Some(Value::Array(_)))
        || !matches!(document.get("summary"), Some(Value::Object(_)))
    {
        return Err(err(
            "site audit has invalid or missing pages/findings/summary",
        ));
    }
    Ok(kind)
}

#[allow(clippy::too_many_arguments)]
fn cov(
    run_id: &str,
    dataset: &str,
    source_id: &str,
    population: &str,
    state: &str,
    reason: Option<Value>,
    source_rows: Option<i64>,
    exported_rows: Option<i64>,
    unavailable_rows: Option<i64>,
    details: Option<&Value>,
) -> Value {
    coverage_row(
        run_id,
        dataset,
        source_id,
        population,
        state,
        reason.as_ref(),
        source_rows.map(|n| json!(n)).as_ref(),
        exported_rows.map(|n| json!(n)).as_ref(),
        unavailable_rows.map(|n| json!(n)).as_ref(),
        details,
    )
}

/// Check coverage rows.
pub fn check_coverage_rows(run_id: &str, run: &Value, coverage: &Value) -> Vec<Value> {
    let mut rows = Vec::new();
    for (key, label) in [
        ("checks_skipped", "skipped"),
        ("checks_disabled", "disabled"),
    ] {
        let Some(entries) = run.get(key).and_then(Value::as_array) else {
            continue;
        };
        for (index, item) in entries.iter().enumerate() {
            let Some(obj) = item.as_object() else {
                continue;
            };
            let check_id = obj
                .get("id")
                .or_else(|| obj.get("check"))
                .map(crate::py::str_value)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("{label}:{index}"));
            rows.push(json!({
                "run_id": run_id,
                "dataset": "checks",
                "evidence_source_id": check_id,
                "population": "check",
                "state": label,
                "reason": obj.get("reason").cloned().unwrap_or(Value::Null),
                "source_rows": 1,
                "exported_rows": 0,
                "unavailable_rows": if label == "skipped" { 1 } else { 0 },
                "details_json": item,
            }));
        }
    }
    if let Some(silent) = coverage.get("checks_silent_ids").and_then(Value::as_array) {
        for check_id in silent {
            if let Some(id) = check_id.as_str() {
                rows.push(json!({
                    "run_id": run_id,
                    "dataset": "checks",
                    "evidence_source_id": id,
                    "population": "check",
                    "state": "silent",
                    "reason": "source audit recorded a silent completed check",
                    "source_rows": 1,
                    "exported_rows": 0,
                    "unavailable_rows": 0,
                    "details_json": {"check_id": id},
                }));
            }
        }
    }
    rows
}

/// A saved audit document becomes the run input.
pub fn audit_source(
    document: &Value,
    raw: &[u8],
    source_name: Option<String>,
) -> Result<RunInput, BIExportError> {
    let kind = audit_kind(document)?;
    let run = if kind == "sf-audit" {
        document.get("run").filter(|v| v.is_object()).cloned()
    } else {
        None
    }
    .unwrap_or_else(|| json!({}));
    let pages = document.get("pages").and_then(Value::as_array);
    let findings = if kind == "sf-audit" {
        document.get("issues")
    } else {
        document.get("findings")
    }
    .and_then(Value::as_array);
    let groups = document
        .get("groups")
        .filter(|_| kind == "sf-audit")
        .cloned()
        .unwrap_or(Value::Null);
    let (pages, findings) = match (pages, findings) {
        (Some(p), Some(f)) => (p.clone(), f.clone()),
        _ => return Err(err("validated audit pages and findings must be lists")),
    };
    if pages.len() > MAX_PAGES {
        return Err(err(format!(
            "audit page rows exceed the {MAX_PAGES}-row source bound"
        )));
    }
    if findings.len() > MAX_FINDINGS {
        return Err(err(format!(
            "audit findings exceed the {MAX_FINDINGS}-row source bound"
        )));
    }
    if pages.iter().any(|p| !p.is_object()) {
        return Err(err("audit contains a non-object page row"));
    }
    let page_urls: Vec<Option<String>> = pages.iter().map(page_url).collect();
    {
        let keys: std::collections::HashSet<_> = page_urls.iter().collect();
        if keys.len() != page_urls.len() {
            return Err(err(
                "source audit has duplicate raw page URLs; page rows cannot be keyed uniquely",
            ));
        }
    }
    if findings.iter().any(|f| !f.is_object()) {
        return Err(err("audit contains a non-object finding row"));
    }
    let raw_sha = digest(raw);
    let source_schema = document
        .get("schema_version")
        .or_else(|| document.get("schema"))
        .cloned()
        .unwrap_or(Value::Null);
    let run_id = run
        .get("scan_uuid")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("audit:{}", &raw_sha[..24]));
    let partial = run.get("crawl_partial").and_then(Value::as_bool);
    let crawl_valid = run.get("crawl_valid").and_then(Value::as_bool);
    let (crawl_state, crawl_reason): (&str, Option<String>) = if crawl_valid == Some(false) {
        (
            "failed",
            Some(
                run.get("crawl_invalid_reason")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "audit marks crawl invalid".into()),
            ),
        )
    } else if partial == Some(true) {
        (
            "partial",
            Some(
                run.get("crawl_finish_reason")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "source audit is partial".into()),
            ),
        )
    } else if run.get("input_mode").and_then(Value::as_str) == Some("crawl") {
        ("complete", None)
    } else {
        (
            "unknown",
            Some("audit source does not establish a completed native crawl".into()),
        )
    };
    let audit_summary = document
        .get("summary")
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let check_coverage = audit_summary
        .get("check_coverage")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let mut coverage_rows = check_coverage_rows(&run_id, &run, &check_coverage);
    let check_skips: Map<String, Value> = run
        .get("checks_skipped")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let obj = item.as_object()?;
                    let id = obj.get("id")?;
                    Some((
                        crate::py::str_value(id),
                        Value::String(
                            obj.get("reason")
                                .map(crate::py::str_value)
                                .filter(|s| !s.is_empty())
                                .unwrap_or_else(|| "source check was skipped".into()),
                        ),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    if kind == "site-audit" {
        if let Some(tools) = audit_summary.get("tools_run").and_then(Value::as_array) {
            for tool in tools {
                if let Some(tool) = tool.as_str() {
                    coverage_rows.push(cov(
                        &run_id,
                        "tools",
                        tool,
                        "tool",
                        "completed",
                        None,
                        Some(1),
                        Some(1),
                        Some(0),
                        Some(&json!({"tool": tool})),
                    ));
                }
            }
        }
        if let Some(failures) = audit_summary.get("tools_failed").and_then(Value::as_array) {
            for failure in failures {
                if let Some(obj) = failure.as_object() {
                    let tool = obj
                        .get("tool")
                        .map(crate::py::str_value)
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "unknown".into());
                    let reason = obj
                        .get("error")
                        .map(crate::py::str_value)
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "site audit tool failed".into());
                    coverage_rows.push(cov(
                        &run_id,
                        "tools",
                        &tool,
                        "tool",
                        "failed",
                        Some(json!(reason)),
                        Some(1),
                        Some(0),
                        Some(1),
                        Some(failure),
                    ));
                }
            }
        }
        if let Some(failures) = audit_summary
            .get("page_tools_failed")
            .and_then(Value::as_array)
        {
            for failure in failures {
                if let Some(obj) = failure.as_object() {
                    let tool = obj
                        .get("tool")
                        .map(crate::py::str_value)
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "unknown".into());
                    let failed_pages = obj
                        .get("failed_pages")
                        .and_then(Value::as_i64)
                        .filter(|n| *n >= 0);
                    let pages_checked = obj
                        .get("pages_checked")
                        .and_then(Value::as_i64)
                        .filter(|n| *n >= 0);
                    let succeeded = match (pages_checked, failed_pages) {
                        (Some(c), Some(f)) => Some(c - f),
                        _ => None,
                    };
                    let state = if failed_pages.is_some()
                        && failed_pages == pages_checked
                        && pages_checked.is_some()
                    {
                        "failed"
                    } else {
                        "partial"
                    };
                    coverage_rows.push(cov(
                        &run_id,
                        "tools",
                        &tool,
                        "page_tool",
                        state,
                        Some(json!("page tool failed for one or more URLs")),
                        pages_checked,
                        succeeded,
                        failed_pages,
                        Some(failure),
                    ));
                }
            }
        }
    }
    let finished_at = run
        .get("finished_at")
        .cloned()
        .filter(|v| !v.is_null())
        .or_else(|| run.get("generated_at").cloned())
        .or_else(|| document.get("generated_at").cloned())
        .unwrap_or(Value::Null);
    let target_scope = run
        .get("source")
        .cloned()
        .filter(|v| !v.is_null())
        .or_else(|| run.get("project").cloned())
        .or_else(|| document.get("url").cloned())
        .or_else(|| document.get("domain").cloned())
        .unwrap_or(Value::Null);
    let crawl_settings = match run.get("crawl_config") {
        Some(v) if !v.is_null() => redact_setting(v, ""),
        _ => Value::Null,
    };
    let mut metadata = json!({
        "run_id": run_id,
        "source_kind": kind,
        "source_schema": source_schema,
        "source_name": source_name,
        "source_sha256": raw_sha,
        "source_bytes": raw.len() as u64,
        "started_at": run.get("started_at").cloned().unwrap_or(Value::Null),
        "finished_at": finished_at,
        "target_scope": target_scope,
        "crawl_state": crawl_state,
        "crawl_reason": crawl_reason.map(Value::String).unwrap_or(Value::Null),
        "crawl_settings": crawl_settings,
        "input_mode": run.get("input_mode").cloned().unwrap_or(Value::Null),
        "summary": audit_summary,
        "check_skips": check_skips,
        "audit_sha256": raw_sha,
    });
    let unavailable_links = "audit document does not retain a complete link-occurrence inventory";
    let groups_list = match &groups {
        Value::Null => Vec::new(),
        Value::Array(items) => {
            if items.iter().any(|g| !g.is_object()) {
                return Err(err("audit groups must be a list of objects"));
            }
            items.clone()
        }
        _ => return Err(err("audit groups must be a list of objects")),
    };
    metadata
        .as_object_mut()
        .map(|m| m.insert("audit_sha256".into(), json!(raw_sha)));
    let pages_clone = pages.clone();
    let findings_clone = findings.clone();
    Ok(RunInput {
        run_id,
        source_kind: kind.into(),
        source_schema: crate::py::str_value(&source_schema).pipe(|s| {
            if s.is_empty() {
                "unknown".into()
            } else {
                s
            }
        }),
        source_name,
        source_sha256: raw_sha.clone(),
        source_bytes: raw.len() as u64,
        run_metadata: metadata.as_object().cloned().unwrap_or_default(),
        pages_factory: Box::new(move || {
            let pages = pages_clone.clone();
            Box::new(pages.into_iter())
        }),
        page_count: page_urls_len(&pages),
        findings_factory: Box::new(move || {
            let findings = findings_clone.clone();
            Box::new(findings.into_iter())
        }),
        finding_count: page_urls_len(&findings),
        groups: Groups::List(groups_list),
        links_factory: Box::new(|| Box::new(std::iter::empty())),
        links_source_state: "unavailable".into(),
        links_source_reason: Some(unavailable_links.into()),
        coverage_rows,
        link_count: None,
        audit_sha256: Some(raw_sha.clone()),
    })
}

fn page_urls_len(items: &[Value]) -> usize {
    items.len()
}

trait Pipe: Sized {
    fn pipe<R>(self, f: impl FnOnce(Self) -> R) -> R {
        f(self)
    }
}
impl Pipe for String {}

/// Read an audit input: inline document or bounded JSON path.
pub fn read_audit_input(value: &Value) -> Result<(Value, Vec<u8>, Option<String>), BIExportError> {
    if value.is_object() {
        let raw = canonical_json(value).map_err(|e| err(e.0))?.into_bytes();
        if raw.len() as u64 > MAX_AUDIT_BYTES {
            return Err(err(format!(
                "inline audit exceeds the {MAX_AUDIT_BYTES}-byte bound"
            )));
        }
        return Ok((value.clone(), raw, None));
    }
    let raw_value = value.as_str().unwrap_or_default().trim().to_owned();
    if raw_value.is_empty() {
        return Err(err("audit must be an audit document or local path"));
    }
    if spiderhead_store::inputs::is_sqlite_input(&json!(raw_value)) {
        return Err(err(
            "SQLite scan paths must be supplied with --scan, not --audit",
        ));
    }
    let (document, raw) =
        read_json_path(&Value::String(raw_value.clone()), "audit", MAX_AUDIT_BYTES)?;
    Ok((
        document,
        raw,
        Path::new(&raw_value)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned()),
    ))
}

/// A validated scan SQLite becomes the run input.
/// The settings manifest comes from the local `crawl_settings` port.
/// Provider join readers are a named gap: their code lives in the data sources crate,
/// outside this crate, so store joins are not read here.
pub fn scan_source(
    path_value: &str,
    con: &Connection,
    max_scan_bytes: u64,
    index_parent: &Path,
) -> Result<RunInput, BIExportError> {
    let _ = index_parent;
    let path = Path::new(path_value);
    let (scan_sha, scan_bytes) = sha256_path(path, max_scan_bytes, "scan")?;
    let scan: Map<String, Value> = con
        .query_row("SELECT * FROM scan WHERE singleton=1", [], |row| {
            Ok(row_to_map(row))
        })
        .map_err(|_| err("validated scan is missing its run manifest"))?;
    let audit_row: Option<(String, String)> = con
        .query_row(
            "SELECT document_json,sha256 FROM audit WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok();
    let audit_reader = if audit_v2_path(path).exists() {
        Some(
            AuditV2Reader::new(path, true, true, None)
                .map_err(|e| err(format!("audit.v2 reader failed: {e}")))?,
        )
    } else {
        None
    };
    let audit: Option<Value> = if let Some(reader) = &audit_reader {
        Some(reader.header.clone())
    } else {
        audit_row
            .as_ref()
            .and_then(|(doc, _)| serde_json::from_str(doc).ok())
    };
    let decode = |key: &str| -> Result<Value, BIExportError> {
        scan.get(key)
            .and_then(Value::as_str)
            .map(|s| {
                serde_json::from_str(s)
                    .map_err(|e| err(format!("validated scan metadata could not be decoded: {e}")))
            })
            .unwrap_or_else(|| {
                Err(err(format!(
                    "validated scan metadata could not be decoded: missing {key}"
                )))
            })
    };
    let capabilities = decode("capabilities_json")?;
    let limitations = decode("limitations_json")?;
    let settings_json = if matches!(
        scan.get("source_kind").and_then(Value::as_str),
        Some("native") | Some("reanalysis")
    ) {
        Value::Object(
            spiderhead_store::scan_schema::read_effective_config(con)
                .map_err(|e| err(format!("validated scan metadata could not be decoded: {e}")))?,
        )
    } else {
        decode("config_json")?
    };
    let retention = decode("retention_json")?;
    if !capabilities.is_object() || !settings_json.is_object() {
        return Err(err("validated scan capabilities or settings are malformed"));
    }
    let page_count: i64 = con
        .query_row("SELECT COUNT(*) FROM pages", [], |r| r.get(0))
        .map_err(|_| err("validated scan is missing its run manifest"))?;
    if page_count < 0 || page_count as usize > MAX_PAGES {
        return Err(err(format!(
            "scan pages exceed the {MAX_PAGES}-row source bound"
        )));
    }
    let dup: Option<i64> = con
        .query_row(
            "SELECT 1 FROM pages p JOIN urls u USING(url_id) GROUP BY u.url \
             HAVING COUNT(*) > 1 LIMIT 1",
            [],
            |r| r.get(0),
        )
        .ok();
    if dup.is_some() {
        return Err(err(
            "retained scan has duplicate raw page URLs; page rows cannot be keyed uniquely",
        ));
    }
    let findings = audit
        .as_ref()
        .and_then(|a| a.get("issues"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let groups_value = audit
        .as_ref()
        .and_then(|a| a.get("groups"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let (findings_vec, finding_count, groups, audit_page_rows): (
        Vec<Value>,
        usize,
        Groups,
        Vec<Value>,
    ) = if let Some(reader) = &audit_reader {
        let count = reader.count("/issues").unwrap_or_default().max(0) as usize;
        let groups = if reader.collections.contains_key("/groups") {
            let total = reader.count("/groups").unwrap_or_default().max(0) as usize;
            // Each pass reopens the reader from the kept scan path; reopening is cheap.
            let scan_path = reader.scan_path.clone();
            Groups::Stream {
                factory: Box::new(move || {
                    match AuditV2Reader::new(&scan_path, true, true, None)
                        .and_then(|r| r.iter_collection("/groups").map(|rows| (r, rows)))
                    {
                        // Swap the stored group marker for a lazy member sequence, one group at a time.
                        Ok((reader, rows)) => Box::new(rows.into_iter().map(move |mut row| {
                            if let Some(members) = reader.group_members(&row) {
                                if let Ok(urls) = members.to_vec() {
                                    row["urls"] = Value::Array(urls);
                                }
                            }
                            row
                        })),
                        Err(_) => Box::new(std::iter::empty()),
                    }
                }),
                count: total,
            }
        } else {
            Groups::List(Vec::new())
        };
        let pages_rows = if reader.collections.contains_key("/pages") {
            reader.iter_collection("/pages").unwrap_or_default()
        } else {
            Vec::new()
        };
        (Vec::new(), count, groups, pages_rows)
    } else {
        let audit_pages = audit
            .as_ref()
            .and_then(|a| a.get("pages"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        (
            findings.clone(),
            findings.len(),
            Groups::List(groups_value),
            audit_pages,
        )
    };
    // Audit page overlay: URL to indexability pair (non-reader path only).
    let audit_pages_overlay: Option<BTreeMap<String, (Value, Value)>> = if audit_reader.is_none() {
        Some(
            audit_page_rows
                .iter()
                .filter_map(|page| {
                    let url = page.get("url").and_then(Value::as_str)?;
                    Some((
                        url.to_owned(),
                        (
                            page.get("indexability").cloned().unwrap_or(Value::Null),
                            page.get("indexability_status")
                                .cloned()
                                .unwrap_or(Value::Null),
                        ),
                    ))
                })
                .collect(),
        )
    } else {
        None
    };
    // Audit v2 overlay: disk-backed URL index, bounded in memory.
    let overlay_path: Option<PathBuf> = if audit_reader.is_some()
        && audit_reader
            .as_ref()
            .map(|r| r.collections.contains_key("/pages"))
            .unwrap_or(false)
    {
        let dir = index_parent.join(format!(".seohead-bi-audit-pages-{}", std::process::id()));
        fs::create_dir_all(&dir).ok();
        let overlay_path = dir.join("pages.sqlite");
        let overlay = Connection::open(&overlay_path)
            .map_err(|e| err(format!("audit page overlay failed: {e}")))?;
        overlay
            .execute_batch("PRAGMA cache_size=-2048; PRAGMA journal_mode=OFF;")
            .map_err(|e| err(format!("audit page overlay failed: {e}")))?;
        overlay
            .pragma_update(None, "max_page_count", MAX_PROJECTION_INDEX_BYTES / 4096)
            .ok();
        overlay
            .execute(
                "CREATE TABLE overlays (url TEXT PRIMARY KEY, indexability_json TEXT, \
                 indexability_status_json TEXT)",
                [],
            )
            .map_err(|e| err(format!("audit page overlay failed: {e}")))?;
        for overlay_row in &audit_page_rows {
            let Some(url) = overlay_row.get("url").and_then(Value::as_str) else {
                if overlay_row.is_object() {
                    return Err(err("saved audit page lacks a URL for streaming projection"));
                }
                return Err(err("saved audit page lacks a URL for streaming projection"));
            };
            let indexability =
                canonical_json(overlay_row.get("indexability").unwrap_or(&Value::Null))
                    .map_err(|e| err(e.0))?;
            let status = canonical_json(
                overlay_row
                    .get("indexability_status")
                    .unwrap_or(&Value::Null),
            )
            .map_err(|e| err(e.0))?;
            if url.len() as u64 > MAX_CELL_BYTES
                || indexability.len() as u64 > MAX_CELL_BYTES
                || status.len() as u64 > MAX_CELL_BYTES
            {
                return Err(err("one audit page overlay exceeds the BI cell bound"));
            }
            overlay
                .execute(
                    "INSERT INTO overlays VALUES (?,?,?)",
                    rusqlite::params![url, indexability, status],
                )
                .map_err(|_| err("saved audit has duplicate URL overlays"))?;
        }
        Some(overlay_path)
    } else {
        None
    };
    if finding_count > MAX_FINDINGS {
        return Err(err(format!(
            "scan findings exceed the {MAX_FINDINGS}-row source bound"
        )));
    }
    let link_count: i64 = con
        .query_row("SELECT COUNT(*) FROM links", [], |r| r.get(0))
        .map_err(|_| err("validated scan is missing its run manifest"))?;
    if link_count < 0 || link_count as usize > MAX_LINK_OCCURRENCES {
        return Err(err(format!(
            "scan link occurrences exceed the {MAX_LINK_OCCURRENCES}-row source bound"
        )));
    }
    let link_capability = capabilities.get("links");
    let (mut links_state, mut links_reason) =
        if let Some(cap) = link_capability.and_then(Value::as_object) {
            (
                cap.get("state")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned(),
                cap.get("reason").and_then(Value::as_str).map(str::to_owned),
            )
        } else {
            (
                "unknown".to_owned(),
                Some("scan does not declare link evidence capability".to_owned()),
            )
        };
    if links_state != "complete" && links_state != "partial" {
        links_state = "unavailable".into();
        if links_reason.is_none() {
            links_reason = Some("scan link occurrence capability is unavailable".into());
        }
    } else if crate::py::truthy(scan.get("crawl_partial")) && links_state == "complete" {
        links_state = "partial".into();
        if links_reason.is_none() {
            links_reason = Some("the source crawl is partial".into());
        }
    }
    let lifecycle = scan.get("lifecycle").and_then(Value::as_str);
    let (crawl_state, crawl_reason): (String, Option<String>) = if lifecycle != Some("finished") {
        if lifecycle == Some("failed") {
            (
                "failed".into(),
                scan.get("finish_reason")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| lifecycle.map(str::to_owned)),
            )
        } else {
            (
                "partial".into(),
                scan.get("finish_reason")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| lifecycle.map(str::to_owned)),
            )
        }
    } else if crate::py::truthy(scan.get("crawl_partial")) {
        (
            "partial".into(),
            Some(
                scan.get("finish_reason")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| "scan is marked partial".into()),
            ),
        )
    } else {
        ("complete".into(), None)
    };
    let safe_settings = crate::crawl_settings::manifest(&settings_json).unwrap_or_else(
        |e| json!({"state": "unavailable", "reason": format!("settings projection failed: {e}")}),
    );
    let source_check_coverage = audit
        .as_ref()
        .and_then(|a| a.get("summary"))
        .and_then(|s| s.get("check_coverage"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let scan_uuid = crate::py::str_value(scan.get("scan_uuid").unwrap_or(&Value::Null));
    let mut coverage_rows = audit
        .as_ref()
        .map(|a| {
            let run = a
                .get("run")
                .filter(|v| v.is_object())
                .cloned()
                .unwrap_or_else(|| json!({}));
            check_coverage_rows(&scan_uuid, &run, &source_check_coverage)
        })
        .unwrap_or_default();
    if audit.is_none() {
        coverage_rows.push(cov(
            &scan_uuid,
            "checks",
            "audit",
            "check_coverage",
            "unavailable",
            Some(json!(
                "scan has no current saved audit; findings and check coverage were not produced"
            )),
            None,
            Some(0),
            None,
            None,
        ));
    }
    let audit_sha256 = audit_reader
        .as_ref()
        .map(|r| r.sha256.clone())
        .or_else(|| audit_row.as_ref().map(|(_, s)| s.clone()));
    let source_name = path.file_name().map(|n| n.to_string_lossy().into_owned());
    let metadata = json!({
        "run_id": scan_uuid,
        "source_kind": "scan",
        "source_schema": scan.get("format_version").cloned().unwrap_or(Value::Null),
        "source_name": source_name,
        "source_sha256": scan_sha,
        "source_bytes": scan_bytes,
        "scan_uuid": scan_uuid,
        "scan_source_kind": scan.get("source_kind").cloned().unwrap_or(Value::Null),
        "parent_scan_uuid": scan.get("parent_scan_uuid").cloned().unwrap_or(Value::Null),
        "started_at": scan.get("created_at").cloned().unwrap_or(Value::Null),
        "finished_at": scan.get("finished_at").cloned().unwrap_or(Value::Null),
        "target_scope": scan.get("start_url").cloned().unwrap_or(Value::Null),
        "crawl_state": crawl_state,
        "crawl_reason": crawl_reason.map(Value::String).unwrap_or(Value::Null),
        "lifecycle": scan.get("lifecycle").cloned().unwrap_or(Value::Null),
        "finish_reason": scan.get("finish_reason").cloned().unwrap_or(Value::Null),
        "crawl_settings": safe_settings,
        "limitations": limitations,
        "capabilities": capabilities,
        "retention": retention,
        "evidence_revision": scan.get("evidence_revision").cloned().unwrap_or(Value::Null),
        "audit_available": audit.is_some(),
        "audit_sha256": audit_sha256.clone().map(Value::String).unwrap_or(Value::Null),
    });
    // Factories reopen their own read-only connection per pass;
    // a second read-only open is equivalent.
    let db_path = path.to_path_buf();
    let db_path_pages = db_path.clone();
    let overlay_pages = overlay_path.clone();
    let audit_pages_overlay = std::sync::Arc::new(audit_pages_overlay);
    let overlay_lookup = audit_pages_overlay.clone();
    let pages_factory = Box::new(move || {
        let con = spiderhead_core::sqlite::open_readonly(
            &db_path_pages,
            std::time::Duration::from_secs(5),
            &["PRAGMA query_only=ON"],
        );
        let overlay_con = overlay_pages
            .as_ref()
            .and_then(|p| Connection::open(p).ok());
        let lookup = overlay_lookup.clone();
        let iter: Vec<Value> = match con {
            Ok(con) => {
                let stmt = con.prepare(
                    "SELECT p.*,u.url FROM pages p JOIN urls u USING(url_id) \
                     ORDER BY p.page_ordinal",
                );
                let mut rows = Vec::new();
                if let Ok(mut stmt) = stmt {
                    let collected = stmt
                        .query_map([], |row| Ok(row_to_map(row)))
                        .map(|m| m.flatten().collect::<Vec<_>>())
                        .unwrap_or_default();
                    for map in collected {
                        let mut page = Value::Object(map);
                        if let Some(oc) = &overlay_con {
                            if let Ok((ia, is)) = oc.query_row(
                                "SELECT indexability_json,indexability_status_json \
                                 FROM overlays WHERE url=?",
                                rusqlite::params![page
                                    .get("url")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()],
                                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                            ) {
                                let ia: Value = serde_json::from_str(&ia).unwrap_or(Value::Null);
                                let is: Value = serde_json::from_str(&is).unwrap_or(Value::Null);
                                page.as_object_mut().map(|m| {
                                    m.insert(
                                        "_bi_audit_page".into(),
                                        json!({
                                            "indexability": ia,
                                            "indexability_status": is,
                                        }),
                                    )
                                });
                            }
                        } else if let Some(map) = lookup.as_ref() {
                            if let Some(url) = page.get("url").and_then(Value::as_str) {
                                if let Some((ia, is)) = map.get(url) {
                                    page.as_object_mut().map(|m| {
                                        m.insert(
                                            "_bi_audit_page".into(),
                                            json!({
                                                "indexability": ia,
                                                "indexability_status": is,
                                            }),
                                        )
                                    });
                                }
                            }
                        }
                        rows.push(page);
                    }
                }
                rows
            }
            Err(_) => Vec::new(),
        };
        Box::new(iter.into_iter()) as Box<dyn Iterator<Item = Value> + Send>
    });
    let db_path_links = db_path;
    let links_factory = Box::new(move || {
        let con = spiderhead_core::sqlite::open_readonly(
            &db_path_links,
            std::time::Duration::from_secs(5),
            &["PRAGMA query_only=ON"],
        );
        let iter: Vec<Value> = match con {
            Ok(con) => con
                .prepare(
                    "SELECT l.*,source.url AS source_url,destination.url AS destination_url \
                     FROM links l \
                     JOIN urls source ON source.url_id=l.source_url_id \
                     JOIN urls destination ON destination.url_id=l.destination_url_id \
                     ORDER BY l.link_id",
                )
                .and_then(|mut stmt| {
                    stmt.query_map([], |row| Ok(row_to_map(row)))
                        .map(|m| m.flatten().collect::<Vec<Map<String, Value>>>())
                })
                .unwrap_or_default()
                .into_iter()
                .map(Value::Object)
                .collect(),
            Err(_) => Vec::new(),
        };
        Box::new(iter.into_iter()) as Box<dyn Iterator<Item = Value> + Send>
    });
    let findings_factory = if let Some(reader) = &audit_reader {
        let scan_path = reader.scan_path.clone();
        Box::new(move || {
            match AuditV2Reader::new(&scan_path, true, true, None)
                .and_then(|r| r.iter_collection("/issues"))
            {
                Ok(rows) => Box::new(rows.into_iter()) as Box<dyn Iterator<Item = Value> + Send>,
                Err(_) => Box::new(std::iter::empty()),
            }
        }) as Box<dyn FnMut() -> Box<dyn Iterator<Item = Value> + Send> + Send>
    } else {
        let rows = findings_vec;
        Box::new(move || {
            let rows = rows.clone();
            Box::new(rows.into_iter()) as Box<dyn Iterator<Item = Value> + Send>
        })
    };
    Ok(RunInput {
        run_id: scan_uuid,
        source_kind: "scan".into(),
        source_schema: crate::py::str_value(scan.get("format_version").unwrap_or(&Value::Null))
            .pipe(|s| if s.is_empty() { "scan.v1".into() } else { s }),
        source_name,
        source_sha256: scan_sha,
        source_bytes: scan_bytes,
        run_metadata: metadata.as_object().cloned().unwrap_or_default(),
        pages_factory,
        page_count: page_count as usize,
        findings_factory,
        finding_count,
        groups,
        links_factory,
        links_source_state: links_state.clone(),
        links_source_reason: links_reason,
        coverage_rows,
        link_count: if ["complete", "partial"].contains(&links_state.as_str()) {
            Some(link_count as usize)
        } else {
            None
        },
        audit_sha256,
    })
}

/// One sqlite row as a JSON object (column-name keyed).
fn row_to_map(row: &rusqlite::Row<'_>) -> Map<String, Value> {
    let mut map = Map::new();
    for (index, name) in row.as_ref().column_names().iter().enumerate() {
        let value = match row.get_ref(index) {
            Ok(rusqlite::types::ValueRef::Null) => Value::Null,
            Ok(rusqlite::types::ValueRef::Integer(n)) => json!(n),
            Ok(rusqlite::types::ValueRef::Real(f)) => json!(f),
            Ok(rusqlite::types::ValueRef::Text(t)) => {
                Value::String(String::from_utf8_lossy(t).into_owned())
            }
            Ok(rusqlite::types::ValueRef::Blob(b)) => Value::String(format!("{:?}", b)),
            Err(_) => Value::Null,
        };
        map.insert((*name).to_owned(), value);
    }
    map
}

/// A store artifact or a bounded JSON document.
///
/// Evidence-join store files are not supported: they are rejected with the unsupported-format
/// error, the same as any file that cannot be read as JSON.
pub fn provider_file(path_value: &str) -> Result<ProviderInput, BIExportError> {
    let path = Path::new(path_value);
    // Probe the SQLite header; a match is rejected as a named gap, never mis-read.
    if path.extension().is_some_and(|e| e == "sqlite" || e == "db")
        || std::fs::read(path)
            .map(|b| b.starts_with(b"SQLite format 3"))
            .unwrap_or(false)
    {
        return Err(err(
            "provider input must be seohead.normalized-evidence.v1, \
             spiderhead.evidence-join.v1, or a saved evidence-join artifact",
        ));
    }
    let (document, raw) = read_json_path(
        &Value::String(path_value.to_owned()),
        "provider join",
        MAX_PROVIDER_JOIN_BYTES,
    )?;
    provider_document(
        &document,
        &raw,
        path.file_name().map(|n| n.to_string_lossy().into_owned()),
    )
}

/// Provider document input.
pub fn provider_document(
    document: &Value,
    raw: &[u8],
    name: Option<String>,
) -> Result<ProviderInput, BIExportError> {
    let source_id = digest(raw);
    let mut compatibility = None;
    let join;
    let normalized;
    let format = document.get("format").and_then(Value::as_str);
    if format == Some(NORMALIZED_FORMAT) {
        normalized = Some(document.clone());
        join = None;
    } else if format == Some(JOIN_FORMAT) {
        join = Some(document.clone());
        normalized = None;
    } else if document
        .get("join")
        .and_then(|j| j.get("format"))
        .and_then(Value::as_str)
        == Some(JOIN_FORMAT)
    {
        join = document.get("join").cloned();
        normalized = None;
        compatibility = document.get("compatibility").cloned();
    } else if spiderhead_core::formats::matches(format, "spiderhead.provider-join.v1") {
        let summary = document.get("join").and_then(|j| j.get("summary"));
        let count = summary.and_then(|s| s.get("rows")).and_then(Value::as_i64);
        let Some(count) = count.filter(|n| *n >= 0) else {
            return Err(err(
                "legacy provider-join artifact has invalid source row counts",
            ));
        };
        return Ok(ProviderInput {
            source_id,
            name,
            sha256: digest(raw),
            byte_count: raw.len() as u64,
            join: None,
            compatibility: None,
            normalized: None,
            error_state: Some("unsupported".into()),
            error_reason: Some(
                "legacy provider-join.v1 does not declare typed metric names, units, \
                 periods, or per-value availability; no metric rows were inferred"
                    .into(),
            ),
            reported_rows: Some(count),
        });
    } else {
        return Err(err(
            "provider input must be seohead.normalized-evidence.v1, \
             spiderhead.evidence-join.v1, or a saved evidence-join artifact",
        ));
    }
    if let Some(doc) = &normalized {
        let rows = doc.get("rows").and_then(Value::as_array);
        match rows {
            Some(rows) if rows.len() <= MAX_PROVIDER_ROWS_PER_SOURCE => {}
            _ => {
                return Err(err(format!(
                    "normalized provider rows exceed the \
                     {MAX_PROVIDER_ROWS_PER_SOURCE}-row source bound"
                )))
            }
        }
        let rows_len = rows.map(|r| r.len()).unwrap_or_default();
        if !doc.get("mapping").is_some_and(Value::is_object) {
            return Err(err(
                "normalized provider evidence lacks its mapping/provenance",
            ));
        }
        let summary_ok = doc
            .get("summary")
            .and_then(|s| s.get("rows"))
            .and_then(Value::as_i64)
            == Some(rows_len as i64);
        if !summary_ok {
            return Err(err(
                "normalized provider summary does not conserve its source row count",
            ));
        }
        let metrics = doc
            .get("mapping")
            .and_then(|m| m.get("metrics"))
            .and_then(Value::as_array);
        if !metrics.is_some_and(|m| !m.is_empty()) {
            return Err(err(
                "normalized provider evidence has no declared numeric metrics",
            ));
        }
    }
    if let Some(join_doc) = &join {
        validate_join(join_doc)?;
    }
    Ok(ProviderInput {
        source_id,
        name,
        sha256: digest(raw),
        byte_count: raw.len() as u64,
        join,
        compatibility,
        normalized,
        error_state: None,
        error_reason: None,
        reported_rows: None,
    })
}

/// Validate a saved join document.
pub fn validate_join(join: &Value) -> Result<(), BIExportError> {
    if join.get("format").and_then(Value::as_str) != Some(JOIN_FORMAT) {
        return Err(err(format!("provider join format must be {JOIN_FORMAT}")));
    }
    for name in [
        "matched",
        "crawl_only",
        "external_only",
        "unkeyable_pages",
        "unkeyable_rows",
    ] {
        if !join.get(name).is_some_and(Value::is_array) {
            return Err(err(format!(
                "provider join {name} population must be a list"
            )));
        }
    }
    let evidence = join.get("evidence");
    let summary = join.get("summary");
    if !evidence.is_some_and(Value::is_object) || !summary.is_some_and(Value::is_object) {
        return Err(err("provider join lacks evidence provenance or summary"));
    }
    let evidence = evidence.and_then(Value::as_object).unwrap();
    let summary = summary.and_then(Value::as_object).unwrap();
    let metrics = evidence.get("metrics").and_then(Value::as_array);
    let dimensions = evidence.get("dimensions").and_then(Value::as_array);
    match metrics {
        Some(m) if !m.is_empty() && m.iter().all(Value::is_object) => {}
        _ => return Err(err("provider join does not declare typed numeric metrics")),
    }
    match dimensions {
        Some(d) if d.iter().all(|i| i.as_str().is_some_and(|s| !s.is_empty())) => {}
        _ => {
            return Err(err(
                "provider join dimensions must be declared string names",
            ))
        }
    }
    let metrics = metrics.unwrap();
    let dimensions = dimensions.unwrap();
    if metrics.len() > 64 || dimensions.len() > 64 {
        return Err(err("provider join exceeds metric/dimension field bounds"));
    }
    let dim_names: Vec<&str> = dimensions.iter().filter_map(Value::as_str).collect();
    if dim_names
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .len()
        != dim_names.len()
    {
        return Err(err("provider join dimension names must be unique"));
    }
    let names: Vec<&str> = metrics
        .iter()
        .map(|m| m.get("name").and_then(Value::as_str).unwrap_or_default())
        .collect();
    if names.iter().collect::<std::collections::HashSet<_>>().len() != names.len()
        || names.iter().any(|n| n.is_empty())
    {
        return Err(err(
            "provider join metric names must be unique non-empty strings",
        ));
    }
    for item in metrics {
        if item.get("type").and_then(Value::as_str).unwrap_or("number") != "number" {
            return Err(err(format!(
                "provider metric {} is not numeric",
                crate::py::repr(item.get("name").unwrap_or(&Value::Null))
            )));
        }
    }
    let row_count = summary.get("rows").and_then(Value::as_i64);
    let Some(row_count) =
        row_count.filter(|n| *n >= 0 && *n as usize <= MAX_PROVIDER_ROWS_PER_SOURCE)
    else {
        return Err(err("provider join row count exceeds its declared bound"));
    };
    let count = |key: &str| -> Result<i64, BIExportError> {
        match summary.get(key).and_then(Value::as_i64) {
            Some(n) if n >= 0 => Ok(n),
            _ => Err(err("provider join contains invalid population counts")),
        }
    };
    let source_counts = |key: &str| count(key);
    let pages = source_counts("pages")?;
    let matched_pages = source_counts("matched_pages")?;
    let matched_rows = source_counts("matched_rows")?;
    let external_only = source_counts("external_only")?;
    let unkeyable_rows = source_counts("unkeyable_rows")?;
    let crawl_only = source_counts("crawl_only")?;
    let unkeyable_pages = source_counts("unkeyable_pages")?;
    for (population, expected) in [
        ("matched", matched_pages),
        ("crawl_only", crawl_only),
        ("external_only", external_only),
        ("unkeyable_pages", unkeyable_pages),
        ("unkeyable_rows", unkeyable_rows),
    ] {
        let actual = join
            .get(population)
            .and_then(Value::as_array)
            .map(|a| a.len() as i64)
            .unwrap_or_default();
        if actual != expected {
            return Err(err(format!(
                "provider join {population} rows disagree with its summary"
            )));
        }
    }
    if matched_rows + external_only + unkeyable_rows != row_count {
        return Err(err(
            "provider join evidence populations do not conserve source rows",
        ));
    }
    if matched_pages + crawl_only + unkeyable_pages != pages {
        return Err(err(
            "provider join page populations do not conserve crawl pages",
        ));
    }
    Ok(())
}

/// Validate a store-backed join.
pub fn validate_store_join(join: &Value) -> Result<(), BIExportError> {
    if !spiderhead_core::formats::matches(
        join.get("format").and_then(Value::as_str),
        "spiderhead.evidence-join-sqlite.v1",
    ) {
        return Err(err("unsupported cursor-backed evidence join format"));
    }
    let evidence = join.get("evidence");
    let summary = join.get("summary");
    if !evidence.is_some_and(Value::is_object) || !summary.is_some_and(Value::is_object) {
        return Err(err("evidence join store lacks provenance or summary"));
    }
    let metrics = evidence
        .and_then(|e| e.get("metrics"))
        .and_then(Value::as_array);
    let dimensions = join.get("dimension_names").and_then(Value::as_array);
    match metrics {
        Some(m) if !m.is_empty() && m.iter().all(Value::is_object) => {}
        _ => {
            return Err(err(
                "evidence join store does not declare typed numeric metrics",
            ))
        }
    }
    match dimensions {
        Some(d) if d.iter().all(|i| i.as_str().is_some_and(|s| !s.is_empty())) => {}
        _ => return Err(err("evidence join store dimensions are invalid")),
    }
    let metrics = metrics.unwrap();
    let dimensions = dimensions.unwrap();
    if metrics.len() > 64 || dimensions.len() > 64 {
        return Err(err(
            "evidence join store exceeds metric/dimension field bounds",
        ));
    }
    let names: Vec<&str> = metrics
        .iter()
        .map(|m| m.get("name").and_then(Value::as_str).unwrap_or_default())
        .collect();
    if names.iter().collect::<std::collections::HashSet<_>>().len() != names.len()
        || names.iter().any(|n| n.is_empty())
    {
        return Err(err("evidence join store metric names are invalid"));
    }
    for item in metrics {
        if item.get("type").and_then(Value::as_str).unwrap_or("number") != "number" {
            return Err(err(format!(
                "provider metric {} is not numeric",
                crate::py::repr(item.get("name").unwrap_or(&Value::Null))
            )));
        }
    }
    Ok(())
}

/// Select the saved join, or join normalized evidence.
///
/// Joining normalized evidence needs the evidence-join routine from the data sources crate,
/// which is not ported: a normalized input without a saved join returns the export error
/// until that dependency is available.
pub fn join_from_provider(
    provider: &ProviderInput,
    run: &mut RunInput,
) -> Result<Option<Value>, BIExportError> {
    if let Some(join) = &provider.join {
        let source_scan_id = join
            .get("crawl")
            .and_then(|c| c.get("scan_uuid"))
            .and_then(Value::as_str);
        if let Some(id) = source_scan_id {
            if run.source_kind == "scan" && id != run.run_id {
                return Err(err(format!(
                    "provider join {} belongs to scan {id}, not {}",
                    provider
                        .name
                        .clone()
                        .unwrap_or_else(|| provider.source_id.clone()),
                    run.run_id
                )));
            }
        }
        return Ok(Some(join.clone()));
    }
    if provider.normalized.is_none() {
        return Ok(None);
    }
    if run.page_count > 50_000 {
        return Err(err(
            "normalized provider evidence must be joined before a large scan BI export; \
             supply the saved evidence-join artifact to avoid materializing crawl pages",
        ));
    }
    // The join routine is a named gap: it lives in the data sources crate.
    // The pre-join collision accounting still runs, so oversized or unkeyable inputs fail the same way.
    let document = provider.normalized.clone().unwrap_or_else(|| json!({}));
    let rows = document
        .get("rows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut page_counts: BTreeMap<String, i64> = BTreeMap::new();
    let mut evidence_counts: BTreeMap<String, i64> = BTreeMap::new();
    let pages: Vec<Value> = (run.pages_factory)().collect();
    for page in &pages {
        if let Some(key) = crate::join::normalize_join_key(page_url(page).as_deref()) {
            *page_counts.entry(key).or_insert(0) += 1;
        }
    }
    for row in &rows {
        let url = row.get("url").cloned().unwrap_or_else(|| json!({}));
        let key = if url.get("state").and_then(Value::as_str) == Some("unkeyable") {
            None
        } else {
            crate::join::normalize_join_key(
                url.get("resolved")
                    .or_else(|| url.get("raw"))
                    .and_then(Value::as_str),
            )
        };
        if let Some(key) = key {
            *evidence_counts.entry(key).or_insert(0) += 1;
        }
    }
    let candidate_pairs: i64 = page_counts
        .keys()
        .filter(|k| evidence_counts.contains_key(*k))
        .map(|k| page_counts[k] * evidence_counts[k])
        .sum();
    if candidate_pairs > 500_000 {
        return Err(err(
            "provider join would create over 500000 page/evidence references; \
             the source has a high normalized-key collision and no BI package was published",
        ));
    }
    crate::evidence_join::join_evidence(
        &pages,
        &document,
        None,
        Some(&json!({"source": run.source_kind, "scan_uuid": run.run_id})),
    )
    .map(Some)
    .map_err(|e| err(e.0))
}

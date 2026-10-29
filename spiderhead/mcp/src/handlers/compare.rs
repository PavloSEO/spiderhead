//! Saved-comparison handlers: the four-way crawl diff, the per-check findings
//! delta, bounded reads of a saved compare.v2 package, the job-status probe,
//! the Markdown summary, the exact-URL field/finding diff, the cross-segment
//! counterpart diff and the third-party crawl import. Every one of them is
//! offline: they read already-saved audits and scans and never fetch.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::Path;

use serde_json::{json, Map, Value};
use sha2::Digest as _;
use spiderhead_crawl::external_import::import_third_party_crawl;
use spiderhead_sf_import::compare::{compare, header_obj, iter_rows, run_of, CompareError};
use spiderhead_sf_import::compare_store::{
    compare_markdown, findings_delta, read_compare_page, writer_is_active,
};
use spiderhead_sf_import::pyvalue::py_eq;
use spiderhead_store::inputs::{load_audit_source, Diagnostic};
use spiderhead_store::scan_artifact::open_scan;

use crate::error::ToolError;

use super::verification::bridge;
use super::{failed, opt, opt_bool, opt_int, opt_str, plain};

/// A compare-store error mapped the way the shared boundary prints it.
fn compare_error(error: CompareError) -> ToolError {
    ToolError::Failed(error.0)
}

/// Clip an error message to the reason budget a handler document carries.
fn clip(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

/// `compare_crawls`: diff two audits into the four buckets, optionally through
/// the checkpointed file pipeline when `out_dir` names a destination.
pub fn compare_crawls(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let before_raw = opt(&args, "before").unwrap_or(&Value::Null);
    let after_raw = opt(&args, "after").unwrap_or(&Value::Null);
    let before = load_audit_source(before_raw, "before", Some(&mut diagnostics)).map_err(failed)?;
    let after = load_audit_source(after_raw, "after", Some(&mut diagnostics)).map_err(failed)?;
    let force = opt_bool(&args, "force").unwrap_or(false);
    let compression = opt_str(&args, "compression").unwrap_or("none");
    let resume = opt_bool(&args, "resume").unwrap_or(false);
    let out_dir = opt_str(&args, "out_dir").map(Path::new);
    let correspondence = opt(&args, "correspondence");
    let before_view = bridge(&before)?;
    let after_view = bridge(&after)?;
    let mut result = compare(
        &before_view,
        &after_view,
        force,
        correspondence,
        out_dir,
        compression,
        resume,
        None,
        None,
    )
    .map_err(compare_error)?;
    if !diagnostics.is_empty() {
        result.insert("input_diagnostics".into(), json!(diagnostics));
    }
    Ok(Value::Object(result))
}

/// `compare_findings`: classify the per-check delta between two audits'
/// finding streams — presence changes only, never a recrawl.
pub fn compare_findings(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    if opt(&args, "before").is_none() || opt(&args, "after").is_none() {
        return Err(ToolError::Failed(
            "before and after required: audit documents or paths to their JSON or SQLite files"
                .into(),
        ));
    }
    let before = load_audit_source(opt(&args, "before").unwrap_or(&Value::Null), "before", None)
        .map_err(failed)?;
    let after = load_audit_source(opt(&args, "after").unwrap_or(&Value::Null), "after", None)
        .map_err(failed)?;
    let before_view = bridge(&before)?;
    let after_view = bridge(&after)?;
    let before_rows = iter_rows(&before_view, "issues").map_err(compare_error)?;
    let after_rows = iter_rows(&after_view, "issues").map_err(compare_error)?;
    let delta = findings_delta(&before_rows, &after_rows).map_err(compare_error)?;
    let mut out = Map::new();
    out.insert("ok".into(), Value::Bool(true));
    out.insert("format".into(), json!("compare-findings.v1"));
    out.extend(delta);
    Ok(Value::Object(out))
}

/// `compare_read`: one bounded, sorted page from one compare.v2 file. A read
/// that fails names itself `unavailable` in the document — an unreadable
/// package is evidence, not a crash.
pub fn compare_read(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    let manifest_path = super::need_str(
        &args,
        "manifest_path",
        "compare-read",
        &[
            "manifest_path",
            "file",
            "offset",
            "limit",
            "sort",
            "direction",
            "contains",
            "segment",
            "change",
            "max_bytes",
        ],
    )?;
    let file = super::need_str(
        &args,
        "file",
        "compare-read",
        &[
            "manifest_path",
            "file",
            "offset",
            "limit",
            "sort",
            "direction",
            "contains",
            "segment",
            "change",
            "max_bytes",
        ],
    )?;
    let offset = opt_int(&args, "offset").unwrap_or(0);
    let limit = opt_int(&args, "limit").unwrap_or(200);
    let sort = opt_str(&args, "sort").unwrap_or("url");
    let direction = opt_str(&args, "direction").unwrap_or("asc");
    let max_bytes = opt_int(&args, "max_bytes").unwrap_or(1_048_576);
    match read_compare_page(
        Path::new(manifest_path),
        file,
        offset,
        limit,
        sort,
        direction,
        opt_str(&args, "contains"),
        opt_str(&args, "segment"),
        opt_str(&args, "change"),
        max_bytes,
    ) {
        Ok(page) => Ok(Value::Object(page)),
        Err(error) => Ok(json!({
            "ok": false,
            "state": "unavailable",
            "error": error.0,
        })),
    }
}

/// `compare_status`: report a comparison destination's progress — finished
/// from its manifest, running/resumable from its work directory's checkpoint
/// and a live writer-lock probe.
pub fn compare_status(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    let out_dir = super::need_str(&args, "out_dir", "compare-status", &["out_dir"])?;
    let destination =
        std::path::absolute(out_dir).map_err(|error| ToolError::Failed(error.to_string()))?;
    if destination.is_symlink() {
        return Err(ToolError::Failed(
            "comparison output path must not be a symbolic link".into(),
        ));
    }
    let manifest = destination.join("compare.json");
    if manifest.is_file() && !manifest.is_symlink() {
        let text = std::fs::read_to_string(&manifest)
            .map_err(|error| ToolError::Failed(error.to_string()))?;
        let parsed: Value =
            serde_json::from_str(&text).map_err(|error| ToolError::Failed(error.to_string()))?;
        let job_id = parsed
            .get("job")
            .filter(|job| job.is_object())
            .and_then(|job| job.get("id"))
            .cloned()
            .unwrap_or(Value::Null);
        return Ok(json!({
            "ok": true,
            "format": "compare.job-status.v1",
            "state": "finished",
            "stage": "complete",
            "job_id": job_id,
            "completed": 1,
            "total": 1,
            "resumable": false,
            "cache_available": true,
        }));
    }
    let work = destination.with_file_name(format!(
        ".{}.compare-work",
        destination
            .file_name()
            .map(|name| name.to_string_lossy())
            .unwrap_or_default()
    ));
    if work.is_symlink() || !work.is_dir() {
        return Ok(json!({
            "ok": false,
            "state": "unavailable",
            "error": "comparison job was not found",
        }));
    }
    let checkpoint = work.join("job.json");
    if checkpoint.is_symlink() || !checkpoint.is_file() {
        return Ok(json!({
            "ok": false,
            "state": "unavailable",
            "error": "comparison checkpoint is unreadable",
        }));
    }
    let text = std::fs::read_to_string(&checkpoint)
        .map_err(|error| ToolError::Failed(error.to_string()))?;
    let data: Value =
        serde_json::from_str(&text).map_err(|error| ToolError::Failed(error.to_string()))?;
    let active = writer_is_active(&work);
    let resumable = !active
        && matches!(
            data.get("state").and_then(Value::as_str),
            Some("failed") | Some("paused") | Some("running")
        );
    let mut out = Map::new();
    out.insert("ok".into(), Value::Bool(true));
    out.insert("format".into(), json!("compare.job-status.v1"));
    for key in ["job_id", "state", "stage", "completed", "total", "reason"] {
        out.insert(key.into(), data.get(key).cloned().unwrap_or(Value::Null));
    }
    out.insert("resumable".into(), Value::Bool(resumable));
    out.insert("active".into(), Value::Bool(active));
    out.insert("cache_available".into(), Value::Bool(false));
    Ok(Value::Object(out))
}

/// `compare_report`: render the saved comparison as bounded Markdown and link
/// it into place under a name that must not already exist.
pub fn compare_report(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    let manifest_path = super::need_str(
        &args,
        "manifest_path",
        "compare-report",
        &["manifest_path", "out_file", "max_bytes"],
    )?;
    let out_file = super::need_str(
        &args,
        "out_file",
        "compare-report",
        &["manifest_path", "out_file", "max_bytes"],
    )?;
    let max_bytes = opt_int(&args, "max_bytes").unwrap_or(65_536);
    let report = compare_markdown(Path::new(manifest_path), max_bytes).map_err(compare_error)?;
    let destination =
        std::path::absolute(out_file).map_err(|error| ToolError::Failed(error.to_string()))?;
    if destination.exists() {
        return Err(ToolError::Failed(format!(
            "report output already exists: {}",
            destination.display()
        )));
    }
    let parent = destination.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(|error| ToolError::Failed(error.to_string()))?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".compare-report-")
        .tempfile_in(parent)
        .map_err(|error| ToolError::Failed(error.to_string()))?;
    temporary
        .write_all(report.as_bytes())
        .map_err(|error| ToolError::Failed(error.to_string()))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| ToolError::Failed(error.to_string()))?;
    std::fs::hard_link(temporary.path(), &destination)
        .map_err(|error| ToolError::Failed(error.to_string()))?;
    drop(temporary);
    let mut hasher = sha2::Sha256::new();
    hasher.update(report.as_bytes());
    let mut digest = String::with_capacity(64);
    for byte in hasher.finalize() {
        digest.push(char::from_digit((byte >> 4) as u32, 16).unwrap_or('0'));
        digest.push(char::from_digit((byte & 0xf) as u32, 16).unwrap_or('0'));
    }
    Ok(json!({
        "ok": true,
        "format": "compare.v2.markdown",
        "report": destination.display().to_string(),
        "bytes": report.len(),
        "sha256": digest,
    }))
}

/// The measured fields an exact-URL card carries, in report order.
const URL_FIELDS: [&str; 11] = [
    "status_code",
    "title",
    "meta_description",
    "h1",
    "word_count",
    "inlinks",
    "internal_outlinks",
    "canonical",
    "response_time",
    "size_bytes",
    "indexability",
];

/// The card/report name for a measured field (`response_time_seconds` reads
/// better in a diff than the internal metric key).
fn field_alias(field: &str) -> &str {
    match field {
        "response_time" => "response_time_seconds",
        "size_bytes" => "html_size_bytes",
        other => other,
    }
}

/// One field of a page row: the top-level key wins (an explicit `null` counts
/// as retained-but-empty), the `metrics` block is the fallback.
fn field_value(page: &Value, field: &str) -> Value {
    if let Some(value) = page.get(field) {
        return value.clone();
    }
    page.get("metrics")
        .filter(|value| value.is_object())
        .and_then(|metrics| metrics.get(field))
        .cloned()
        .unwrap_or(Value::Null)
}

/// The card for a URL one scan actually fetched: every field measured or
/// named unavailable.
fn present_card(page: &Value, url: &str) -> Value {
    let mut fields = Map::new();
    for field in URL_FIELDS {
        let value = field_value(page, field);
        let measured = !value.is_null();
        let mut entry = Map::new();
        entry.insert(
            "state".into(),
            json!(if measured { "measured" } else { "unavailable" }),
        );
        entry.insert("value".into(), value);
        if !measured {
            entry.insert(
                "reason".into(),
                json!(format!("source does not retain {field}")),
            );
        }
        fields.insert(field_alias(field).to_owned(), Value::Object(entry));
    }
    json!({"state": "present", "url": url, "fields": fields})
}

/// Whether a raw argument names a native scan file the body diff can open.
fn is_sqlite_input(args: &Map<String, Value>, name: &str) -> bool {
    args.get(name)
        .and_then(Value::as_str)
        .map(|path| {
            matches!(
                Path::new(path)
                    .extension()
                    .map(|extension| extension.to_string_lossy().to_lowercase()),
                Some(extension) if extension == "sqlite" || extension == "db"
            )
        })
        .unwrap_or(false)
}

/// The retained-body text diff over two native scans; every failure mode
/// becomes the reason text the boundary catches.
#[allow(clippy::too_many_arguments)]
fn body_diff_sqlite(
    left_path: &str,
    right_path: &str,
    url: &str,
    max_bytes: i64,
    max_lines: i64,
    left_representation: &str,
    right_representation: &str,
) -> Result<Value, String> {
    let left = open_scan(Path::new(left_path), false, None).map_err(|error| plain(&error))?;
    let right = open_scan(Path::new(right_path), false, None).map_err(|error| plain(&error))?;
    let row = spiderhead_store::body_diff::body_diff(
        &left,
        &right,
        url,
        None,
        "static",
        true,
        max_bytes,
        max_lines,
        Some(left_representation),
        Some(right_representation),
    )
    .map_err(|error| plain(&error))?;
    Ok(Value::Object(row))
}

/// `compare_url`: one exact URL side by side across two audits — field cards,
/// the field-change list, the finding delta, and, for native scans, the
/// retained-body text diff.
pub fn compare_url(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    let url = match opt(&args, "url").and_then(Value::as_str) {
        Some(text) if !text.is_empty() && text.chars().count() <= 8192 => text.to_owned(),
        _ => {
            return Err(ToolError::Failed(
                "url must be nonempty exact URL text of at most 8192 characters".into(),
            ))
        }
    };
    let max_bytes = match opt(&args, "max_bytes").map(Value::as_i64) {
        None => 65_536,
        Some(Some(value)) if (1..=65_536).contains(&value) => value,
        Some(_) => {
            return Err(ToolError::Failed(
                "max_bytes must be an integer 1..65536".into(),
            ))
        }
    };
    let max_lines = match opt(&args, "max_lines").map(Value::as_i64) {
        None => 200,
        Some(Some(value)) if (1..=200).contains(&value) => value,
        Some(_) => {
            return Err(ToolError::Failed(
                "max_lines must be an integer 1..200".into(),
            ))
        }
    };
    let left_representation = opt_str(&args, "left_representation").unwrap_or("static");
    let right_representation = opt_str(&args, "right_representation").unwrap_or("static");
    for representation in [left_representation, right_representation] {
        if !matches!(representation, "static" | "rendered" | "legacy_fragment") {
            return Err(ToolError::Failed(
                "representations must be static, rendered, or legacy_fragment".into(),
            ));
        }
    }
    let before = load_audit_source(opt(&args, "before").unwrap_or(&Value::Null), "before", None)
        .map_err(failed)?;
    let after = load_audit_source(opt(&args, "after").unwrap_or(&Value::Null), "after", None)
        .map_err(failed)?;
    let before_view = bridge(&before)?;
    let after_view = bridge(&after)?;

    let mut cards: Vec<Value> = Vec::new();
    let mut per_source_findings: Vec<Vec<Value>> = Vec::new();
    for source in [&before_view, &after_view] {
        let pages = iter_rows(source, "pages").map_err(compare_error)?;
        let issues = iter_rows(source, "issues").map_err(compare_error)?;
        let page = pages
            .iter()
            .find(|page| page.get("url").and_then(Value::as_str) == Some(url.as_str()));
        cards.push(match page {
            None => json!({"state": "absent", "url": url, "fields": {}}),
            Some(page) => present_card(page, &url),
        });
        per_source_findings.push(
            issues
                .into_iter()
                .filter(|issue| {
                    issue.get("target_url").and_then(Value::as_str) == Some(url.as_str())
                })
                .collect(),
        );
    }

    let (state, reason, changes, finding_delta);
    if cards[0].get("state").and_then(Value::as_str) != Some("present")
        || cards[1].get("state").and_then(Value::as_str) != Some("present")
    {
        state = "not_rechecked";
        reason = Some(
            "both scans must contain this URL before field or finding deltas can be interpreted",
        );
        changes = Map::new();
        finding_delta = json!({
            "state": "not_rechecked",
            "reason": reason,
            "entered": [],
            "left": [],
        });
    } else {
        state = "compared";
        reason = None;
        let left_fields = cards[0]
            .get("fields")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let right_fields = cards[1]
            .get("fields")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut compared = Map::new();
        for (name, left) in &left_fields {
            let right = right_fields.get(name);
            let left_measured = left.get("state").and_then(Value::as_str) == Some("measured");
            let right_measured = right
                .and_then(|entry| entry.get("state"))
                .and_then(Value::as_str)
                == Some("measured");
            if !left_measured || !right_measured {
                compared.insert(
                    name.clone(),
                    json!({
                        "state": "unavailable",
                        "reason": "field was not measured on both scans",
                    }),
                );
            } else {
                let left_value = left.get("value").cloned().unwrap_or(Value::Null);
                let right_value = right
                    .and_then(|entry| entry.get("value"))
                    .cloned()
                    .unwrap_or(Value::Null);
                if !py_eq(&left_value, &right_value) {
                    compared.insert(
                        name.clone(),
                        json!({"state": "changed", "before": left_value, "after": right_value}),
                    );
                } else {
                    compared.insert(
                        name.clone(),
                        json!({"state": "unchanged", "value": left_value}),
                    );
                }
            }
        }
        changes = compared;

        // Findings keyed by check; an absent check name keys the empty string
        // so audit-wide rows still take part in the delta.
        let before_by_check: BTreeMap<String, Value> = per_source_findings[0]
            .iter()
            .map(|row| {
                (
                    row.get("check")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    row.clone(),
                )
            })
            .collect();
        let after_by_check: BTreeMap<String, Value> = per_source_findings[1]
            .iter()
            .map(|row| {
                (
                    row.get("check")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    row.clone(),
                )
            })
            .collect();
        let mut unavailable: BTreeSet<String> = BTreeSet::new();
        let mut invalid_run = false;
        for source in [&before_view, &after_view] {
            let run = run_of(header_obj(source));
            if matches!(run.get("crawl_valid"), Some(Value::Bool(false))) {
                invalid_run = true;
            }
            for field in ["checks_skipped", "checks_disabled"] {
                for item in run
                    .get(field)
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(id) = item
                        .as_object()
                        .and_then(|entry| entry.get("id"))
                        .and_then(Value::as_str)
                    {
                        unavailable.insert(id.to_owned());
                    }
                }
            }
        }
        if invalid_run {
            unavailable.extend(before_by_check.keys().cloned());
            unavailable.extend(after_by_check.keys().cloned());
        }
        let mut entered = Vec::new();
        let mut left_rows = Vec::new();
        let mut not_comparable = Vec::new();
        for (check, row) in &after_by_check {
            if before_by_check.contains_key(check) {
                continue;
            }
            if unavailable.contains(check) {
                not_comparable.push(json!({
                    "check": check,
                    "state": "not_measured",
                    "reason": "check coverage is incomplete",
                }));
            } else {
                entered.push(row.clone());
            }
        }
        for (check, row) in &before_by_check {
            if after_by_check.contains_key(check) {
                continue;
            }
            if unavailable.contains(check) {
                not_comparable.push(json!({
                    "check": check,
                    "state": "not_measured",
                    "reason": "check coverage is incomplete",
                }));
            } else {
                left_rows.push(row.clone());
            }
        }
        let unchanged: Vec<String> = before_by_check
            .keys()
            .filter(|check| after_by_check.contains_key(*check))
            .cloned()
            .collect();
        finding_delta = json!({
            "state": "compared",
            "entered": entered,
            "left": left_rows,
            "not_comparable": not_comparable,
            "unchanged": unchanged,
        });
    }

    let mut body = json!({
        "state": "not_requested",
        "reason": "text diff requires native scan.v1 SQLite inputs",
    });
    if is_sqlite_input(&args, "before") && is_sqlite_input(&args, "after") {
        let left_path = args
            .get("before")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let right_path = args
            .get("after")
            .and_then(Value::as_str)
            .unwrap_or_default();
        body = match body_diff_sqlite(
            left_path,
            right_path,
            &url,
            max_bytes,
            max_lines,
            left_representation,
            right_representation,
        ) {
            Ok(value) => value,
            Err(message) => json!({
                "state": "unavailable",
                "reason": clip(&message, 2048),
            }),
        };
    }

    let mut out = Map::new();
    out.insert("ok".into(), Value::Bool(true));
    out.insert("format".into(), json!("compare-url.v1"));
    out.insert("url".into(), json!(url));
    out.insert("state".into(), json!(state));
    if let Some(reason) = reason {
        out.insert("reason".into(), json!(reason));
    }
    out.insert("before".into(), cards[0].clone());
    out.insert("after".into(), cards[1].clone());
    out.insert("changes".into(), Value::Object(changes));
    out.insert("findings".into(), finding_delta);
    out.insert("body_diff".into(), body);
    Ok(Value::Object(out))
}

/// `crawl_import`: read one third-party crawl bundle through its manifest into
/// the toolkit's own crawl dataset. Refusals are `ok: false` documents, not
/// errors — a bad manifest is caller evidence, not a crash.
pub fn crawl_import(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    let manifest_path = args.get("manifest_path").and_then(Value::as_str);
    if manifest_path.map(str::trim).unwrap_or_default().is_empty() {
        return Ok(json!({
            "ok": false,
            "error": "manifest_path must be a non-empty local path",
        }));
    }
    match import_third_party_crawl(manifest_path.unwrap_or_default()) {
        Ok(result) => {
            let mut out = Map::new();
            out.insert("ok".into(), Value::Bool(true));
            if let Some(object) = result.as_object() {
                out.extend(object.clone());
            }
            Ok(Value::Object(out))
        }
        Err(error) => Ok(json!({
            "ok": false,
            "error": error.0,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "https://example.test/a";
    const B: &str = "https://example.test/b";
    const C: &str = "https://example.test/c";

    /// The shared audit fixture shape (`tests.test_verify_fixes._audit`): one
    /// crawl config so two sides never differ in results-affecting settings.
    fn audit(urls: &[&str], issues: Vec<Value>) -> Value {
        let pages: Vec<Value> = urls
            .iter()
            .map(|url| {
                json!({
                    "url": url,
                    "status_code": 200,
                    "content_type": "text/html",
                    "metrics": {"representation": "static"},
                })
            })
            .collect();
        json!({
            "schema_version": "2.0",
            "run": {
                "generated_at": "2026-10-01T00:00:00Z",
                "source": "https://example.test/",
                "crawl_config": {"robots": "respect", "max_urls": 1000},
                "checks_skipped": [],
                "checks_disabled": [],
                "crawl_partial": false,
            },
            "summary": {
                "check_coverage": {
                    "checks_silent_ids": ["TITLE_MISSING", "DESC_MISSING", "CANONICAL_MISSING"],
                },
            },
            "pages": pages,
            "issues": issues,
        })
    }

    fn issue(id: &str, check: &str, url: &str) -> Value {
        json!({"id": id, "check": check, "target_url": url, "status_code": 200})
    }

    // (the legacy in-memory branch: documents in, compare.v1 buckets out)
    #[test]
    fn compare_crawls_documents_diff_in_memory() {
        let before = audit(&[A, B, C], vec![issue("before-a", "TITLE_MISSING", A)]);
        let after = audit(&[A, B, C], vec![issue("after-b", "DESC_MISSING", B)]);
        let legacy = compare_crawls(&json!({"before": before, "after": after})).unwrap();
        assert_eq!(legacy["schema_version"], "compare.v1");
        assert_eq!(legacy["left"].as_array().map(Vec::len), Some(1));
        assert_eq!(legacy["entered"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn file_compare_is_deterministic_and_status_reads_finished() {
        let dir = tempfile::Builder::new()
            .prefix("compare-deterministic-")
            .tempdir()
            .unwrap();
        let before = audit(
            &[A, B],
            vec![
                issue("b", "TITLE_MISSING", B),
                issue("a", "TITLE_MISSING", A),
            ],
        );
        let after = audit(&[A, B, C], vec![]);
        let first_dir = dir.path().join("first");
        let first = compare_crawls(&json!({
            "before": before,
            "after": after,
            "out_dir": first_dir.display().to_string(),
        }))
        .unwrap();
        let cached = compare_crawls(&json!({
            "before": before,
            "after": after,
            "out_dir": first_dir.display().to_string(),
        }))
        .unwrap();
        assert_eq!(cached["cache_hit"], json!(true));
        let status = compare_status(&json!({"out_dir": first_dir.display().to_string()})).unwrap();
        assert_eq!(status["state"], "finished");
        let mut before_reversed = before.clone();
        before_reversed["issues"] = {
            let mut rows = before["issues"].as_array().cloned().unwrap();
            rows.reverse();
            Value::Array(rows)
        };
        let second_dir = dir.path().join("second");
        let second = compare_crawls(&json!({
            "before": before_reversed,
            "after": after,
            "out_dir": second_dir.display().to_string(),
        }))
        .unwrap();
        assert_eq!(first["files"], second["files"]);
        let refused = compare_crawls(&json!({
            "before": before_reversed,
            "after": after,
            "out_dir": first_dir.display().to_string(),
        }));
        assert!(refused
            .err()
            .unwrap()
            .to_string()
            .contains("already exists"));
    }

    // a destination with neither manifest nor work directory is evidence, not a crash)
    #[test]
    fn compare_status_reports_an_unknown_destination() {
        let dir = tempfile::Builder::new()
            .prefix("compare-status-missing-")
            .tempdir()
            .unwrap();
        let result =
            compare_status(&json!({"out_dir": dir.path().join("nowhere").display().to_string()}))
                .unwrap();
        assert_eq!(result["ok"], json!(false));
        assert_eq!(result["state"], "unavailable");
        assert_eq!(result["error"], "comparison job was not found");
    }

    // `ok: false` document, never a raised error across this boundary)
    #[test]
    fn compare_read_reports_an_unavailable_package() {
        let result = compare_read(&json!({
            "manifest_path": "/nonexistent/compare/compare.json",
            "file": "entered",
        }))
        .unwrap();
        assert_eq!(result["ok"], json!(false));
        assert_eq!(result["state"], "unavailable");
        assert!(!result["error"].as_str().unwrap_or_default().is_empty());
    }

    #[test]
    fn compare_findings_requires_both_sides() {
        let error = compare_findings(&json!({"after": []})).unwrap_err();
        assert!(error.to_string().contains("before and after required"));
    }

    // (the shipped JSON fixture is not part of this tree; the same mutation runs
    // on an inline stand-in with the fixture's issue shape)
    #[test]
    fn compare_findings_reads_documents_and_classifies_a_mutation() {
        let document = audit(
            &[A, B],
            vec![json!({
                "check": "THIN_CONTENT",
                "severity": "warning",
                "target_url": "https://example.com/p3",
            })],
        );
        let after = json!({
            "schema_version": "2.0",
            "run": document["run"].clone(),
            "summary": document["summary"].clone(),
            "pages": document["pages"].clone(),
            "issues": [
                {"check": "TITLE_MISSING", "severity": "critical", "target_url": A},
                {"check": "DESC_LONG", "severity": "warning", "target_url": B},
            ],
        });
        let result = compare_findings(&json!({"before": document, "after": after})).unwrap();
        assert_eq!(result["ok"], json!(true));
        assert_eq!(result["format"], "compare-findings.v1");
        let directions: Vec<(String, String, String)> = result["changed"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row["check"].as_str().unwrap().to_owned(),
                    row["target_url"].as_str().unwrap().to_owned(),
                    row["direction"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        assert_eq!(
            directions,
            vec![
                ("DESC_LONG".into(), B.into(), "added".into()),
                (
                    "THIN_CONTENT".into(),
                    "https://example.com/p3".into(),
                    "resolved".into()
                ),
                ("TITLE_MISSING".into(), A.into(), "regressed".into()),
            ]
        );
        assert_eq!(result["totals"]["unchanged"], json!(0));
        assert_eq!(result["totals"]["before"], json!(1));
        assert_eq!(result["totals"]["after"], json!(2));
    }

    #[test]
    fn compare_url_never_calls_a_missing_url_a_fix() {
        let before = audit(&[A], vec![issue("a", "TITLE_MISSING", A)]);
        let after = audit(&[], vec![]);
        let result = compare_url(&json!({"before": before, "after": after, "url": A})).unwrap();
        assert_eq!(result["state"], "not_rechecked");
        assert_eq!(result["findings"]["state"], "not_rechecked");
        assert_eq!(result["findings"]["left"], json!([]));
    }

    // (the refusal branch; the import path itself is covered by the dataset tests)
    #[test]
    fn crawl_import_refuses_an_empty_manifest_path() {
        let result = crawl_import(&json!({})).unwrap();
        assert_eq!(
            result,
            json!({"ok": false, "error": "manifest_path must be a non-empty local path"})
        );
    }
}

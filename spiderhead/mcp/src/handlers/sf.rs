//! External-crawl audit tool handlers: export discovery, the health summary,
//! bounded issue pages, the task backlog builder, the snippet-edit importer
//! and the run entry point. Everything here reads local exports, audits and
//! retained scans — only `sf_audit_run` drives a live crawl, and it is a
//! validation shell in this build: it refuses malformed requests with the
//! same texts the runner would, then reports the crawl itself as not yet
//! ported.

use std::path::Path;
use std::rc::Rc;
use std::sync::LazyLock;

use serde_json::{json, Map, Value};
use spiderhead_parse::text_normalize::{normalize_text, script_evidence};
use spiderhead_sf_import::config::load_config;
use spiderhead_sf_import::loader::{discover_exports, EXPORT_MATCHERS};
use spiderhead_sf_import::pyvalue::py_repr_value;
use spiderhead_sf_import::snippet_edits::{
    attach_retained_context, attach_retained_measurements, build_snippet_tasks,
    measure_snippet_edits, parse_snippet_csv, write_snippet_tasks, FontLoader,
    RetainedContextOptions, SnippetError, SpellcheckArgs, TextMeasure,
};
use spiderhead_store::inputs::Diagnostic;

use crate::error::ToolError;

use super::{failed, opt, opt_int, opt_str};

/// The input modes `sf_audit_run` accepts, mirroring the runner's vocabulary.
const INPUT_MODES: [&str; 4] = ["crawl", "crawl-list", "load-crawl", "parse-exports"];

/// The audit profiles `sf_audit_run` accepts.
const VALID_PROFILES: [&str; 3] = ["custom", "full", "lite"];

/// Sentence/paragraph split for the spelling pass: a run of whitespace after
/// sentence punctuation, or any run of newlines.
static BLOCK_SPLIT: LazyLock<fancy_regex::Regex> =
    LazyLock::new(|| fancy_regex::Regex::new(r"(?<=[.!?…])\s+|\n+").expect("static regex"));

/// A snippet-error mapped to the tool error channel.
fn snippet_error(error: SnippetError) -> ToolError {
    ToolError::Failed(error.0)
}

/// Load one audit document by path, collecting its input notices. A path that
/// is not a file is refused before any read, matching the boundary's check.
fn sf_load(json_path: &str, diagnostics: Option<&mut Vec<Diagnostic>>) -> Result<Value, ToolError> {
    if !Path::new(json_path).is_file() {
        return Err(ToolError::Failed(format!(
            "audit json not found: {json_path}"
        )));
    }
    let (document, notices) =
        spiderhead_store::inputs::resolve_audit_input(&json!(json_path)).map_err(failed)?;
    if let Some(diagnostics) = diagnostics {
        diagnostics.extend(notices);
    }
    Ok(document)
}

/// The spelling pass with no backend installed: block and language counters
/// are still measured, the result is a named skip with no findings.
fn spellcheck_without_backend(args: &SpellcheckArgs<'_>) -> Value {
    // The retained-context caller passes the already-extracted visible text,
    // so the content-area resolver is never re-run here.
    let text = args.visible_text_override;
    let mut blocks: Vec<&str> = Vec::new();
    let mut cursor = 0usize;
    for hit in BLOCK_SPLIT.find_iter(text) {
        // A split that exhausts the engine's budget ends the scan; the
        // remaining text becomes one final block.
        let Ok(part) = hit else { break };
        blocks.push(&text[cursor..part.start()]);
        cursor = part.end();
    }
    blocks.push(&text[cursor..]);
    let mut languages = Map::new();
    languages.insert("ru".into(), json!(0));
    languages.insert("en".into(), json!(0));
    let mut skipped_blocks = 0i64;
    for block in blocks {
        if block.is_empty() || block.trim().is_empty() {
            continue;
        }
        match script_evidence(&normalize_text(block)).script.as_str() {
            "cyrillic" => {
                if let Some(Value::Number(count)) = languages.get_mut("ru") {
                    let value = count.as_i64().unwrap_or(0) + 1;
                    languages.insert("ru".into(), json!(value));
                }
            }
            "latin" => {
                if let Some(Value::Number(count)) = languages.get_mut("en") {
                    let value = count.as_i64().unwrap_or(0) + 1;
                    languages.insert("en".into(), json!(value));
                }
            }
            _ => skipped_blocks += 1,
        }
    }
    json!({
        "ok": true,
        "backend": Value::Null,
        "languages": languages,
        "skipped_blocks": skipped_blocks,
        "state": "skipped",
        "reason": "no spellcheck backend installed",
        "findings_total": 0,
        "findings": [],
    })
}

/// Font stub for this build: no rasterizer is linked, so every measurement
/// request reports an unloadable font — the same refusal the reference fallback
/// hits without Pillow and a font file.
fn font_stub(_font_path: &str, _font_size: i64) -> Result<Rc<dyn TextMeasure>, String> {
    Err("font_path could not be loaded".into())
}

/// `sf_audit_run`: validate the request exactly as the runner would, then
/// report the live crawl as not yet ported. No crawl is started.
pub fn sf_audit_run(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    let mode = opt(&args, "mode").cloned().unwrap_or(Value::Null);
    if !mode
        .as_str()
        .is_some_and(|text| INPUT_MODES.contains(&text))
    {
        return Err(ToolError::Failed(format!(
            "mode must be one of ['crawl', 'crawl-list', 'load-crawl', 'parse-exports'], got {}",
            py_repr_value(&mode)
        )));
    }
    match opt(&args, "profile") {
        None | Some(Value::Null) => {}
        Some(Value::String(text)) if VALID_PROFILES.contains(&text.as_str()) => {}
        Some(other) => {
            return Err(ToolError::Failed(format!(
                "profile must be one of ['custom', 'full', 'lite'], got {}",
                py_repr_value(other)
            )))
        }
    }
    if opt_str(&args, "input")
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
    {
        return Err(ToolError::Failed(
            "`input` (exports dir / .seospider / url / list) is required".into(),
        ));
    }
    if let Some(inspect) = opt(&args, "inspect_gsc") {
        if !inspect.is_boolean() {
            return Err(ToolError::Failed("inspect_gsc must be a boolean".into()));
        }
        if inspect.as_bool().unwrap_or(false) && opt_str(&args, "project").is_none() {
            return Err(ToolError::Failed(
                "inspect_gsc requires a project for source binding and quota history".into(),
            ));
        }
    }
    Err(ToolError::NotYetPorted("sf_audit_run".to_owned()))
}

/// `sf_audit_summary`: the compact health card — project, score, severity
/// counts, the first fifteen ranked checks, sitemap statistics.
pub fn sf_audit_summary(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    let json_path = super::need_str(&args, "json_path", "sf-audit-summary", &["json_path"])?;
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let data = sf_load(json_path, Some(&mut diagnostics))?;
    let Some(summary) = data.get("summary") else {
        return Err(ToolError::Failed("KeyError: 'summary'".into()));
    };
    let Some(run) = data.get("run") else {
        return Err(ToolError::Failed("KeyError: 'run'".into()));
    };
    let top_checks: Map<String, Value> = summary
        .get("by_check")
        .and_then(Value::as_object)
        .map(|checks| {
            checks
                .iter()
                .take(15)
                .map(|(name, row)| (name.clone(), row.clone()))
                .collect()
        })
        .unwrap_or_default();
    let mut result = Map::new();
    result.insert(
        "project".into(),
        run.get("project").cloned().unwrap_or(Value::Null),
    );
    result.insert(
        "health_score".into(),
        summary.get("health_score").cloned().unwrap_or(Value::Null),
    );
    result.insert(
        "by_severity".into(),
        summary.get("by_severity").cloned().unwrap_or(Value::Null),
    );
    result.insert("top_checks".into(), Value::Object(top_checks));
    result.insert(
        "sitemap".into(),
        summary.get("sitemap").cloned().unwrap_or(Value::Null),
    );
    if let Some(provider) = summary.get("provider_findings") {
        result.insert("provider_findings".into(), provider.clone());
    }
    if summary.get("health_score").is_none_or(Value::is_null) {
        for key in [
            "health_score_reason",
            "health_score_basis",
            "health_score_scope",
        ] {
            if let Some(value) = summary.get(key) {
                result.insert(key.into(), value.clone());
            }
        }
    }
    if !diagnostics.is_empty() {
        result.insert("input_diagnostics".into(), json!(diagnostics));
    }
    Ok(Value::Object(result))
}

/// `sf_audit_issues`: the first issues from the audit, filtered by check or
/// severity, clamped to a bounded page.
pub fn sf_audit_issues(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    let json_path = super::need_str(
        &args,
        "json_path",
        "sf-audit-issues",
        &["json_path", "check", "severity", "limit"],
    )?;
    let data = sf_load(json_path, None)?;
    let Some(issues) = data.get("issues").and_then(Value::as_array) else {
        return Err(ToolError::Failed("KeyError: 'issues'".into()));
    };
    // the reference int(): numeric strings parse, floats truncate, booleans coerce.
    let limit = match opt(&args, "limit") {
        None | Some(Value::Null) => 50,
        Some(Value::Number(number)) => number
            .as_f64()
            .map(|value| value as i64)
            .ok_or_else(|| ToolError::Failed("limit must be an integer".into()))?,
        Some(Value::String(text)) => text.trim().parse::<i64>().map_err(|_| {
            ToolError::Failed(format!(
                "invalid literal for int() with base 10: {}",
                py_repr_value(&json!(text))
            ))
        })?,
        Some(Value::Bool(flag)) => {
            if *flag {
                1
            } else {
                0
            }
        }
        Some(_) => return Err(ToolError::Failed("limit must be an integer".into())),
    };
    let limit = limit.clamp(1, 1000);
    let check = opt_str(&args, "check");
    let severity = opt_str(&args, "severity");
    let mut out: Vec<Value> = Vec::new();
    for issue in issues {
        if let Some(check) = check {
            if issue.get("check").and_then(Value::as_str) != Some(check) {
                continue;
            }
        }
        if let Some(severity) = severity {
            if issue.get("severity").and_then(Value::as_str) != Some(severity) {
                continue;
            }
        }
        out.push(issue.clone());
        if out.len() as i64 >= limit {
            break;
        }
    }
    Ok(Value::Array(out))
}

/// `sf_list_exports`: which recognized export files a directory holds and
/// which logical exports are missing, so an agent can explain skipped checks
/// before running the audit.
pub fn sf_list_exports(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    let exports_dir = super::need_str(&args, "exports_dir", "sf-list-exports", &["exports_dir"])?;
    let found = discover_exports(Path::new(exports_dir))
        .map_err(|error| ToolError::Failed(error.to_string()))?;
    let mut found_map = Map::new();
    for (name, path) in found {
        found_map.insert(name, json!(path.display().to_string()));
    }
    let missing: Vec<String> = EXPORT_MATCHERS
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .filter(|name| !found_map.contains_key(name.as_str()))
        .collect();
    Ok(json!({
        "found": Value::Object(found_map),
        "missing": missing,
    }))
}

/// `sf_audit_tasks`: build the prioritized backlog (tasks.json + tasks.md)
/// from an audit document or, when the scan's audit.v2 stream exists, from
/// that stream directly.
pub fn sf_audit_tasks(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    let json_path = super::need_str(
        &args,
        "json_path",
        "sf-audit-tasks",
        &["json_path", "out", "config"],
    )?;
    let out = opt_str(&args, "out").unwrap_or("report");
    let config = opt_str(&args, "config");
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    // An absent default config file means defaults; only a named missing file
    // is a hard error.
    let resolved =
        load_config(config.or(Some("config.json"))).map_err(|error| ToolError::Failed(error.0))?;
    let scan_path = spiderhead_store::audit_v2::audit_v2_path(Path::new(json_path));
    let backlog = if scan_path.exists() {
        let mut entry = Map::new();
        entry.insert("code".into(), json!("audit_v2_stream"));
        entry.insert(
            "message".into(),
            json!("tasks read the audit.v2 findings stream"),
        );
        diagnostics.push(entry);
        spiderhead_reports::sf_tasks::build_tasks_from_audit_v2(&scan_path, Some(&resolved))
            .map_err(|error| ToolError::Failed(error.0))?
    } else {
        let document = sf_load(json_path, Some(&mut diagnostics))?;
        spiderhead_reports::sf_tasks::build_tasks(&document, Some(&resolved))
            .map_err(|error| ToolError::Failed(error.0))?
    };
    std::fs::create_dir_all(out).map_err(|error| ToolError::Failed(error.to_string()))?;
    let (json_file, md_file) = spiderhead_reports::sf_tasks::write_tasks(
        &backlog,
        &Path::new(out).join("tasks.json").display().to_string(),
        &Path::new(out).join("tasks.md").display().to_string(),
    )
    .map_err(|error| ToolError::Failed(error.0))?;
    let mut result = Map::new();
    result.insert(
        "summary".into(),
        backlog.get("summary").cloned().unwrap_or(Value::Null),
    );
    result.insert(
        "tasks_json".into(),
        json!(std::path::absolute(&json_file)
            .map(|path| path.display().to_string())
            .unwrap_or(json_file)),
    );
    result.insert(
        "tasks_md".into(),
        json!(std::path::absolute(&md_file)
            .map(|path| path.display().to_string())
            .unwrap_or(md_file)),
    );
    if !diagnostics.is_empty() {
        result.insert("input_diagnostics".into(), json!(diagnostics));
    }
    Ok(Value::Object(result))
}

/// `sf_snippet_edits`: import supplied title/description edit CSV and write
/// unapplied review tasks to a new JSON file, with optional local-font width
/// measurements and retained-content context.
pub fn sf_snippet_edits(args: &Value) -> Result<Value, ToolError> {
    const ACCEPTED: [&str; 9] = [
        "csv_text",
        "out",
        "metrics",
        "retained_scan",
        "query",
        "representation",
        "variant_key",
        "content_area",
        "max_decoded_bytes",
    ];
    let args = super::kwargs(args);
    let csv_text = super::need_str(&args, "csv_text", "sf-snippet-edits", &ACCEPTED)?;
    let out = super::need_str(&args, "out", "sf-snippet-edits", &ACCEPTED)?;
    let parsed = parse_snippet_csv(csv_text).map_err(snippet_error)?;
    let mut tasks = build_snippet_tasks(&parsed).map_err(snippet_error)?;
    // reference truthiness: an empty query string behaves as no query.
    let query = opt_str(&args, "query").filter(|text| !text.is_empty());
    let retained_scan = opt_str(&args, "retained_scan");
    if query.is_some() && retained_scan.is_none() {
        return Err(ToolError::Failed("query requires retained_scan".into()));
    }
    let metrics = opt(&args, "metrics");
    let loader: FontLoader = &font_stub;
    if let Some(metrics) = metrics {
        let edits: Vec<Value> = parsed
            .get("edits")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let measurements = measure_snippet_edits(&edits, metrics, loader).map_err(snippet_error)?;
        if let Some(object) = tasks.as_object_mut() {
            object.insert("measurements".into(), measurements);
        }
    }
    if let Some(scan) = retained_scan {
        let options = RetainedContextOptions {
            representation: opt_str(&args, "representation").unwrap_or("static"),
            variant_key: opt_str(&args, "variant_key"),
            query,
            content_area: opt(&args, "content_area"),
            max_decoded_bytes: opt_int(&args, "max_decoded_bytes").unwrap_or(8 * 1024 * 1024),
        };
        attach_retained_context(
            &mut tasks,
            Path::new(scan),
            &options,
            &spellcheck_without_backend,
            &[],
        )
        .map_err(snippet_error)?;
        if let Some(metrics) = metrics {
            attach_retained_measurements(&mut tasks, metrics, loader).map_err(snippet_error)?;
        }
    }
    let path = write_snippet_tasks(&tasks, Path::new(out)).map_err(snippet_error)?;
    Ok(json!({
        "schema_version": tasks.get("schema_version").cloned().unwrap_or(Value::Null),
        "summary": tasks.get("summary").cloned().unwrap_or(Value::Null),
        "task_file": std::path::absolute(&path)
            .map(|absolute| absolute.display().to_string())
            .unwrap_or(path),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write one JSON document into the temp dir; answers the path text.
    fn write_json(dir: &tempfile::TempDir, name: &str, document: &Value) -> String {
        let path = dir.path().join(name);
        std::fs::write(&path, serde_json::to_string(document).unwrap()).unwrap();
        path.display().to_string()
    }

    #[test]
    fn sf_audit_summary_surfaces_reason_when_score_is_null() {
        let dir = tempfile::Builder::new()
            .prefix("sf-summary-")
            .tempdir()
            .unwrap();
        let document = json!({
            "run": {"project": "demo"},
            "summary": {
                "health_score": Value::Null,
                "health_score_reason": "requires_rendering: JS rendering needed to trust this crawl",
                "by_severity": {"critical": 3, "warning": 1, "notice": 0},
                "by_check": {"BROKEN_LINK": 3},
                "sitemap": {"urls_in_sitemap": 100},
            },
            "issues": [],
        });
        let path = write_json(&dir, "audit.json", &document);
        let result = sf_audit_summary(&json!({"json_path": path})).unwrap();
        assert_eq!(result["health_score"], Value::Null);
        assert_eq!(
            result["health_score_reason"],
            json!("requires_rendering: JS rendering needed to trust this crawl")
        );
    }

    // (negative control: a numeric score with no reason key must not gain one)
    #[test]
    fn sf_audit_summary_normal_score_unchanged() {
        let dir = tempfile::Builder::new()
            .prefix("sf-summary-ok-")
            .tempdir()
            .unwrap();
        let document = json!({
            "run": {"project": "demo"},
            "summary": {
                "health_score": 87,
                "by_severity": {"critical": 0, "warning": 1, "notice": 0},
                "by_check": {},
                "sitemap": {},
            },
            "issues": [],
        });
        let path = write_json(&dir, "audit.json", &document);
        let result = sf_audit_summary(&json!({"json_path": path})).unwrap();
        assert_eq!(result["health_score"], json!(87));
        assert!(result.get("health_score_reason").is_none());
        assert!(result.get("health_score_basis").is_none());
        assert!(result.get("health_score_scope").is_none());
    }

    #[test]
    fn snippet_csv_exports_review_tasks() {
        let dir = tempfile::Builder::new()
            .prefix("sf-snippet-")
            .tempdir()
            .unwrap();
        let out = dir.path().join("snippet-tasks.json");
        let result = sf_snippet_edits(&json!({
            "csv_text": "url,title,description\nhttps://example.test/p,New title,\n",
            "out": out.display().to_string(),
        }))
        .unwrap();
        assert_eq!(result["summary"], json!({"tasks": 1, "rejected_rows": 0}));
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(saved["tasks"][0]["target_url"], "https://example.test/p");
        assert_eq!(saved["tasks"][0]["state"], "candidate");
    }
}

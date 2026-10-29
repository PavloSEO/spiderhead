//! Paused-artifact steering: the resume inputs a saved crawl must repeat, and the
//! two operator actions (`narrow-scope`, `add-urls`) that apply to a paused site
//! crawl before its resume.

use std::path::Path;

use rusqlite::{OpenFlags, OptionalExtension};
use serde_json::{json, Map, Value};

use spiderhead_core::artifacts::SCAN_V1;
use spiderhead_core::crawl_settings;
use spiderhead_core::host::SystemHost;
use spiderhead_core::settings_data;
use spiderhead_crawl::scope::Scope;
use spiderhead_projects::run_control::{checked_operator_urls, checked_pattern};
use spiderhead_store::error::StoreError;
use spiderhead_store::scan_lease::NativeScanLease;
use spiderhead_store::scan_schema::{read_effective_config, NativeScanSchemaMixin};

use crate::error::ToolError;

fn store_failed(error: StoreError) -> ToolError {
    ToolError::Failed(error.to_string())
}

fn failed(message: impl Into<String>) -> ToolError {
    ToolError::Failed(message.into())
}

/// reference truthiness for the settings fields the resume refusals read.
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(map)) => !map.is_empty(),
        Some(Value::Number(number)) => number.as_f64() != Some(0.0),
    }
}

/// Read back what a resume must repeat: the scan's own start URL and settings.
///
/// A resume is one crawl continuing, not a second crawl aimed at the same file.
/// The frontier, scope and limits already in the artifact decide what is left to
/// fetch, so they are read from it rather than restated at the call site, where
/// a single changed value would either be refused by the writer's configuration
/// fingerprint or -- worse -- continue one crawl under another crawl's rules.
///
/// Read-only and before any request: every refusal below is decided from the
/// file alone, so a mismatched artifact costs the target site nothing.
pub fn resume_inputs(scan_path: &str) -> Result<Map<String, Value>, ToolError> {
    if scan_path.is_empty() {
        return Err(failed("resume requires the path of a SQLite scan artifact"));
    }
    if !Path::new(scan_path).is_file() {
        return Err(failed(format!(
            "scan to resume does not exist: {scan_path}"
        )));
    }
    let inspected =
        NativeScanSchemaMixin::inspect(Path::new(scan_path), None).map_err(store_failed)?;
    let header = inspected
        .get("scan")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let text = |key: &str| {
        header
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let source_kind = text("source_kind");
    if source_kind != "native" {
        return Err(failed(format!(
            "{scan_path} is a derived {source_kind} artifact, not a crawl that can be resumed"
        )));
    }
    let lifecycle = text("lifecycle");
    let finish_reason = text("finish_reason");
    let budget_stopped = lifecycle == "finished" && finish_reason == "stopped_by_budget";
    if matches!(lifecycle.as_str(), "finished" | "failed") && !budget_stopped {
        return Err(failed(format!(
            "{scan_path} is already {lifecycle} (finish reason: {finish_reason}); \
             there is nothing left to resume"
        )));
    }
    let con = rusqlite::Connection::open_with_flags(scan_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| failed(error.to_string()))?;
    let settings = read_effective_config(&con).map_err(store_failed)?;
    let list_mode = con
        .query_row(
            "SELECT 1 FROM context_items WHERE kind='list_mode' AND item_key='run'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|error| failed(error.to_string()))?
        .is_some();
    if truthy(settings.get("http").and_then(|http| http.get("proxy"))) {
        return Err(failed(
            "proxied scans cannot be resumed from redacted route settings; start a new scan \
             with the original proxy configuration",
        ));
    }
    let mut settings = settings;
    // Pre-budget artifacts made no total-attempt promise. Keep that recorded
    // semantics on resume instead of silently applying a later default.
    let has_max_requests = settings
        .get("limits")
        .and_then(|limits| limits.get("max_requests"))
        .is_some();
    if !has_max_requests {
        settings
            .entry("limits".to_owned())
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .expect("limits is an object")
            .insert("max_requests".to_owned(), json!(0));
    }
    // Pre-retry artifacts predate the retry setting; its default (off) is
    // exactly the behaviour those runs recorded, so backfilling it here is
    // continuation, not a change of rules.
    if settings.get("fetch").is_none() {
        settings.insert("fetch".to_owned(), json!({"retry_server_errors": 0}));
    }
    if !truthy_option_graph(&settings) {
        match settings.get("resources") {
            None => {
                settings.insert(
                    "resources".to_owned(),
                    settings_data::defaults()
                        .get("resources")
                        .cloned()
                        .unwrap_or_else(|| json!({})),
                );
            }
            Some(_) => {
                settings
                    .get_mut("resources")
                    .and_then(Value::as_object_mut)
                    .expect("resources is an object")
                    .insert(
                        "graph".to_owned(),
                        settings_data::defaults()
                            .get("resources")
                            .and_then(|resources| resources.get("graph"))
                            .cloned()
                            .unwrap_or_else(|| json!({})),
                    );
            }
        }
    }
    let has_format_version = settings
        .get("storage")
        .and_then(|storage| storage.get("format_version"))
        .is_some();
    if !has_format_version {
        settings
            .entry("storage".to_owned())
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .expect("storage is an object")
            .insert("format_version".to_owned(), json!(SCAN_V1));
    }
    // The artifact stores credential references redacted, by design, so the
    // settings read back from it are not the settings the interrupted run
    // used. Continuing under them would crawl the same site unauthenticated
    // and report the result as a continuation of an authenticated crawl.
    let http = settings.get("http");
    if truthy(http.and_then(|http| http.get("credential_headers")))
        || truthy(http.and_then(|http| http.get("auth")))
    {
        return Err(failed(format!(
            "{scan_path} was crawled with HTTP authentication, which it stores only in redacted \
             form; a resume cannot restore it. Continue it with the original --config and \
             --scan-out instead of --resume"
        )));
    }
    let fingerprint = crawl_settings::fingerprint(&Value::Object(settings.clone()), &SystemHost)
        .map_err(|error| failed(error.to_string()))?;
    Ok(Map::from_iter([
        ("start_url".to_owned(), json!(text("start_url"))),
        ("settings".to_owned(), Value::Object(settings)),
        ("writer_revision".to_owned(), json!(text("writer_revision"))),
        ("config_fingerprint".to_owned(), json!(fingerprint)),
        ("list_mode".to_owned(), json!(list_mode)),
    ]))
}

/// Whether the effective settings already carry a `resources.graph` entry.
fn truthy_option_graph(settings: &Map<String, Value>) -> bool {
    settings
        .get("resources")
        .and_then(|resources| resources.get("graph"))
        .is_some()
}

/// `crawl_control narrow-scope` on a paused artifact: drop the queued URLs the
/// new pattern rejects, fold the pattern into the stored scope in the same
/// transaction, and journal the operator action.
pub fn paused_narrow_scope(
    scan_path: &str,
    exclude: Option<String>,
    include: Option<String>,
) -> Result<Value, ToolError> {
    if exclude.is_none() && include.is_none() {
        return Err(failed("narrow-scope requires exclude or include"));
    }
    let checked = |pattern: Option<String>| -> Result<Option<String>, ToolError> {
        pattern
            .map(|pattern| checked_pattern(&pattern).map_err(|error| failed(error.to_string())))
            .transpose()
    };
    let exclude = checked(exclude)?;
    let include = checked(include)?;
    let inputs = resume_inputs(scan_path)?;
    if inputs["list_mode"] == json!(true) {
        return Err(failed(
            "narrow-scope applies to a site crawl, not a URL-list scan",
        ));
    }
    let start_url = inputs["start_url"].as_str().unwrap_or_default();
    let host = url_hostname(start_url).to_ascii_lowercase();
    let mut scope =
        Scope::from_config(inputs.get("settings").unwrap_or(&Value::Null)).map_err(failed)?;
    scope.set_start_host(&host);
    let scope = scope
        .narrowed(exclude.as_deref(), include.as_deref())
        .map_err(failed)?;
    let mut scan = NativeScanSchemaMixin::open(Path::new(scan_path), None, None, None, false)
        .map_err(store_failed)?;
    let outcome = scan
        .narrow_scope(
            &|url: &str| {
                scope
                    .rejection(url)
                    .map(|rejection| rejection.reason)
                    .unwrap_or_default()
            },
            exclude.as_deref(),
            include.as_deref(),
        )
        .map_err(store_failed);
    scan.close();
    let outcome = outcome?;
    Ok(json!({
        "ok": true,
        "scan": scan_path,
        "state": "narrowed",
        "exclude": exclude,
        "include": include,
        "dropped": outcome["dropped"],
        "queued": outcome["queued"],
    }))
}

/// `crawl_control add-urls` on a paused artifact: queue the operator's URLs at
/// depth 0, bypassing the scope rules because the operator asked for exactly
/// these targets, and journal the request.
pub fn paused_add_urls(scan_path: &str, urls: Option<&Value>) -> Result<Value, ToolError> {
    let urls = match urls {
        Some(Value::Array(items)) if !items.is_empty() => items.clone(),
        _ => return Err(failed("add-urls requires urls")),
    };
    let cleaned = checked_operator_urls(&Value::Array(urls.clone()))
        .map_err(|error| failed(error.to_string()))?;
    let inputs = resume_inputs(scan_path)?;
    if inputs["list_mode"] == json!(true) {
        return Err(failed(
            "add-urls applies to a site crawl, not a URL-list scan",
        ));
    }
    let mut scan = NativeScanSchemaMixin::open(Path::new(scan_path), None, None, None, false)
        .map_err(store_failed)?;
    let outcome = scan.enqueue_operator(&cleaned).map_err(store_failed);
    scan.close();
    let outcome = outcome?;
    Ok(json!({
        "ok": true,
        "scan": scan_path,
        "state": "urls_added",
        "added": outcome["added"],
        "already_known": outcome["already_known"],
    }))
}

/// The URL's host, lowercased; the stand-in for `urlsplit(url).hostname`.
fn url_hostname(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_owned))
        .unwrap_or_default()
}

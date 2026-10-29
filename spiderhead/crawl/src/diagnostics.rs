//! Offline explanations for unexpectedly small or unfinished native crawls.
//!
//! Everything here reads retained evidence only: no URL is fetched and no crawl is
//! resumed. An optional redacted export keeps the shape of the diagnosis while
//! replacing anything that could identify the site or its visitors.

use spiderhead_core::filesystem::PrivateMode;
use std::io::Write;
use std::path::Path;

use rusqlite::types::ValueRef;
use rusqlite::Connection;
use serde_json::{json, Map, Value};

/// The most decision samples a diagnosis carries.
pub const MAX_DECISIONS: i64 = 20;
const MAX_AUDIT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_JSONL_LINE_BYTES: usize = 8 * 1024 * 1024;
const REDACTED: &str = "[redacted]";

const SAFE_FINISH_REASONS: &[&str] = &[
    "finished",
    "running",
    "url_limit",
    "stopped_by_budget",
    "request_limit",
    "duration_limit",
    "robots_unavailable",
    "errors",
    "interrupted",
    "storage_backpressure",
    "finalization_blocked",
    "offline_reanalysis",
    "operator_requested_stop",
    "capture_finished_no_audit",
];
const SAFE_ERROR_KINDS: &[&str] = &["timeout", "connection", "blocked_redirect", "decoding"];
const SAFE_DECISION_REASONS: &[&str] = &[
    "blocked_by_robots",
    "depth_limit",
    "outside_host",
    "excluded_host",
    "excluded_by_pattern",
    "outside_segment",
    "redirect_off_host",
    "url_too_long",
    "query_variants_limit",
    "nofollow",
    "link_observations_limit",
    "form_observations_limit",
];
const SAFE_MEDIA_TYPES: &[&str] = &[
    "text/html",
    "text/plain",
    "text/xml",
    "text/css",
    "application/json",
    "application/xml",
    "application/pdf",
    "application/javascript",
    "image/jpeg",
    "image/png",
    "image/webp",
    "image/gif",
];

/// Why a diagnosis could not be produced; the text is what the operator reads.
#[derive(Debug)]
pub struct DiagnoseError(pub String);

impl std::fmt::Display for DiagnoseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DiagnoseError {}

type Result<T> = std::result::Result<T, DiagnoseError>;

fn fail<T>(message: impl Into<String>) -> Result<T> {
    Err(DiagnoseError(message.into()))
}

fn sql(error: rusqlite::Error) -> DiagnoseError {
    DiagnoseError(error.to_string())
}

/// What a request for a diagnosis names.
#[derive(Debug, Clone, Default)]
pub struct DiagnoseRequest {
    /// A retained native scan file.
    pub scan: Option<String>,
    /// A legacy run directory holding `audit.json`.
    pub run: Option<String>,
    /// Decision samples to include (1..=[`MAX_DECISIONS`]).
    pub max_decisions: Option<i64>,
    /// Where to write a redacted copy; the file must not exist yet.
    pub export: Option<String>,
}

fn render_failure_kind(reason: &str) -> &'static str {
    let lowered = reason.to_lowercase();
    if [
        "not installed",
        "missing dependency",
        "browser unavailable",
        "executable doesn't exist",
    ]
    .iter()
    .any(|marker| lowered.contains(marker))
    {
        return "missing_dependency";
    }
    if lowered.contains("timeout") || lowered.contains("timed out") {
        return "timeout";
    }
    "other"
}

/// reference `str(value)` for the JSON scalars a settings sample can hold.
fn py_str(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "None".to_owned(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        other => other.to_string(),
    }
}

/// Only the settings needed to explain admission and fetch decisions leave the artifact.
fn settings(config: &Value) -> Value {
    let get = |path: &str| -> Value {
        if let Some(flat) = config.get(path) {
            return flat.clone();
        }
        let mut value = config;
        for part in path.split('.') {
            match value.as_object() {
                Some(object) => match object.get(part) {
                    Some(next) => value = next,
                    None => return Value::Null,
                },
                None => return Value::Null,
            }
        }
        value.clone()
    };
    let scoped = |path: &str| -> Value {
        match get(path) {
            Value::Array(items) => json!({
                "count": items.len(),
                "sample": items.iter().take(10).map(|item| py_str(item).chars().take(200).collect::<String>()).collect::<Vec<_>>(),
                "truncated": items.len() > 10,
            }),
            other => other,
        }
    };
    let mut scope = Map::new();
    for key in [
        "internal",
        "include_patterns",
        "exclude_patterns",
        "exclude_hosts",
        "segments_only",
    ] {
        scope.insert(key.into(), scoped(&format!("scope.{key}")));
    }
    let mut limits = Map::new();
    for key in [
        "max_urls",
        "max_requests",
        "max_depth",
        "max_crawl_seconds",
        "max_query_variants_per_path",
        "max_url_length",
    ] {
        limits.insert(key.into(), get(&format!("limits.{key}")));
    }
    let mut discovery = Map::new();
    for key in ["hyperlinks.crawl", "redirects.crawl", "follow_nofollow"] {
        discovery.insert(key.into(), get(&format!("discovery.{key}")));
    }
    json!({
        "user_agent": get("http.user_agent"),
        "robots_token": get("robots.user_agent_token"),
        "robots_policy": get("robots.policy"),
        "scope": scope,
        "limits": limits,
        "discovery": discovery,
        "rendering_mode": get("rendering.mode"),
    })
}

fn cell(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(number) => json!(number),
        ValueRef::Real(number) => json!(number),
        ValueRef::Text(text) => json!(String::from_utf8_lossy(text)),
        ValueRef::Blob(_) => Value::Null,
    }
}

fn row_object(
    con: &Connection,
    query: &str,
    params: impl rusqlite::Params,
) -> Result<Option<Map<String, Value>>> {
    let mut statement = con.prepare(query).map_err(sql)?;
    let names: Vec<String> = statement
        .column_names()
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    let mut rows = statement.query(params).map_err(sql)?;
    match rows.next().map_err(sql)? {
        None => Ok(None),
        Some(row) => {
            let mut object = Map::new();
            for (index, name) in names.iter().enumerate() {
                object.insert(name.clone(), cell(row.get_ref(index).map_err(sql)?));
            }
            Ok(Some(object))
        }
    }
}

fn counted(con: &Connection, query: &str) -> Result<Vec<(String, i64)>> {
    let mut statement = con.prepare(query).map_err(sql)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                row.get::<_, i64>(1)?,
            ))
        })
        .map_err(sql)?;
    rows.map(|row| row.map_err(sql)).collect()
}

fn count_map(rows: &[(String, i64)], keep: usize) -> Value {
    Value::Object(
        rows.iter()
            .take(keep)
            .map(|(key, count)| (key.clone(), json!(count)))
            .collect(),
    )
}

fn scalar(con: &Connection, query: &str) -> Result<i64> {
    con.query_row(query, [], |row| row.get(0)).map_err(sql)
}

fn text_field<'a>(object: &'a Map<String, Value>, key: &str) -> &'a str {
    object.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn scan_evidence(path: &Path, limit: i64) -> Result<Value> {
    let con = spiderhead_store::scan_artifact::open_scan(path, false, None).map_err(|error| {
        DiagnoseError(match error {
            spiderhead_store::error::StoreError::Invalid(message) => message,
            spiderhead_store::error::StoreError::ScanError(e) => e.message,
            other => other.to_string(),
        })
    })?;
    let header = row_object(&con, "SELECT * FROM scan WHERE singleton=1", [])?
        .ok_or_else(|| DiagnoseError("scan has no header".to_owned()))?;
    let config: Value = serde_json::from_str(text_field(&header, "config_json"))
        .map_err(|error| DiagnoseError(error.to_string()))?;
    let mut frontier = Map::new();
    for (state, count) in counted(&con, "SELECT state,COUNT(*) FROM frontier GROUP BY state")? {
        frontier.insert(state, json!(count));
    }
    for state in ["queued", "inflight", "done", "excluded"] {
        frontier.entry(state.to_owned()).or_insert(json!(0));
    }
    let page_count = scalar(&con, "SELECT COUNT(*) FROM pages")?;
    let link_count = scalar(&con, "SELECT COUNT(*) FROM links")?;
    let decision_count = scalar(&con, "SELECT COUNT(*) FROM decisions")?;
    let reason_rows = counted(
        &con,
        "SELECT reason,COUNT(*) FROM decisions GROUP BY reason ORDER BY COUNT(*) DESC,reason LIMIT 21",
    )?;
    let reasons = count_map(&reason_rows, 20);
    let mut samples = Vec::new();
    {
        let mut statement = con
            .prepare("SELECT decision_id,url,reason,source,depth FROM decisions ORDER BY decision_id LIMIT ?")
            .map_err(sql)?;
        let names: Vec<String> = statement
            .column_names()
            .iter()
            .map(|n| (*n).to_owned())
            .collect();
        let mut rows = statement.query([limit]).map_err(sql)?;
        while let Some(row) = rows.next().map_err(sql)? {
            let mut object = Map::new();
            for (index, name) in names.iter().enumerate() {
                object.insert(name.clone(), cell(row.get_ref(index).map_err(sql)?));
            }
            samples.push(Value::Object(object));
        }
    }
    let type_rows = counted(
        &con,
        "SELECT content_type,COUNT(*) FROM pages GROUP BY content_type ORDER BY COUNT(*) DESC,content_type LIMIT 11",
    )?;
    let content_types = count_map(&type_rows, 10);
    let error_rows = counted(
        &con,
        "SELECT error_kind,COUNT(*) FROM pages WHERE error_kind!='' GROUP BY error_kind ORDER BY COUNT(*) DESC,error_kind LIMIT 11",
    )?;
    let errors = count_map(&error_rows, 10);
    let redirects = scalar(&con, "SELECT COUNT(*) FROM pages WHERE redirect_url!=''")?;
    let start = row_object(
        &con,
        "SELECT p.status_code,p.content_type,p.outlinks,p.error_kind,p.representation FROM pages p JOIN urls u USING(url_id) WHERE u.url=?",
        [text_field(&header, "start_url")],
    )?;
    let runtime: Option<(i64, f64, String)> = con
        .query_row(
            "SELECT max_depth_reached,elapsed_seconds,throttle_state_json FROM resume_state WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map(Some)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
        .map_err(sql)?;
    let mut render_requests = 0i64;
    let mut unprobed = 0i64;
    let mut budget_exhausted = false;
    let mut failure_reasons = Map::new();
    {
        let mut statement = con
            .prepare("SELECT payload_json FROM context_items WHERE kind='render_phase_summary' ORDER BY item_key")
            .map_err(sql)?;
        let payloads: Vec<String> = statement
            .query_map([], |row| row.get(0))
            .map_err(sql)?
            .collect::<std::result::Result<_, _>>()
            .map_err(sql)?;
        for payload in payloads {
            let payload: Value =
                serde_json::from_str(&payload).map_err(|error| DiagnoseError(error.to_string()))?;
            render_requests += payload
                .get("render_requests")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            unprobed += payload
                .get("patterns_unprobed")
                .and_then(Value::as_array)
                .map_or(0, |list| list.len() as i64);
            let truthy = |name: &str| match payload.get(name) {
                None | Some(Value::Null) | Some(Value::Bool(false)) => false,
                Some(Value::Number(n)) => n.as_f64() != Some(0.0),
                Some(_) => true,
            };
            budget_exhausted |=
                truthy("render_budget_exhausted") || truthy("time_budget_exhausted");
            if let Some(reasons) = payload
                .get("patterns_unprobed_reasons")
                .and_then(Value::as_object)
            {
                for reason in reasons.values() {
                    let kind = render_failure_kind(&py_str(reason));
                    let next = failure_reasons
                        .get(kind)
                        .and_then(Value::as_i64)
                        .unwrap_or(0)
                        + 1;
                    failure_reasons.insert(kind.to_owned(), json!(next));
                }
            }
        }
    }
    let render = json!({
        "render_requests": render_requests,
        "unprobed_patterns": unprobed,
        "budget_exhausted": budget_exhausted,
        "failure_reasons": failure_reasons,
    });
    let audit_document: Option<String> = con
        .query_row(
            "SELECT document_json FROM audit WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
        .map_err(sql)?;
    let audit_run = match &audit_document {
        Some(text) => serde_json::from_str::<Value>(text)
            .map_err(|error| DiagnoseError(error.to_string()))?
            .get("run")
            .cloned()
            .unwrap_or_else(|| json!({})),
        None => json!({}),
    };
    let event_table = scalar(
        &con,
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='scan_events'",
    )? > 0;
    let provider_failures = if event_table {
        let mut statement = con
            .prepare("SELECT payload_json FROM scan_events WHERE event_type='provider_enrichment'")
            .map_err(sql)?;
        let payloads: Vec<String> = statement
            .query_map([], |row| row.get(0))
            .map_err(sql)?
            .collect::<std::result::Result<_, _>>()
            .map_err(sql)?;
        let mut failed = 0i64;
        for payload in payloads {
            let payload: Value =
                serde_json::from_str(&payload).map_err(|error| DiagnoseError(error.to_string()))?;
            if matches!(
                payload.get("state").and_then(Value::as_str),
                Some("failed" | "error" | "unavailable" | "timeout" | "blocked")
            ) {
                failed += 1;
            }
        }
        json!(failed)
    } else {
        Value::Null
    };
    let sample_len = samples.len() as i64;
    let source_kind = text_field(&header, "source_kind").to_owned();
    let partial = header
        .get("crawl_partial")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        != 0;
    let limitations = serde_json::from_str::<Value>(text_field(&header, "limitations_json"))
        .map_err(|error| DiagnoseError(error.to_string()))?;
    let throttle = runtime
        .as_ref()
        .map(|(_, _, text)| serde_json::from_str::<Value>(text).unwrap_or(Value::Null));
    let render_state = if render_requests != 0 || unprobed != 0 {
        "recorded"
    } else if settings(&config)["rendering_mode"] == json!("raw") {
        "not_requested"
    } else {
        "not_recorded"
    };
    Ok(json!({
        "source": {
            "kind": "scan",
            "source_kind": source_kind,
            "scan_uuid": header.get("scan_uuid").cloned().unwrap_or(Value::Null),
            "format_version": header.get("format_version").cloned().unwrap_or(Value::Null),
            "path": path.to_string_lossy(),
            "lifecycle": header.get("lifecycle").cloned().unwrap_or(Value::Null),
            "finish_reason": header.get("finish_reason").cloned().unwrap_or(Value::Null),
            "crawl_partial": partial,
            "start_url": header.get("start_url").cloned().unwrap_or(Value::Null),
        },
        "settings": settings(&config),
        "observed": {
            "page_records": page_count,
            "links_recorded": link_count,
            "decisions_recorded": decision_count,
            "site_total_urls": null,
            "frontier": if source_kind == "native" { Value::Object(frontier) } else { Value::Null },
            "content_types": content_types,
            "content_types_truncated": type_rows.len() > 10,
            "page_errors": errors,
            "page_error_groups_truncated": error_rows.len() > 10,
            "provider_failures": provider_failures,
            "redirects_recorded": redirects,
            "max_depth_reached": runtime.as_ref().map_or(Value::Null, |r| json!(r.0)),
            "elapsed_seconds": runtime.as_ref().map_or(Value::Null, |r| json!(r.1)),
            "requests_used": throttle.as_ref().and_then(|t| t.get("requests_used").cloned()).unwrap_or(Value::Null),
            "start_page": start.map_or(Value::Null, Value::Object),
            "render": render,
            "requires_rendering": audit_run.get("requires_rendering").cloned().unwrap_or(Value::Null),
        },
        "decisions": {
            "by_reason": reasons,
            "sample": samples,
            "omitted_from_sample": (decision_count - sample_len).max(0),
            "reason_groups_truncated": reason_rows.len() > 20,
        },
        "limitations_count": limitations.as_array().map_or(0, Vec::len),
        "coverage": {
            "pages": "recorded",
            "links": if partial { "partial" } else { "recorded" },
            "decisions": "recorded",
            "frontier": match source_kind.as_str() {
                "native" => "recorded",
                "reanalysis" => "historical",
                _ => "unavailable",
            },
            "audit": if audit_document.is_some() { "recorded" } else { "unavailable" },
            "provider_events": if event_table { "recorded" } else { "unavailable" },
            "render": render_state,
        },
    }))
}

/// One JSON-lines file: its record count, the first `limit` records (or decision
/// samples) and, for decisions, the per-reason counts.
struct Jsonl {
    total: i64,
    sample: Vec<Value>,
    counts: Vec<(String, i64)>,
}

fn read_jsonl(path: &Path, limit: usize, decision: bool) -> Result<Jsonl> {
    let mut out = Jsonl {
        total: 0,
        sample: Vec::new(),
        counts: Vec::new(),
    };
    if !path.is_file() {
        return Ok(out);
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let text = std::fs::read_to_string(path).map_err(|e| DiagnoseError(e.to_string()))?;
    for (index, line) in text.split_inclusive('\n').enumerate() {
        let number = index + 1;
        if line.trim().is_empty() {
            continue;
        }
        if line.len() > MAX_JSONL_LINE_BYTES {
            return fail(format!("{name}:{number} exceeds the 8 MiB line budget"));
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return fail(format!("{name}:{number} is invalid JSON"));
        };
        let Some(object) = value.as_object() else {
            return fail(format!("{name}:{number} must be an object"));
        };
        out.total += 1;
        if decision {
            if let Some(reason) = object.get("reason").and_then(Value::as_str) {
                match out.counts.iter_mut().find(|(key, _)| key == reason) {
                    Some(slot) => slot.1 += 1,
                    None => out.counts.push((reason.to_owned(), 1)),
                }
            }
            if out.sample.len() < limit {
                let mut item = Map::new();
                item.insert("decision_id".into(), json!(out.total));
                for key in ["url", "reason", "source", "depth"] {
                    item.insert(key.into(), object.get(key).cloned().unwrap_or(Value::Null));
                }
                out.sample.push(Value::Object(item));
            }
        } else if out.sample.len() < limit {
            out.sample.push(value.clone());
        }
    }
    Ok(out)
}

/// `Counter.most_common(n)`: highest count first, ties in first-seen order.
fn most_common(counts: &[(String, i64)], n: usize) -> Vec<(String, i64)> {
    let mut ordered: Vec<(usize, &(String, i64))> = counts.iter().enumerate().collect();
    ordered.sort_by(|a, b| b.1 .1.cmp(&a.1 .1).then(a.0.cmp(&b.0)));
    ordered
        .into_iter()
        .take(n)
        .map(|(_, pair)| pair.clone())
        .collect()
}

fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(Value::Bool(true)) => true,
    }
}

struct LegacyPages {
    total: Value,
    start: Value,
    content_types: Value,
    errors: Value,
    redirects: Value,
    types_truncated: bool,
    errors_truncated: bool,
}

fn legacy_pages(path: &Path, start_url: Option<&Value>) -> Result<LegacyPages> {
    if !path.is_file() {
        return Ok(LegacyPages {
            total: Value::Null,
            start: Value::Null,
            content_types: json!({}),
            errors: json!({}),
            redirects: Value::Null,
            types_truncated: false,
            errors_truncated: false,
        });
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let text = std::fs::read_to_string(path).map_err(|e| DiagnoseError(e.to_string()))?;
    let (mut total, mut redirects) = (0i64, 0i64);
    let mut first = Value::Null;
    let mut content_types: Vec<(String, i64)> = Vec::new();
    let mut errors: Vec<(String, i64)> = Vec::new();
    let bump =
        |list: &mut Vec<(String, i64)>, key: String| match list.iter_mut().find(|(k, _)| *k == key)
        {
            Some(slot) => slot.1 += 1,
            None => list.push((key, 1)),
        };
    for (index, line) in text.split_inclusive('\n').enumerate() {
        let number = index + 1;
        if line.trim().is_empty() {
            continue;
        }
        if line.len() > MAX_JSONL_LINE_BYTES {
            return fail(format!("{name}:{number} exceeds the 8 MiB line budget"));
        }
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            return fail(format!("{name}:{number} is invalid JSON"));
        };
        let Some(object) = row.as_object() else {
            return fail(format!("{name}:{number} must be an object"));
        };
        total += 1;
        if start_url.is_some_and(|start| object.get("url") == Some(start))
            || (start_url.is_none() && object.get("url").is_none())
        {
            let mut selected = Map::new();
            for key in [
                "status_code",
                "content_type",
                "outlinks",
                "error_kind",
                "representation",
            ] {
                selected.insert(key.into(), object.get(key).cloned().unwrap_or(Value::Null));
            }
            first = Value::Object(selected);
        }
        let content = match object.get("content_type") {
            Some(value) if truthy(Some(value)) => py_str(value),
            _ => "unknown".to_owned(),
        };
        bump(&mut content_types, content);
        if let Some(kind) = object.get("error_kind").filter(|v| truthy(Some(v))) {
            bump(&mut errors, py_str(kind));
        }
        if truthy(object.get("redirect_url")) {
            redirects += 1;
        }
    }
    Ok(LegacyPages {
        total: json!(total),
        start: first,
        content_types: count_map(&most_common(&content_types, 10), 10),
        errors: count_map(&most_common(&errors, 10), 10),
        redirects: json!(redirects),
        types_truncated: content_types.len() > 10,
        errors_truncated: errors.len() > 10,
    })
}

fn render_summary(value: Option<&Value>) -> Value {
    let Some(object) = value.and_then(Value::as_object) else {
        return Value::Null;
    };
    let unprobed = object.get("patterns_unprobed").and_then(Value::as_array);
    let mut failure_reasons: Vec<(String, i64)> = Vec::new();
    if let Some(reasons) = object
        .get("patterns_unprobed_reasons")
        .and_then(Value::as_object)
    {
        for reason in reasons.values() {
            let kind = render_failure_kind(&py_str(reason)).to_owned();
            match failure_reasons.iter_mut().find(|(k, _)| *k == kind) {
                Some(slot) => slot.1 += 1,
                None => failure_reasons.push((kind, 1)),
            }
        }
    }
    json!({
        "render_requests": object.get("render_requests").cloned().unwrap_or(Value::Null),
        "unprobed_patterns": unprobed.map_or(Value::Null, |list| json!(list.len())),
        "budget_exhausted": truthy(object.get("render_budget_exhausted")) || truthy(object.get("time_budget_exhausted")),
        "failure_reasons": count_map(&failure_reasons, usize::MAX),
    })
}

fn run_evidence(path: &Path, limit: usize) -> Result<Value> {
    let audit_path = path.join("audit.json");
    if !path.is_dir() || !audit_path.is_file() {
        return fail("run must contain audit.json; unfinished legacy runs lack enough context");
    }
    let size = std::fs::metadata(&audit_path)
        .map_err(|e| DiagnoseError(e.to_string()))?
        .len();
    if size > MAX_AUDIT_BYTES {
        return fail("audit.json exceeds the 64 MiB diagnostic read budget");
    }
    let text = std::fs::read_to_string(&audit_path).map_err(|e| DiagnoseError(e.to_string()))?;
    let audit: Value = serde_json::from_str(&text).map_err(|e| DiagnoseError(e.to_string()))?;
    let Some(run) = audit.get("run").filter(|run| run.is_object()) else {
        return fail("audit.json has no run object");
    };
    if !matches!(
        run.get("input_mode").and_then(Value::as_str),
        Some("crawl" | "crawl-list")
    ) {
        return fail("run is not a native crawl audit");
    }
    let pages = legacy_pages(&path.join("pages.jsonl"), run.get("source"))?;
    let decisions = read_jsonl(&path.join("decisions.jsonl"), limit, true)?;
    let links = path.join("links.jsonl");
    let links_count = if links.is_file() {
        json!(read_jsonl(&links, 0, false)?.total)
    } else {
        Value::Null
    };
    let config = match run.get("crawl_config") {
        None | Some(Value::Null) => json!({}),
        Some(value) if value.is_object() => value.clone(),
        Some(value) if !truthy(Some(value)) => json!({}),
        Some(_) => return fail("audit.json crawl_config must be an object"),
    };
    let reasons = most_common(&decisions.counts, 20);
    let has = |name: &str| path.join(name).is_file();
    let partial = run.get("crawl_partial").cloned().unwrap_or(Value::Null);
    let omitted = (decisions.total - decisions.sample.len() as i64).max(0);
    let sample = decisions.sample;
    Ok(json!({
        "source": {
            "kind": "run",
            "source_kind": "legacy_directory",
            "path": path.to_string_lossy(),
            "lifecycle": if run.get("crawl_finish_reason").and_then(Value::as_str) == Some("finished") { "finished" } else { "interrupted" },
            "finish_reason": run.get("crawl_finish_reason").cloned().unwrap_or(Value::Null),
            "crawl_partial": partial,
            "start_url": run.get("source").cloned().unwrap_or(Value::Null),
        },
        "settings": settings(&config),
        "observed": {
            "page_records": pages.total,
            "links_recorded": links_count,
            "decisions_recorded": if has("decisions.jsonl") { json!(decisions.total) } else { Value::Null },
            "site_total_urls": null,
            "frontier": null,
            "content_types": pages.content_types,
            "content_types_truncated": pages.types_truncated,
            "page_errors": pages.errors,
            "page_error_groups_truncated": pages.errors_truncated,
            "provider_failures": null,
            "redirects_recorded": pages.redirects,
            "max_depth_reached": null,
            "elapsed_seconds": null,
            "requests_used": null,
            "start_page": pages.start,
            "render": render_summary(run.get("render_escalation")),
            "requires_rendering": run.get("requires_rendering").cloned().unwrap_or(Value::Null),
        },
        "decisions": {
            "by_reason": count_map(&reasons, 20),
            "sample": sample,
            "omitted_from_sample": omitted,
            "reason_groups_truncated": decisions.counts.len() > 20,
        },
        "limitations_count": run.get("checks_skipped").and_then(Value::as_array).map_or(0, Vec::len),
        "coverage": {
            "pages": if has("pages.jsonl") { "recorded" } else { "unavailable" },
            "links": if partial == json!(true) { "partial" } else if has("links.jsonl") { "recorded" } else { "unavailable" },
            "decisions": if has("decisions.jsonl") { "recorded" } else { "unavailable" },
            "frontier": "unavailable",
            "audit": "recorded",
            "provider_events": "unavailable",
            "render": if truthy(run.get("render_escalation")) { "recorded" } else { "unknown" },
        },
    }))
}

fn at<'a>(value: &'a Value, path: &[&str]) -> &'a Value {
    let mut current = value;
    for key in path {
        current = current.get(*key).unwrap_or(&Value::Null);
    }
    current
}

fn add(findings: &mut Vec<Value>, code: &str, conclusion: String, evidence: &[&str], next: &str) {
    findings.push(json!({
        "code": code,
        "conclusion": conclusion,
        "evidence": evidence,
        "next_step": next,
    }));
}

fn explain(result: &Value) -> Vec<Value> {
    let source = &result["source"];
    let obs = &result["observed"];
    let coverage = &result["coverage"];
    let reasons: Vec<(String, i64)> = result["decisions"]["by_reason"]
        .as_object()
        .map(|o| {
            o.iter()
                .map(|(k, v)| (k.clone(), v.as_i64().unwrap_or(0)))
                .collect()
        })
        .unwrap_or_default();
    let mut findings: Vec<Value> = Vec::new();
    let complete_discovery = source.get("crawl_partial") == Some(&json!(false))
        && coverage["pages"] == "recorded"
        && coverage["links"] == "recorded"
        && coverage["decisions"] == "recorded";
    if !complete_discovery {
        add(
            &mut findings,
            "coverage_gap",
            "Discovery or decision evidence is partial or unavailable; zero observed links cannot prove the crawl exhausted the site's routes.".into(),
            &["source.crawl_partial", "coverage.pages", "coverage.links", "coverage.decisions"],
            "Inspect the missing or omitted evidence before treating a one-page result as complete.",
        );
    }
    let lifecycle = source["lifecycle"].as_str().unwrap_or_default();
    let finish = source["finish_reason"].as_str();
    if lifecycle == "running" {
        add(
            &mut findings,
            "worker_state_unverified",
            "The artifact still says running; a saved file cannot prove a worker is alive.".into(),
            &["source.lifecycle", "observed.frontier"],
            "Check the process and scan status before resuming or requeueing.",
        );
    }
    if matches!(lifecycle, "failed" | "interrupted")
        && !matches!(
            finish,
            Some("url_limit" | "request_limit" | "duration_limit" | "robots_unavailable")
        )
    {
        add(
            &mut findings,
            "worker_or_run_failure",
            "The retained run ended without a completed crawl; its exact cause needs the recorded failure evidence.".into(),
            &["source.lifecycle", "source.finish_reason", "observed.page_errors"],
            "Inspect the failed URL and process records before a focused retry.",
        );
    }
    if matches!(
        finish,
        Some("url_limit" | "stopped_by_budget" | "request_limit" | "duration_limit")
    ) {
        add(
            &mut findings,
            "budget_exhausted",
            format!(
                "The recorded finish reason is {}.",
                finish.unwrap_or_default()
            ),
            &["source.finish_reason", "settings.limits"],
            "Review the explicit crawl budget and scope before another run.",
        );
    }
    if finish == Some("robots_unavailable") {
        add(
            &mut findings,
            "robots_unavailable",
            "The run stopped because robots evidence was unavailable.".into(),
            &["source.finish_reason", "settings.robots_policy"],
            "Check robots.txt reachability under the recorded user-agent; do not bypass it silently.",
        );
    }
    let robots: i64 = reasons
        .iter()
        .filter(|(r, _)| r.contains("robots"))
        .map(|(_, c)| c)
        .sum();
    if robots != 0 {
        add(
            &mut findings,
            "robots_exclusion",
            format!("{robots} recorded decisions mention robots policy."),
            &["decisions.by_reason", "settings.robots_policy"],
            "Inspect the cited rules and user-agent token; do not bypass robots silently.",
        );
    }
    let scope: i64 = reasons
        .iter()
        .filter(|(r, _)| {
            ["scope", "host", "segment", "include", "exclude"]
                .iter()
                .any(|w| r.contains(w))
        })
        .map(|(_, c)| c)
        .sum();
    if scope != 0 {
        add(
            &mut findings,
            "scope_exclusion",
            format!("{scope} recorded decisions exclude discovered URLs by scope."),
            &["decisions.by_reason", "settings.scope"],
            "Inspect the excluded URL samples and change the named scope setting explicitly if intended.",
        );
    }
    if reasons.iter().any(|(r, _)| r.contains("depth")) {
        add(
            &mut findings,
            "depth_exclusion",
            "Recorded decisions stopped discovery at a depth limit.".into(),
            &["decisions.by_reason", "settings.limits.max_depth"],
            "Review max_depth and the excluded decision samples.",
        );
    }
    if reasons.iter().any(|(r, _)| r.contains("redirect")) || truthy(obs.get("redirects_recorded"))
    {
        add(
            &mut findings,
            "redirect_discovery",
            "Redirect outcomes are present in retained evidence.".into(),
            &[
                "observed.redirects_recorded",
                "decisions.by_reason",
                "settings.discovery.redirects",
            ],
            "Inspect redirect targets and the explicit discovery.redirects.crawl setting.",
        );
    }
    if truthy(obs.get("page_errors")) {
        add(
            &mut findings,
            "fetch_failure",
            "Page fetch failures were recorded.".into(),
            &["observed.page_errors"],
            "Inspect the failed URL records and retry only after the transport or provider is available.",
        );
    }
    if truthy(obs.get("provider_failures")) {
        add(
            &mut findings,
            "provider_failure",
            "The saved event timeline records failed provider operations.".into(),
            &["observed.provider_failures", "coverage.provider_events"],
            "Inspect the provider event and dependency status before retrying enrichment.",
        );
    }
    let first = if truthy(obs.get("start_page")) {
        obs["start_page"].clone()
    } else {
        json!({})
    };
    if truthy(first.get("content_type")) && !py_str(&first["content_type"]).starts_with("text/html")
    {
        add(
            &mut findings,
            "non_html_start",
            "The start response was not recorded as HTML.".into(),
            &["observed.start_page.content_type"],
            "Check the target URL and Content-Type before expecting link discovery.",
        );
    }
    if obs.get("requires_rendering") == Some(&json!(true)) {
        add(
            &mut findings,
            "render_eligibility",
            "The saved audit marked the start page as requiring rendering.".into(),
            &["observed.requires_rendering", "settings.rendering_mode"],
            "Run a focused render-check or explicitly configure rendering after verifying the page.",
        );
    }
    let render = obs.get("render").cloned().unwrap_or(Value::Null);
    let render = if truthy(Some(&render)) {
        render
    } else {
        json!({})
    };
    if render.is_object()
        && (truthy(render.get("unprobed_patterns")) || truthy(render.get("patterns_unprobed")))
    {
        add(
            &mut findings,
            "render_unavailable",
            "Some rendering probes produced no verdict.".into(),
            &["observed.render"],
            "Check browser availability and the recorded probe failure; do not treat raw-only pages as rendered.",
        );
    }
    if render.is_object()
        && truthy(Some(at(
            &render,
            &["failure_reasons", "missing_dependency"],
        )))
    {
        add(
            &mut findings,
            "dependency_unavailable",
            "A rendering dependency was recorded as unavailable.".into(),
            &["observed.render.failure_reasons"],
            "Install or restore the optional local browser dependency before a focused render retry.",
        );
    }
    if render.is_object()
        && (truthy(render.get("budget_exhausted"))
            || truthy(render.get("render_budget_exhausted"))
            || truthy(render.get("time_budget_exhausted")))
    {
        add(
            &mut findings,
            "render_budget",
            "The recorded render budget was exhausted.".into(),
            &["observed.render"],
            "Review rendering.escalation limits before a focused diagnostic run.",
        );
    }
    if obs["page_records"] == json!(1)
        && finish == Some("finished")
        && first.get("outlinks") == Some(&json!(0))
        && reasons.is_empty()
        && !truthy(obs.get("page_errors"))
        && complete_discovery
    {
        add(
            &mut findings,
            "complete_one_page",
            "One page finished with no recorded internal outlinks; site-wide URL total is unknown.".into(),
            &["observed.start_page.outlinks", "source.finish_reason", "decisions.by_reason"],
            "Inspect source links or run a focused render-check if client-side navigation is expected.",
        );
    } else if obs["page_records"] == json!(1) && first.get("outlinks") == Some(&json!(0)) {
        add(
            &mut findings,
            "no_discovery",
            "The fetched start page recorded no internal outlinks.".into(),
            &["observed.start_page.outlinks", "source.finish_reason"],
            "Inspect eligible anchors, scope decisions and a focused render-check if navigation is client-side.",
        );
    }
    if findings.is_empty() {
        add(
            &mut findings,
            "insufficient_evidence",
            "No recorded decision uniquely explains the low progress.".into(),
            &["coverage", "observed"],
            "Inspect the start page and exact decision samples; avoid changing crawler policy without evidence.",
        );
    }
    findings
}

fn nonneg_int(value: Option<&Value>) -> Value {
    match value.and_then(Value::as_i64) {
        Some(number) if number >= 0 && !value.is_some_and(Value::is_boolean) => json!(number),
        _ => Value::Null,
    }
}

fn safe_groups(groups: &Value, allowed: &[&str]) -> Value {
    let mut out: Vec<(String, i64)> = Vec::new();
    if let Some(object) = groups.as_object() {
        for (key, count) in object {
            let key = if allowed.contains(&key.as_str()) {
                key.clone()
            } else {
                REDACTED.to_owned()
            };
            let count = count.as_i64().unwrap_or(0);
            match out.iter_mut().find(|(k, _)| *k == key) {
                Some(slot) => slot.1 += count,
                None => out.push((key, count)),
            }
        }
    }
    count_map(&out, usize::MAX)
}

fn redact(result: &Value) -> Value {
    let mut redacted = result.clone();
    let source = redacted["source"].as_object_mut().expect("source object");
    for key in ["path", "start_url", "scan_uuid"] {
        if source.contains_key(key) {
            source.insert(key.into(), json!(REDACTED));
        }
    }
    let finish_unsafe = match source.get("finish_reason") {
        None | Some(Value::Null) => false,
        Some(Value::String(text)) => !SAFE_FINISH_REASONS.contains(&text.as_str()),
        Some(_) => true,
    };
    if finish_unsafe {
        source.insert("finish_reason".into(), json!(REDACTED));
    }
    if !source.get("crawl_partial").is_some_and(Value::is_boolean) {
        source.insert("crawl_partial".into(), Value::Null);
    }
    let settings = redacted["settings"]
        .as_object_mut()
        .expect("settings object");
    for key in ["user_agent", "robots_token", "scope"] {
        settings.insert(key.into(), json!(REDACTED));
    }
    if !matches!(
        settings.get("robots_policy").and_then(Value::as_str),
        Some("respect" | "report_only" | "ignore")
    ) {
        settings.insert("robots_policy".into(), json!(REDACTED));
    }
    if !matches!(
        settings.get("rendering_mode").and_then(Value::as_str),
        Some("raw" | "js" | "legacy_fragment")
    ) {
        settings.insert("rendering_mode".into(), json!(REDACTED));
    }
    let limits: Map<String, Value> = settings["limits"]
        .as_object()
        .map(|o| {
            o.iter()
                .map(|(k, v)| (k.clone(), nonneg_int(Some(v))))
                .collect()
        })
        .unwrap_or_default();
    settings.insert("limits".into(), Value::Object(limits));
    let discovery: Map<String, Value> = settings["discovery"]
        .as_object()
        .map(|o| {
            o.iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        if v.is_boolean() {
                            v.clone()
                        } else {
                            Value::Null
                        },
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    settings.insert("discovery".into(), Value::Object(discovery));
    let observed = redacted["observed"]
        .as_object_mut()
        .expect("observed object");
    if !observed
        .get("requires_rendering")
        .is_some_and(Value::is_boolean)
    {
        observed.insert("requires_rendering".into(), Value::Null);
    }
    let mut media: Vec<(String, i64)> = Vec::new();
    if let Some(types) = observed["content_types"].as_object() {
        for (key, count) in types {
            let kind = key
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_lowercase();
            let kind = if SAFE_MEDIA_TYPES.contains(&kind.as_str()) {
                kind
            } else {
                REDACTED.to_owned()
            };
            let count = count.as_i64().unwrap_or(0);
            match media.iter_mut().find(|(k, _)| *k == kind) {
                Some(slot) => slot.1 += count,
                None => media.push((kind, count)),
            }
        }
    }
    observed.insert("content_types".into(), count_map(&media, usize::MAX));
    let errors = safe_groups(&observed["page_errors"], SAFE_ERROR_KINDS);
    observed.insert("page_errors".into(), errors);
    if truthy(observed.get("start_page")) {
        let start = observed["start_page"]
            .as_object_mut()
            .expect("start page object");
        start.insert("content_type".into(), json!(REDACTED));
        start.insert("error_kind".into(), json!(REDACTED));
        for key in ["status_code", "outlinks"] {
            let value = nonneg_int(start.get(key));
            start.insert(key.into(), value);
        }
        if !matches!(
            start.get("representation").and_then(Value::as_str),
            Some("static" | "rendered" | "legacy_fragment")
        ) {
            start.insert("representation".into(), json!(REDACTED));
        }
    }
    if let Some(render) = observed.get_mut("render").and_then(Value::as_object_mut) {
        for key in ["render_requests", "unprobed_patterns"] {
            let value = nonneg_int(render.get(key));
            render.insert(key.into(), value);
        }
        let exhausted = render.get("budget_exhausted") == Some(&json!(true));
        render.insert("budget_exhausted".into(), json!(exhausted));
        let reasons = render
            .get("failure_reasons")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let reasons = if truthy(Some(&reasons)) {
            reasons
        } else {
            json!({})
        };
        render.insert(
            "failure_reasons".into(),
            safe_groups(&reasons, &["missing_dependency", "timeout", "other"]),
        );
    }
    let by_reason = safe_groups(&redacted["decisions"]["by_reason"], SAFE_DECISION_REASONS);
    redacted["decisions"]["by_reason"] = by_reason;
    if let Some(samples) = redacted["decisions"]["sample"].as_array_mut() {
        for item in samples {
            let Some(item) = item.as_object_mut() else {
                continue;
            };
            item.insert("url".into(), json!(REDACTED));
            item.insert("source".into(), json!(REDACTED));
            if !matches!(item.get("reason").and_then(Value::as_str), Some(r) if SAFE_DECISION_REASONS.contains(&r))
            {
                item.insert("reason".into(), json!(REDACTED));
            }
            let depth = nonneg_int(item.get("depth"));
            item.insert("depth".into(), depth);
        }
    }
    redacted
}

/// Summarize stored evidence only; no URL is fetched and no crawl is resumed.
pub fn diagnose(request: &DiagnoseRequest) -> Result<Value> {
    let scan = request.scan.as_deref().filter(|text| !text.is_empty());
    let run = request.run.as_deref().filter(|text| !text.is_empty());
    if scan.is_some() == run.is_some() {
        return fail("provide exactly one of scan or run");
    }
    let limit = request.max_decisions.unwrap_or(MAX_DECISIONS);
    if !(1..=MAX_DECISIONS).contains(&limit) {
        return fail(format!("max_decisions must be 1..{MAX_DECISIONS}"));
    }
    let mut result = match (scan, run) {
        (Some(scan), _) => scan_evidence(Path::new(scan), limit)?,
        (_, Some(run)) => run_evidence(Path::new(run), limit as usize)?,
        _ => return fail("provide exactly one of scan or run"),
    };
    result["schema_version"] = json!("crawl_diagnostics.v1");
    result["diagnoses"] = Value::Array(explain(&result));
    if let Some(export) = request.export.as_deref() {
        let redacted = redact(&result);
        let path = Path::new(export);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .private_mode(0o600)
            .open(path)
            .map_err(|error| match error.raw_os_error() {
                Some(code) => DiagnoseError(format!(
                    "[Errno {code}] {}: '{export}'",
                    error
                        .to_string()
                        .split(" (os error")
                        .next()
                        .unwrap_or_default()
                )),
                None => DiagnoseError(error.to_string()),
            })?;
        let text =
            serde_json::to_string_pretty(&redacted).map_err(|e| DiagnoseError(e.to_string()))?;
        let written = file
            .write_all(text.as_bytes())
            .and_then(|()| file.sync_all());
        if let Err(error) = written {
            let _ = std::fs::remove_file(path);
            return fail(error.to_string());
        }
        result["redacted_export"] = json!(export);
    }
    Ok(result)
}

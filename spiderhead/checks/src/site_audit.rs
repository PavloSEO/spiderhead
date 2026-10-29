//! Bulk site auditing: one call, one contract-stable document.
//!
//! Site-level tools run once, page-level tools run for every selected URL, and
//! all results are assembled into a `spiderhead.site-audit/1` document consumed
//! by the report generators. A failure in one tool never aborts the audit: the
//! failed check and its reason are recorded in `summary.tools_failed` instead,
//! because an audit that silently loses half its checks is more dangerous than
//! one that states exactly which evidence is missing.

use std::collections::{BTreeMap, HashSet};

use serde_json::{json, Map, Value as Json};
use spiderhead_core::net::normalize_url;
use spiderhead_core::timeutil::{stamp, Format};
use spiderhead_parse::py_str::py_repr;
use spiderhead_parse::py_url::urlsplit;

use crate::cwv::{assess, truthy};

pub const SCHEMA: &str = "spiderhead.site-audit/1";

/// Finding severity is assigned here by policy; it is not measured by the
/// source tool. Ordering is significant: the first matching rule wins, so
/// critical rules must precede warnings.
const SEVERITY_RULES: [(&str, &str); 24] = [
    // The page is physically unavailable to a crawler or removes itself from the index.
    ("empty page", "critical"),
    ("empty container", "critical"),
    ("canonicaliz", "critical"),
    ("redirect to the main site", "critical"),
    ("noindex", "critical"),
    ("contains effectively no page copy", "critical"),
    ("unavailable or return errors", "critical"),
    ("5xx", "critical"),
    // The issue can hinder ranking or waste crawl budget.
    ("only after JavaScript", "warning"),
    ("content matches", "warning"),
    ("same title", "warning"),
    ("city is missing", "warning"),
    ("wasting crawl budget", "warning"),
    ("title changes after JavaScript", "warning"),
    ("canonical", "warning"),
    ("affiliate site", "warning"),
    ("two schema types at once", "warning"),
    // The markup exists and is void: the owner believes they publish it.
    ("cannot be parsed", "warning"),
    ("json-ld block(s) on the page are invalid", "warning"),
    // These missing directives are visible to both search engines and browsers.
    ("missing strict-transport-security", "warning"),
    ("missing content-security-policy", "warning"),
    ("missing meta description", "warning"),
    ("no analytics integration was detected", "warning"),
    ("neither etag nor last-modified", "warning"),
    // Every other observation remains a notice.
];

/// Site-level tools run once against the domain or homepage.
pub const SITE_TOOLS: [&str; 10] = [
    "domain_profile",
    "cdn_check",
    "tech_detect",
    "security_check",
    "robots_check",
    "ai_bots_check",
    "llms_txt_check",
    "regions_check",
    "render_check",
    "sitemap_crawl",
];

/// Page-level tools run once for every URL in the selected page set.
pub const PAGE_TOOLS: [&str; 3] = ["parse", "schema_check", "social_meta_check"];

/// Classify finding text with the documented, explicitly heuristic policy.
pub fn classify(text: &str) -> &'static str {
    let low = text.to_lowercase();
    for (marker, level) in SEVERITY_RULES {
        if low.contains(&marker.to_lowercase()) {
            return level;
        }
    }
    "notice"
}

/// One tool invocation: which URL (or domain, or sitemap address) to check.
pub struct ToolCall<'a> {
    pub url: &'a str,
    pub domain: &'a str,
    pub render: bool,
    pub sitemap_url: &'a str,
}

/// The tool registry the audit composes. `Err` carries the full
/// `TypeName: message` text the orchestrator boundary records verbatim.
pub trait SiteTools {
    fn has(&self, tool: &str) -> bool;
    fn call(&self, tool: &str, call: &ToolCall) -> Result<Json, String>;
}

/// String conversion over a JSON value: strings pass through, `None`/bools take
/// their capitalised word, everything else prints as compact JSON.
pub(crate) fn py_str_of(value: &Json) -> String {
    match value {
        Json::String(text) => text.clone(),
        Json::Null => "None".to_string(),
        Json::Bool(true) => "True".to_string(),
        Json::Bool(false) => "False".to_string(),
        other => other.to_string(),
    }
}

/// String conversion over a float value: integral floats keep their `.0`.
fn py_number_str(value: &Json) -> String {
    match value {
        Json::Number(number) => match number.as_f64() {
            Some(f) if f.is_finite() && f == f.trunc() => format!("{f}.0"),
            Some(f) => format!("{f}"),
            None => number.to_string(),
        },
        other => py_str_of(other),
    }
}

fn chars_take(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

/// The `_run` orchestrator boundary: a tool's exception becomes result data and
/// must not abort the remaining site checks; a non-dict result is wrapped.
fn run_tool(tools: &dyn SiteTools, tool: &str, call: &ToolCall) -> Json {
    match tools.call(tool, call) {
        Ok(result) if result.is_object() => result,
        Ok(result) => json!({"ok": true, "result": result}),
        Err(error) => json!({"ok": false, "error": error}),
    }
}

/// The keyword arguments each site tool is invoked with.
fn site_kwargs<'a>(
    tool: &str,
    url: &'a str,
    domain: &'a str,
    render: bool,
    sitemap_url: &'a str,
) -> ToolCall<'a> {
    ToolCall {
        url: if tool == "sitemap_crawl" && !sitemap_url.is_empty() {
            sitemap_url
        } else {
            url
        },
        domain,
        render,
        sitemap_url,
    }
}

/// Extract page URLs from a sitemap result regardless of its nesting depth.
pub fn urls_from_sitemap(sitemap: &Json, limit: i64) -> Vec<String> {
    let cap = limit * 4;
    let mut found: Vec<String> = Vec::new();
    // Fast path: the standard sitemap-crawl response stores records under `urls`.
    if let Some(entries) = sitemap
        .get("urls")
        .filter(|value| truthy(value))
        .and_then(Json::as_array)
    {
        for entry in entries {
            if let Some(text) = entry.as_str() {
                if text.starts_with("http") {
                    found.push(text.to_string());
                }
            } else if let Some(record) = entry.as_object() {
                if let Some(loc) = record.get("loc").and_then(Json::as_str) {
                    found.push(loc.to_string());
                }
            }
            if found.len() as i64 >= cap {
                break;
            }
        }
    }
    let empty = Json::Object(Map::new());
    let mut stack: Vec<Json> = if found.is_empty() {
        vec![sitemap.clone()]
    } else {
        Vec::new()
    };
    while !stack.is_empty() && (found.len() as i64) < cap {
        let node = stack.pop().unwrap_or_else(|| empty.clone());
        if let Some(map) = node.as_object() {
            for key in ["loc", "url"] {
                if let Some(Json::String(value)) = map.get(key) {
                    if value.starts_with("http") {
                        found.push(value.clone());
                    }
                }
            }
            for value in map.values() {
                if value.is_object() || value.is_array() {
                    stack.push(value.clone());
                }
            }
        } else if let Some(items) = node.as_array() {
            for item in items {
                stack.push(item.clone());
            }
        }
    }
    // Sitemap order is meaningful--important pages usually appear first--so preserve it.
    let mut seen = HashSet::new();
    found
        .into_iter()
        .filter(|url| seen.insert(url.clone()))
        .take(limit.max(0) as usize)
        .collect()
}

/// Read H1 from any heading shape returned by `parse`.
fn first_h1(facts: &Json) -> String {
    if let Some(direct) = facts.get("h1").and_then(Json::as_str) {
        if !direct.trim().is_empty() {
            return direct.to_string();
        }
    }
    if let Some(headings) = facts.get("headings") {
        if let Some(map) = headings.as_object() {
            let none = Json::Array(Vec::new());
            let chosen = map
                .get("h1")
                .filter(|value| truthy(value))
                .or_else(|| map.get("H1").filter(|value| truthy(value)))
                .unwrap_or(&none);
            if let Some(items) = chosen.as_array() {
                if let Some(first) = items.first() {
                    return py_str_of(first);
                }
            }
            if let Some(text) = chosen.as_str() {
                return text.to_string();
            }
        } else if let Some(items) = headings.as_array() {
            for item in items {
                if let Some(record) = item.as_object() {
                    let level = record
                        .get("level")
                        .map(py_str_of)
                        .unwrap_or_default()
                        .to_lowercase();
                    if level == "1" || level == "h1" {
                        return record.get("text").map(py_str_of).unwrap_or_default();
                    }
                }
            }
        }
    }
    String::new()
}

/// Whether every item in `parse`'s batch lacked a response.
///
/// `parse` reports a completed 4xx/5xx response with page fields and a false
/// `ok` value; that is still measured evidence. A transport failure is instead
/// the distinct failure shape, which has `error` and no response fields.
fn parse_results_unavailable(result: &Json) -> bool {
    let rows = result.get("results").and_then(Json::as_array);
    match rows {
        Some(rows) if !rows.is_empty() => rows.iter().all(|row| {
            row.is_object()
                && row.get("ok") == Some(&Json::Bool(false))
                && row.get("error").is_some()
                && row.get("status_code").is_none()
        }),
        _ => false,
    }
}

/// Recognize a failed page tool without recasting measured parse responses.
fn page_tool_failed(tool: &str, result: &Json) -> bool {
    result.get("ok") == Some(&Json::Bool(false))
        || (tool == "parse" && parse_results_unavailable(result))
}

/// Say why a completed response is still not evidence about the URL.
///
/// A status >= 400 means the body is the server's error page, and a
/// `blocked_by_waf` marker means it is the edge's challenge page: either way
/// every finding extracted from that body would describe the block, not the
/// requested URL. `None` for a page that was measured.
fn page_not_measured_reason(facts: &Json) -> Option<String> {
    if let Some(blocked) = facts.get("blocked_by_waf") {
        if truthy(blocked) {
            return Some(format!("blocked_by_waf ({})", py_str_of(blocked)));
        }
    }
    if let Some(status) = facts.get("status_code") {
        if !status.is_boolean() {
            if let Some(value) = status.as_i64() {
                if value >= 400 {
                    return Some(format!("HTTP {value}"));
                }
            }
        }
    }
    None
}

fn object_or<'a>(value: Option<&'a Json>, empty: &'a Json) -> &'a Json {
    match value {
        Some(found) if truthy(found) && found.is_object() => found,
        _ => empty,
    }
}

/// Build one normalized page row for the Excel and Word reports.
fn page_row(url: &str, results: &[(String, Json)]) -> Json {
    let empty = Json::Object(Map::new());
    let empty_list: Vec<Json> = Vec::new();
    let lookup = |name: &str| {
        results
            .iter()
            .find(|(tool, _)| tool == name)
            .map(|(_, value)| value)
    };
    // `parse` returns a batch envelope: {"count": N, "results": [...]}. Reading
    // fields directly from the envelope produces empty report columns; this
    // exact mistake surfaced during the first production run.
    let parsed = object_or(lookup("parse"), &empty);
    let rows = parsed
        .get("results")
        .filter(|value| truthy(value))
        .and_then(Json::as_array)
        .unwrap_or(&empty_list);
    let facts = rows.first().unwrap_or(parsed);
    let schema = object_or(lookup("schema_check"), &empty);
    let schema_types: Vec<String> = match schema.get("types") {
        Some(Json::String(text)) if truthy(&Json::String(text.clone())) => {
            text.chars().map(String::from).collect()
        }
        Some(Json::Array(items)) if !items.is_empty() => items.iter().map(py_str_of).collect(),
        _ => {
            let mut names: Vec<String> = schema
                .get("entities")
                .filter(|value| truthy(value))
                .and_then(Json::as_array)
                .map(|entities| {
                    entities
                        .iter()
                        .filter_map(Json::as_object)
                        .flat_map(|entity| {
                            entity
                                .get("types")
                                .filter(|value| truthy(value))
                                .and_then(Json::as_array)
                                .unwrap_or(&empty_list)
                                .iter()
                                .filter_map(Json::as_str)
                                .map(str::to_string)
                                .collect::<Vec<_>>()
                        })
                        .collect()
                })
                .unwrap_or_default();
            names.sort();
            names.dedup();
            names
        }
    };
    let social = object_or(lookup("social_meta_check"), &empty);
    let mut issues: Vec<String> = Vec::new();
    for (tool, data) in results {
        let Some(map) = data.as_object() else {
            continue;
        };
        if map.get("ok") == Some(&Json::Bool(false))
            && map.get("error").map(truthy).unwrap_or(false)
        {
            let error = map.get("error").map(py_str_of).unwrap_or_default();
            issues.push(format!("{tool}: {error}"));
        } else if tool == "parse" && parse_results_unavailable(data) {
            let rows = data
                .get("results")
                .and_then(Json::as_array)
                .unwrap_or(&empty_list);
            let error = rows
                .first()
                .and_then(|row| row.get("error"))
                .filter(|value| truthy(value))
                .map(py_str_of)
                .unwrap_or_else(|| "no response received".to_string());
            issues.push(format!("{tool}: {error}"));
        }
        if let Some(findings) = data
            .get("findings")
            .filter(|value| truthy(value))
            .and_then(Json::as_array)
        {
            for finding in findings.iter().take(5) {
                issues.push(py_str_of(finding));
            }
        }
    }
    let title = facts.get("title").and_then(Json::as_str).unwrap_or("");
    let description = facts
        .get("meta_description")
        .and_then(Json::as_str)
        .filter(|text| !text.is_empty())
        .or_else(|| {
            facts
                .get("description")
                .and_then(Json::as_str)
                .filter(|text| !text.is_empty())
        })
        .unwrap_or("");
    let words = facts
        .get("word_count")
        .filter(|value| truthy(value))
        .or_else(|| facts.get("words").filter(|value| truthy(value)))
        .cloned()
        .unwrap_or(json!(0));
    let mut row = Map::new();
    row.insert("url".into(), json!(url));
    row.insert(
        "status".into(),
        facts
            .get("status_code")
            .filter(|value| truthy(value))
            .or_else(|| facts.get("status").filter(|value| truthy(value)))
            .cloned()
            .unwrap_or(Json::Null),
    );
    row.insert("title".into(), json!(chars_take(title, 300)));
    row.insert("title_length".into(), json!(title.chars().count()));
    row.insert(
        "description_length".into(),
        json!(description.chars().count()),
    );
    row.insert("h1".into(), json!(chars_take(&first_h1(facts), 300)));
    row.insert(
        "canonical".into(),
        json!(facts.get("canonical").and_then(Json::as_str).unwrap_or("")),
    );
    row.insert("words".into(), words);
    row.insert(
        "schema_types".into(),
        json!(schema_types
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ")),
    );
    row.insert(
        "schema_errors".into(),
        json!(schema
            .get("errors")
            .filter(|value| truthy(value))
            .and_then(Json::as_array)
            .map(Vec::len)
            .unwrap_or(0)),
    );
    row.insert(
        "social_missing".into(),
        json!(social
            .get("missing")
            .filter(|value| truthy(value))
            .and_then(Json::as_array)
            .map(Vec::len)
            .unwrap_or(0)),
    );
    row.insert("issues".into(), json!(issues));
    // An error or block body is not the requested page: the row stays (with its
    // status) so the audit states what was requested and what came back, but
    // nothing extracted from that body may count as a finding.
    if let Some(reason) = page_not_measured_reason(facts) {
        row.insert("measured".into(), json!(false));
        row.insert("not_measured_reason".into(), json!(reason));
        row.insert("issues".into(), json!(Vec::<String>::new()));
    } else {
        row.insert("measured".into(), json!(true));
    }
    Json::Object(row)
}

/// The audit origin `scheme://authority` of a normalized URL.
fn origin_of(url: &str) -> String {
    let parts = urlsplit(url).unwrap_or_default();
    format!("{}://{}", parts.scheme, parts.netloc)
}

/// The lower-cased host of a URL, without userinfo, port or IPv6 brackets.
fn hostname_of(url: &str) -> String {
    let netloc = urlsplit(url).map(|parts| parts.netloc).unwrap_or_default();
    let host = match netloc.rfind('@') {
        Some(index) => &netloc[index + 1..],
        None => netloc.as_str(),
    };
    let host = if host.starts_with('[') {
        match host.find(']') {
            Some(end) => &host[1..end],
            None => host,
        }
    } else {
        match host.rfind(':') {
            Some(index) => &host[..index],
            None => host,
        }
    };
    host.trim().to_lowercase()
}

fn severity_rank(severity: &str) -> i32 {
    match severity {
        "critical" => 0,
        "warning" => 1,
        "notice" => 2,
        _ => 3,
    }
}

/// Caller options of `audit_site`: the scalar knobs the handler passes
/// straight through from tool arguments.
pub struct AuditOptions<'a> {
    /// Explicit page list; when empty, pages are read from the sitemap.
    pub urls: &'a [String],
    /// Cap on the number of selected pages.
    pub limit: i64,
    /// Parallel page fetches; kept conservative so an audit is not a load test.
    pub concurrency: i64,
    /// Request a rendered DOM pass where a tool supports one.
    pub render: bool,
    /// Registry tool names that must not run.
    pub skip: &'a [String],
    /// Caller-supplied CrUX current field data, bypassing the provider layer.
    pub crux_evidence: Option<&'a Json>,
}

/// Assemble a complete site audit into one contract-stable document.
///
/// `options.urls` is an explicit page list; when empty, pages are read from
/// the sitemap. `options.limit` caps the selected pages, while
/// `options.skip` names checks that should not run. The audit makes many
/// network requests, so `options.concurrency` remains deliberately
/// conservative: an SEO audit must not resemble a load test to the target
/// site. `Err` names a registry tool the audit must have (the orchestrator's
/// own `KeyError`, which no boundary may swallow).
#[allow(clippy::too_many_lines)]
pub fn audit_site(
    url: &str,
    options: &AuditOptions<'_>,
    tools: &dyn SiteTools,
) -> Result<Json, String> {
    let AuditOptions {
        urls,
        limit,
        concurrency,
        render,
        skip,
        crux_evidence,
    } = *options;
    if url.trim().is_empty() {
        return Ok(json!({"ok": false, "error": "URL is required"}));
    }
    let start = normalize_url(url.trim());
    let domain = hostname_of(&start);
    // `normalize_url` is intentionally permissive and can turn arbitrary text
    // into a host-like value. Because an audit emits dozens of requests, reject
    // malformed input before any network work begins.
    if domain.is_empty() || domain.contains(' ') || !domain.trim_matches('.').contains('.') {
        return Ok(json!({
            "ok": false,
            "error": format!("Value does not look like a domain: {}", py_repr(url))
        }));
    }

    let mut cwv_assessments: Vec<Json> = Vec::new();
    let mut cwv_sampling: Option<Map<String, Json>> = None;
    if let Some(crux_evidence) = crux_evidence {
        let Some(crux) = crux_evidence.as_object() else {
            return Ok(json!({"ok": false, "error": "CrUX evidence must be an object"}));
        };
        let supplied_value = crux
            .get("result")
            .cloned()
            .unwrap_or_else(|| crux_evidence.clone());
        let Some(supplied) = supplied_value.as_object() else {
            return Ok(json!({"ok": false, "error": "CrUX evidence result must be an object"}));
        };
        if let Some(envelope) = crux.get("evidence") {
            if envelope.is_object()
                && envelope.get("provider").and_then(Json::as_str) != Some("crux")
            {
                return Ok(json!({"ok": false, "error": "provider artifact is not CrUX evidence"}));
            }
        }
        let records: Vec<Json> = match supplied.get("records") {
            Some(Json::Array(items)) => items.clone(),
            Some(_) => {
                return Ok(json!({
                    "ok": false,
                    "error": "CrUX evidence must contain at most 25 records"
                }))
            }
            None => vec![Json::Object(supplied.clone())],
        };
        if records.len() > 25 || records.iter().any(|record| !record.is_object()) {
            return Ok(json!({
                "ok": false,
                "error": "CrUX evidence must contain at most 25 records"
            }));
        }
        if supplied.contains_key("records") {
            if supplied.get("provider").and_then(Json::as_str) != Some("crux")
                || supplied.get("metric_source").and_then(Json::as_str) != Some("field")
            {
                return Ok(json!({
                    "ok": false,
                    "error": "CrUX sample has invalid provider provenance"
                }));
            }
            let mut counts = Map::new();
            for key in ["requested", "sampled", "omitted", "requests", "cache_hits"] {
                let value = supplied.get(key);
                let number = match value {
                    Some(found) if !found.is_boolean() => found.as_i64(),
                    _ => None,
                };
                match number {
                    Some(count) if count >= 0 => {
                        counts.insert(key.to_string(), json!(count));
                    }
                    _ => {
                        return Ok(json!({
                            "ok": false,
                            "error": "CrUX sample has invalid count metadata"
                        }))
                    }
                }
            }
            let get = |key: &str| counts.get(key).and_then(Json::as_i64).unwrap_or(0);
            let (requested, sampled, omitted, requests, cache_hits) = (
                get("requested"),
                get("sampled"),
                get("omitted"),
                get("requests"),
                get("cache_hits"),
            );
            if sampled != records.len() as i64
                || requested != sampled + omitted
                || sampled > 25
                || requests + cache_hits > sampled
            {
                return Ok(json!({
                    "ok": false,
                    "error": "CrUX sample counts do not match retained records"
                }));
            }
            let targets: Vec<String> = records
                .iter()
                .filter_map(|record| {
                    record
                        .get("target")
                        .and_then(Json::as_str)
                        .map(str::to_string)
                })
                .collect();
            if targets.len() != records.len() {
                return Ok(json!({
                    "ok": false,
                    "error": "CrUX sample has invalid target records"
                }));
            }
            if targets.iter().collect::<HashSet<_>>().len() != targets.len() {
                return Ok(json!({
                    "ok": false,
                    "error": "CrUX sample repeats a requested URL"
                }));
            }
            let mut observed_targets: Vec<String> = Vec::new();
            for record in &records {
                let chosen = record
                    .get("record_target")
                    .filter(|value| truthy(value))
                    .unwrap_or_else(|| record.get("target").unwrap_or(&Json::Null));
                let Some(text) = chosen.as_str() else {
                    return Ok(json!({
                        "ok": false,
                        "error": "CrUX sample has invalid measured targets"
                    }));
                };
                observed_targets.push(text.to_string());
            }
            let duplicate_record_targets =
                observed_targets.len() - observed_targets.iter().collect::<HashSet<_>>().len();
            let mut sampling = Map::new();
            for key in [
                "requested",
                "sampled",
                "omitted",
                "requests",
                "cache_hits",
                "duplicate_record_targets",
                "cache_max_age_hours",
            ] {
                sampling.insert(
                    key.to_string(),
                    supplied.get(key).cloned().unwrap_or(Json::Null),
                );
            }
            sampling.insert(
                "duplicate_record_targets".into(),
                json!(duplicate_record_targets),
            );
            sampling.insert(
                "source_state".into(),
                if duplicate_record_targets > 0 {
                    json!("partial")
                } else {
                    supplied.get("state").cloned().unwrap_or(Json::Null)
                },
            );
            cwv_sampling = Some(sampling);
        }
        let site_origin = origin_of(&start);
        for record in &records {
            let target = record.get("target").and_then(Json::as_str);
            let kind = record.get("target_kind").and_then(Json::as_str);
            if !matches!(kind, Some("url") | Some("origin")) || target.is_none() {
                return Ok(json!({
                    "ok": false,
                    "error": "CrUX evidence needs target and target_kind"
                }));
            }
            let target = target.unwrap_or_default();
            if origin_of(target) != site_origin
                || (kind == Some("origin") && target.trim_end_matches('/') != site_origin)
            {
                return Ok(json!({
                    "ok": false,
                    "error": "CrUX evidence target is outside audit origin"
                }));
            }
            let assessment = match assess(record) {
                Ok(value) => value,
                Err(error) => return Ok(json!({"ok": false, "error": error})),
            };
            let observed_target = assessment.get("target").map(py_str_of).unwrap_or_default();
            if origin_of(&observed_target) != site_origin {
                return Ok(json!({
                    "ok": false,
                    "error": "CrUX record target is outside audit origin"
                }));
            }
            cwv_assessments.push(assessment);
        }
    }

    let limit = limit.max(1);
    // Site tools could run on a bounded thread pool sized by this knob;
    // execution here is sequential, which preserves the document
    // insertion order exactly. The value stays accepted for signature parity.
    let _ = concurrency.clamp(1, 10);

    let skipped: Vec<&str> = skip.iter().map(String::as_str).collect();
    let mut site_tools: Vec<&str> = SITE_TOOLS
        .into_iter()
        .filter(|tool| !skipped.contains(tool))
        .collect();
    let page_tools: Vec<&str> = PAGE_TOOLS
        .into_iter()
        .filter(|tool| !skipped.contains(tool))
        .collect();

    let mut site = Map::new();

    // Run robots first and separately because it declares the sitemap URL(s).
    // Without this step, sitemap-crawl receives the homepage, returns "Unknown
    // sitemap format", and the audit silently falls back to a single page.
    let mut sitemap_url = String::new();
    let mut sitemap_urls: Vec<String> = Vec::new();
    if site_tools.contains(&"robots_check") {
        if !tools.has("robots_check") {
            return Err("KeyError: 'robots_check'".to_string());
        }
        let result = run_tool(
            tools,
            "robots_check",
            &site_kwargs("robots_check", &start, &domain, render, ""),
        );
        sitemap_urls = result
            .get("sitemaps")
            .filter(|value| truthy(value))
            .and_then(Json::as_array)
            .map(|items| items.iter().map(py_str_of).collect())
            .unwrap_or_default();
        sitemap_url = sitemap_urls.first().cloned().unwrap_or_default();
        site.insert("robots_check".into(), result);
        site_tools.retain(|tool| *tool != "robots_check");
    }
    if sitemap_url.is_empty() {
        sitemap_url = format!("{}/sitemap.xml", start.trim_end_matches('/'));
        sitemap_urls = vec![sitemap_url.clone()];
    }

    if site_tools.contains(&"sitemap_crawl") && sitemap_urls.len() > 1 {
        if !tools.has("sitemap_crawl") {
            return Err("KeyError: 'sitemap_crawl'".to_string());
        }
        // robots.txt can declare several independent Sitemap: directives (e.g.
        // one per content type); each is its own document, so each needs its own
        // fetch. Page selection below then samples from their combined url set
        // instead of only ever seeing whichever source happened to be declared
        // first.
        let empty_list: Vec<Json> = Vec::new();
        let mut seen: Vec<Json> = Vec::new();
        let mut merged_urls: Vec<Json> = Vec::new();
        let mut errors: Vec<Json> = Vec::new();
        let mut truncated = false;
        for one in &sitemap_urls {
            let fetched = run_tool(
                tools,
                "sitemap_crawl",
                &site_kwargs("sitemap_crawl", one, &domain, render, ""),
            );
            for entry in fetched
                .get("urls")
                .filter(|value| truthy(value))
                .and_then(Json::as_array)
                .unwrap_or(&empty_list)
            {
                let loc = match entry {
                    Json::Object(map) => map.get("loc").cloned().unwrap_or(Json::Null),
                    other => other.clone(),
                };
                if truthy(&loc) && !seen.contains(&loc) {
                    seen.push(loc);
                    merged_urls.push(entry.clone());
                }
            }
            for error in fetched
                .get("errors")
                .filter(|value| truthy(value))
                .and_then(Json::as_array)
                .unwrap_or(&empty_list)
            {
                if error.is_object() {
                    errors.push(error.clone());
                } else {
                    errors.push(json!({"url": one, "error": py_str_of(error)}));
                }
            }
            if fetched.get("truncated").map(truthy).unwrap_or(false) {
                truncated = true;
            }
            if fetched.get("ok") == Some(&Json::Bool(false)) {
                let error = fetched
                    .get("error")
                    .map(py_str_of)
                    .unwrap_or_else(|| "fetch failed".to_string());
                errors.push(json!({"url": one, "error": error}));
            }
        }
        // A root with no URLs and no errors is a legitimately empty sitemap; a
        // root with errors and nothing collected is a real failure and must not
        // read as clean evidence.
        let mut merged = Map::new();
        let ok = !merged_urls.is_empty() || errors.is_empty();
        merged.insert("ok".into(), json!(ok));
        merged.insert("urls".into(), Json::Array(merged_urls));
        merged.insert("sources".into(), json!(sitemap_urls));
        if !errors.is_empty() {
            // Some roots still produced URLs, so this is not a hard failure —
            // but the unavailable roots must still surface to report writers
            // instead of disappearing behind the successful ones. The wording is
            // not free: classify() matches SEVERITY_RULES by plain substring,
            // and its marker is "unavailable or return errors". Writing
            // "returned" here instead left this finding classified as a notice
            // -- sorted to the bottom of every report.
            let findings: Vec<String> = errors
                .iter()
                .map(|error| {
                    let root = error
                        .get("url")
                        .filter(|value| truthy(value))
                        .map(py_str_of)
                        .unwrap_or_else(|| "(unknown root)".to_string());
                    let text = error
                        .get("error")
                        .map(py_str_of)
                        .unwrap_or_else(|| "None".to_string());
                    format!("Sitemap source unavailable or return errors: {root} — {text}")
                })
                .collect();
            merged.insert("errors".into(), Json::Array(errors));
            if ok {
                merged.insert("findings".into(), json!(findings));
            }
        }
        if truncated {
            merged.insert("truncated".into(), json!(true));
        }
        site.insert("sitemap_crawl".into(), Json::Object(merged));
        site_tools.retain(|tool| *tool != "sitemap_crawl");
    }

    for tool in &site_tools {
        if !tools.has(tool) {
            continue;
        }
        let call = site_kwargs(tool, &start, &domain, render, &sitemap_url);
        site.insert((*tool).to_string(), run_tool(tools, tool, &call));
    }

    let empty = Json::Object(Map::new());
    let mut page_urls: Vec<String> = urls
        .iter()
        .filter(|candidate| !candidate.is_empty() && !candidate.trim().is_empty())
        .map(|candidate| normalize_url(candidate))
        .collect();
    if page_urls.is_empty() {
        let sitemap = site.get("sitemap_crawl").unwrap_or(&empty);
        page_urls = urls_from_sitemap(sitemap, limit);
    }
    if page_urls.is_empty() {
        page_urls.push(start.clone()); // An empty sitemap still warrants a homepage analysis.
    }
    page_urls.truncate(limit.max(0) as usize);

    let mut pages: Vec<Json> = Vec::new();
    let mut failure_counts: BTreeMap<String, i64> = BTreeMap::new();
    for page_url in &page_urls {
        let mut results: Vec<(String, Json)> = Vec::new();
        for tool in &page_tools {
            if !tools.has(tool) {
                continue;
            }
            let call = ToolCall {
                url: page_url,
                domain: &domain,
                render,
                sitemap_url: "",
            };
            results.push(((*tool).to_string(), run_tool(tools, tool, &call)));
        }
        for (tool, data) in &results {
            if page_tool_failed(tool, data) {
                *failure_counts.entry(tool.clone()).or_insert(0) += 1;
            }
        }
        pages.push(page_row(page_url, &results));
    }

    // A page tool that raises for every sampled page hides just as much
    // evidence as a site-level tool failure and must be counted the same way,
    // not left to whatever severity classify() happens to assign a raw
    // exception message.
    let page_tools_failed: Vec<Json> = failure_counts
        .iter()
        .filter(|(_, count)| !pages.is_empty() && **count == pages.len() as i64)
        .map(|(tool, count)| {
            json!({"tool": tool, "failed_pages": count, "pages_checked": pages.len()})
        })
        .collect();
    let fully_failed: Vec<&str> = page_tools_failed
        .iter()
        .filter_map(|row| row.get("tool").and_then(Json::as_str))
        .collect();

    let empty_list: Vec<Json> = Vec::new();
    let mut findings: Vec<Json> = Vec::new();
    let mut failed_tools: Vec<Json> = Vec::new();
    for (tool, data) in &site {
        let Some(map) = data.as_object() else {
            continue;
        };
        if map.get("ok") == Some(&Json::Bool(false)) {
            let error = map
                .get("error")
                .map(py_str_of)
                .unwrap_or_else(|| "None".to_string());
            failed_tools.push(json!({"tool": tool, "error": chars_take(&error, 300)}));
            continue;
        }
        for text in map
            .get("findings")
            .filter(|value| truthy(value))
            .and_then(Json::as_array)
            .unwrap_or(&empty_list)
        {
            let text = py_str_of(text);
            findings.push(json!({
                "source": tool,
                "severity": classify(&text),
                "text": text,
            }));
        }
    }
    for page in &pages {
        if page.get("measured") == Some(&Json::Bool(false)) {
            // Nothing an error or block body produced may reach the totals; the
            // row itself is accounted for in pages_not_measured.
            continue;
        }
        for text in page
            .get("issues")
            .and_then(Json::as_array)
            .unwrap_or(&empty_list)
        {
            let text = py_str_of(text);
            let mut severity = classify(&text).to_string();
            // This tool produced zero usable evidence for the whole crawl -- a
            // raw exception message must not be allowed to fall through to
            // "notice" purely because its text doesn't match a rule marker.
            if fully_failed
                .iter()
                .any(|tool| text.starts_with(&format!("{tool}: ")))
            {
                severity = "critical".to_string();
            }
            findings.push(json!({
                "source": "page",
                "url": page.get("url").cloned().unwrap_or(Json::Null),
                "severity": severity,
                "text": text,
            }));
        }
    }

    // Duplicate titles become visible only after pages are assembled; no
    // isolated page tool can detect them. Unmeasured pages stay out: their
    // titles belong to error or challenge bodies, so "the same title" would be
    // a false warning about pages that were never seen.
    let mut titles: Vec<(String, Vec<String>)> = Vec::new();
    for page in &pages {
        if page.get("measured") == Some(&Json::Bool(false)) {
            continue;
        }
        let title = page
            .get("title")
            .and_then(Json::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if title.is_empty() {
            continue;
        }
        let url = page
            .get("url")
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_string();
        match titles.iter_mut().find(|(known, _)| *known == title) {
            Some((_, owners)) => owners.push(url),
            None => titles.push((title, vec![url])),
        }
    }
    for (title, owners) in &titles {
        if owners.len() > 1 {
            findings.push(json!({
                "source": "pages",
                "severity": "warning",
                "text": format!(
                    "The same title is used by {} pages ({}…): {}",
                    owners.len(),
                    chars_take(title, 60),
                    owners.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
                ),
            }));
        }
    }

    for assessment in &cwv_assessments {
        let mut entries: Vec<(&String, &Json)> = assessment
            .get("metrics")
            .and_then(Json::as_object)
            .map(|metrics| metrics.iter().collect())
            .unwrap_or_default();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        for (_, metric) in &entries {
            let state = metric.get("state").and_then(Json::as_str).unwrap_or("");
            if !matches!(state, "poor" | "needs_improvement") {
                continue;
            }
            let period = assessment
                .get("collection_period")
                .cloned()
                .unwrap_or(Json::Null);
            let first_date = period
                .get("first_date")
                .map(py_str_of)
                .unwrap_or_else(|| "None".to_string());
            let last_date = period
                .get("last_date")
                .map(py_str_of)
                .unwrap_or_else(|| "None".to_string());
            let kind = assessment
                .get("target_kind")
                .map(py_str_of)
                .unwrap_or_else(|| "None".to_string());
            let target = assessment
                .get("target")
                .map(py_str_of)
                .unwrap_or_else(|| "None".to_string());
            let form_factor = assessment
                .get("form_factor")
                .map(py_str_of)
                .unwrap_or_else(|| "None".to_string());
            let policy = assessment
                .get("policy")
                .map(py_str_of)
                .unwrap_or_else(|| "None".to_string());
            let label = metric
                .get("label")
                .map(py_str_of)
                .unwrap_or_else(|| "None".to_string());
            let p75 = metric
                .get("p75")
                .map(py_number_str)
                .unwrap_or_else(|| "None".to_string());
            let unit = metric
                .get("unit")
                .map(py_str_of)
                .unwrap_or_else(|| "None".to_string());
            findings.push(json!({
                "source": "crux_field",
                "severity": if state == "poor" { "warning" } else { "notice" },
                "url": if assessment.get("target_kind").and_then(Json::as_str) == Some("url") {
                    assessment.get("target").cloned().unwrap_or(Json::Null)
                } else {
                    Json::Null
                },
                "text": format!(
                    "CrUX field {label} p75 {p75} {unit} is {state} for {kind} {target} \
                     ({form_factor}, {first_date}..{last_date}; policy {policy})."
                ),
                "evidence": assessment.clone(),
            }));
        }
    }

    findings.sort_by_key(|finding| {
        severity_rank(finding.get("severity").and_then(Json::as_str).unwrap_or(""))
    });
    let count_of = |level: &str| {
        findings
            .iter()
            .filter(|finding| finding.get("severity").and_then(Json::as_str) == Some(level))
            .count()
    };
    let mut by_severity = Map::new();
    by_severity.insert("critical".into(), json!(count_of("critical")));
    by_severity.insert("warning".into(), json!(count_of("warning")));
    by_severity.insert("notice".into(), json!(count_of("notice")));
    // Requested pages the audit could not measure -- error responses and
    // bot-protection blocks. Every page the audit selected stays in `pages` and
    // in `pages_checked`; this list is the subset whose body was not evidence
    // about the URL, so reports can say so up front.
    let pages_not_measured: Vec<Json> = pages
        .iter()
        .filter(|page| page.get("measured") == Some(&Json::Bool(false)))
        .map(|page| {
            json!({
                "url": page.get("url").cloned().unwrap_or(Json::Null),
                "status": page.get("status").cloned().unwrap_or(Json::Null),
                "reason": page.get("not_measured_reason").cloned().unwrap_or(Json::Null),
            })
        })
        .collect();
    let cwv_state: &str = if crux_evidence.is_none() {
        "not_requested"
    } else if cwv_assessments.is_empty()
        || cwv_assessments.iter().all(|assessment| {
            assessment.get("overall").and_then(Json::as_str) == Some("unavailable")
        })
    {
        "unavailable"
    } else {
        let sampling_partial = cwv_sampling.as_ref().is_some_and(|sampling| {
            truthy(sampling.get("omitted").unwrap_or(&Json::Null))
                || sampling.get("source_state").and_then(Json::as_str) != Some("complete")
        });
        if sampling_partial
            || cwv_assessments.iter().any(|assessment| {
                matches!(
                    assessment.get("overall").and_then(Json::as_str),
                    Some("partial") | Some("unavailable")
                )
            })
        {
            "partial"
        } else {
            "complete"
        }
    };

    let mut tools_run: Vec<String> = site
        .iter()
        .filter(|(_, data)| !(data.is_object() && data.get("ok") == Some(&Json::Bool(false))))
        .map(|(tool, _)| tool.clone())
        .collect();
    tools_run.sort();

    let mut field_cwv = Map::new();
    field_cwv.insert("state".into(), json!(cwv_state));
    field_cwv.insert("assessments".into(), Json::Array(cwv_assessments.clone()));
    field_cwv.insert(
        "sampling".into(),
        cwv_sampling.map(Json::Object).unwrap_or(Json::Null),
    );

    let mut summary = Map::new();
    summary.insert("pages_checked".into(), json!(pages.len()));
    summary.insert("pages_not_measured".into(), Json::Array(pages_not_measured));
    summary.insert("findings_total".into(), json!(findings.len()));
    summary.insert("findings_by_severity".into(), Json::Object(by_severity));
    summary.insert("tools_run".into(), json!(tools_run));
    summary.insert("tools_failed".into(), Json::Array(failed_tools));
    summary.insert("page_tools_failed".into(), Json::Array(page_tools_failed));
    summary.insert("field_cwv".into(), Json::Object(field_cwv));
    summary.insert(
        "severity_note".into(),
        json!("Severity is assigned by the aggregation policy (SEVERITY_RULES); it is not measured by the source tool."),
    );

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("schema".into(), json!(SCHEMA));
    out.insert("url".into(), json!(start));
    out.insert("domain".into(), json!(domain));
    // Whole seconds, no microseconds.
    out.insert("generated_at".into(), json!(stamp(Format::IsoSeconds)));
    out.insert("site".into(), Json::Object(site));
    out.insert("pages".into(), Json::Array(pages));
    out.insert("findings".into(), Json::Array(findings));
    out.insert("field_cwv".into(), Json::Array(cwv_assessments));
    out.insert("summary".into(), Json::Object(summary));
    Ok(Json::Object(out))
}

#[cfg(test)]
#[path = "site_audit_tests.rs"]
mod tests;

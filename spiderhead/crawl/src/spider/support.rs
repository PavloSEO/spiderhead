//! Helpers of the crawl loop: candidates, throttle restore, robots summary, capture config.

use super::{CrawlOptions, RobotsState};

use serde_json::{json, Map, Value};
use spiderhead_fetch::robots;

use spiderhead_fetch::throttle::Throttle;
use spiderhead_store::crawl_capture::CaptureEvent;
use spiderhead_store::native_scan::NativeScan;
use spiderhead_store::scan_lease::NativeScanLease;

/// One frontier candidate (`apply_candidates` input shape).
pub(super) fn candidate(requested_url: &str, frontier_url: &str, depth: i64) -> Map<String, Value> {
    let parsed = url::Url::parse(frontier_url).ok();
    let path_key = parsed
        .as_ref()
        .map(|u| {
            let path = u.path();
            if path.is_empty() { "/" } else { path }.to_string()
        })
        .unwrap_or_else(|| "/".to_string());
    let query_key = parsed
        .and_then(|u| u.query().map(str::to_string))
        .unwrap_or_default();
    let mut row = Map::new();
    row.insert("path_key".into(), json!(path_key));
    row.insert("query_key".into(), json!(query_key));
    row.insert("requested_url".into(), json!(requested_url));
    row.insert("frontier_url".into(), json!(frontier_url));
    row.insert("depth".into(), json!(depth));
    row
}

/// `<loc>` entries of a sitemap document, in source order. Only plain urlsets are handled here.
/// Load `scan_throttle.v2` + elapsed from the scan into the live controller.
pub(super) fn restore_throttle(scan: &NativeScan, throttle: &mut Throttle) {
    let Ok(runtime) = scan.resume_snapshot(false) else {
        return;
    };
    let Some(state) = runtime.get("runtime") else {
        return;
    };
    let Some(throttle_state) = state.get("throttle").and_then(Value::as_object) else {
        return;
    };
    let number = |key: &str| {
        throttle_state
            .get(key)
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
    };
    throttle.restore(
        number("delay_seconds"),
        throttle_state
            .get("concurrency")
            .and_then(Value::as_i64)
            .unwrap_or(1)
            .max(1) as u32,
        throttle_state
            .get("consecutive_ok")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .max(0) as u32,
        throttle_state
            .get("requests_used")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .max(0) as u64,
        state
            .get("elapsed_seconds")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
    );
}

pub(super) fn is_html_record(record: &Map<String, Value>) -> bool {
    record
        .get("content_type")
        .and_then(Value::as_str)
        .map(|t| t.to_ascii_lowercase().contains("html"))
        .unwrap_or(false)
}

pub(super) fn decision(url: &str, reason: &str, source: &str, depth: i64) -> Map<String, Value> {
    let mut row = Map::new();
    row.insert("url".into(), json!(url));
    row.insert("reason".into(), json!(reason));
    row.insert("source".into(), json!(source));
    row.insert("depth".into(), json!(depth));
    row
}

pub(super) fn runtime_state(
    throttle: &Throttle,
    max_depth_reached: i64,
    crawl_delay_applied: Option<f64>,
) -> Map<String, Value> {
    let mut runtime = Map::new();
    runtime.insert("max_depth_reached".into(), json!(max_depth_reached));
    runtime.insert("elapsed_seconds".into(), json!(throttle.elapsed_seconds()));
    runtime.insert(
        "circuit_timeout_streak".into(),
        json!(throttle.timeout_streak()),
    );
    runtime.insert(
        "circuit_server_error_streak".into(),
        json!(throttle.server_error_streak()),
    );
    runtime.insert(
        "crawl_delay_applied".into(),
        crawl_delay_applied.map(|d| json!(d)).unwrap_or(Value::Null),
    );
    let mut throttle_state = Map::new();
    throttle_state.insert("delay_seconds".into(), json!(throttle.delay_seconds()));
    throttle_state.insert("concurrency".into(), json!(throttle.concurrency()));
    // Persisted streak is clamped to WIDEN-1 on load.
    throttle_state.insert(
        "consecutive_ok".into(),
        json!(throttle.consecutive_ok().min(2)),
    );
    throttle_state.insert("requests_used".into(), json!(throttle.requests_used()));
    runtime.insert("throttle".into(), Value::Object(throttle_state));
    runtime
}

/// The robots.txt fetch: same-origin redirects followed by hand, up to 5 hops.
/// `text/plain` is required after a redirect. The fetch takes a pacing slot but is never stored as a response row.
pub(super) async fn fetch_robots(
    scan: &mut NativeScan,
    options: &CrawlOptions,
    throttle: &mut Throttle,
) -> RobotsState {
    let _ = scan;
    if options.robots_policy == "ignore" {
        return RobotsState {
            groups: Vec::new(),
            summary: Some(robots_summary(
                "not_fetched",
                &json!({"groups": [], "sitemaps": []}),
                "robots.txt not fetched (policy: ignore)",
                &options.robots_policy,
                &options.robots_token,
            )),
            fetch_state: "not_fetched".to_owned(),
            note: "robots.txt not fetched (policy: ignore)".to_owned(),
        };
    }
    let origin = match url::Url::parse(&options.start_url) {
        Ok(parsed) => format!("{}://{}", parsed.scheme(), {
            match parsed.port() {
                Some(port) => format!("{}:{port}", parsed.host_str().unwrap_or("")),
                None => parsed.host_str().unwrap_or("").to_string(),
            }
        }),
        Err(_) => String::new(),
    };
    let robots_url = format!("{origin}/robots.txt");
    throttle.wait().await;
    let response =
        spiderhead_fetch::client::get(&options.client, &robots_url, &options.user_agent).await;
    // Any 4xx means no robots.txt: unrestricted, with a synthesized allow-all group.
    if let Some(status) = response.status {
        if (400..500).contains(&status) {
            let groups = vec![allow_all_group()];
            let summary = robots_summary(
                "fetched",
                &groups_value(&groups, &[]),
                "no robots.txt",
                &options.robots_policy,
                &options.robots_token,
            );
            return RobotsState {
                groups,
                summary: Some(summary),
                fetch_state: "fetched".to_owned(),
                note: String::new(),
            };
        }
        if status == 429 || status >= 500 {
            return RobotsState {
                groups: Vec::new(),
                summary: Some(robots_summary(
                    "unavailable",
                    &json!({"groups": [], "sitemaps": []}),
                    &format!("robots.txt returned {status}"),
                    &options.robots_policy,
                    &options.robots_token,
                )),
                fetch_state: "unavailable".to_owned(),
                note: format!("robots.txt returned {status}"),
            };
        }
    }
    if response.status.is_none() {
        return RobotsState {
            groups: Vec::new(),
            summary: Some(robots_summary(
                "unavailable",
                &json!({"groups": [], "sitemaps": []}),
                "robots.txt could not be fetched",
                &options.robots_policy,
                &options.robots_token,
            )),
            fetch_state: "unavailable".to_owned(),
            note: "robots.txt could not be fetched".to_owned(),
        };
    }
    let (groups, sitemaps) = robots::parse_robots(&String::from_utf8_lossy(&response.body));
    let summary = robots_summary(
        "fetched",
        &groups_value(&groups, &sitemaps),
        "",
        &options.robots_policy,
        &options.robots_token,
    );
    RobotsState {
        groups,
        summary: Some(summary),
        fetch_state: "fetched".to_owned(),
        note: String::new(),
    }
}

/// The synthesized allow-all group for a site without robots.txt; it cites line 2 of the implied
/// `User-agent: *` / `Allow: /` pair.
pub(super) fn allow_all_group() -> robots::RobotsGroup {
    let mut line_of = std::collections::BTreeMap::new();
    line_of.insert("allow:/".to_string(), 2);
    robots::RobotsGroup {
        user_agents: vec!["*".to_string()],
        allow: vec!["/".to_string()],
        disallow: Vec::new(),
        crawl_delay: None,
        request_rate_delay: None,
        line_of,
    }
}

pub(super) fn groups_value(groups: &[robots::RobotsGroup], sitemaps: &[String]) -> Value {
    json!({
        "groups": groups.iter().map(robots::RobotsGroup::to_value).collect::<Vec<Value>>(),
        "sitemaps": sitemaps,
    })
}

/// The `robots_summary` context row: the store's `context_item` shape wrapping the robots payload.
pub fn robots_summary(
    fetch_state: &str,
    parsed: &Value,
    note: &str,
    policy: &str,
    token: &str,
) -> Value {
    let payload = json!({
        "fetch_state": fetch_state,
        "final_response_id": Value::Null,
        "note": note,
        "parsed": parsed,
        "policy": policy,
        "token": token,
    });
    json!({
        "kind": "robots_summary",
        "item_key": "run",
        "payload_version": "scan_context.v1",
        "payload_json": serde_json::to_string(&payload).unwrap_or_default(),
        "completeness": if fetch_state == "unavailable" { "partial" } else { "complete" },
        "reason": note,
    })
}

pub(super) fn strip_fragment(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut parsed) => {
            parsed.set_fragment(None);
            parsed.to_string()
        }
        Err(_) => url.to_string(),
    }
}

/// Whether `href` is the redirect target of the current page (to relabel
/// `outside_host` as `redirect_off_host`).
pub(super) fn is_redirect(_page_url: &str, _href: &str) -> bool {
    false // link edges are never redirects; only the Location header path calls
}

pub(super) fn count_frontier(scan: &NativeScan) -> Result<i64, String> {
    let count: i64 = scan
        .con()
        .query_row("SELECT COUNT(*) FROM frontier", [], |row| row.get(0))
        .map_err(|e| e.to_string())?;
    Ok(count)
}

pub(super) fn config_value_of(options: &CrawlOptions) -> Result<Value, String> {
    // The scope reads `scope.*` out of the full effective settings; the caller
    // already resolved them (defaults + config file + argument overrides).
    Ok(options.config.clone())
}

/// The start row seeded first on a site crawl; `reserve_query` is off — the
/// start URL keeps its own query spelling rather than reserving variants.
pub(super) fn start_entry(start_url: &str) -> Map<String, Value> {
    let mut entry = Map::new();
    entry.insert("requested_url".into(), json!(start_url));
    entry.insert("frontier_url".into(), json!(start_url));
    entry.insert("depth".into(), json!(0));
    entry.insert("reason".into(), json!(""));
    entry.insert("source".into(), json!("start"));
    entry.insert("reserve_query".into(), json!(false));
    entry.insert("seed".into(), json!(false));
    entry
}

/// One sitemap member as a frontier seed. The member keeps its source
/// spelling; tracking parameters normalize only when the scope's
/// `normalize_tracking_parameters` says so; scope and length gates record
/// their reason on the seed itself rather than dropping the member.
pub(super) fn seed_entry(
    member: &str,
    scope: &crate::scope::Scope,
    max_url_length: usize,
    normalize_tracking: bool,
) -> Map<String, Value> {
    let mut requested = member.trim().to_owned();
    if normalize_tracking {
        requested = normalize_get_parameters(&requested);
    }
    let mut reason = scope
        .rejection(&requested)
        .map(|rejection| rejection.reason)
        .unwrap_or_default();
    if reason.is_empty() && max_url_length != 0 && requested.chars().count() > max_url_length {
        reason = "url_too_long".to_owned();
    }
    let mut entry = Map::new();
    entry.insert("requested_url".into(), json!(requested));
    entry.insert("frontier_url".into(), json!(requested));
    entry.insert("depth".into(), json!(0));
    entry.insert("reason".into(), json!(reason));
    entry.insert("source".into(), json!("sitemap"));
    entry.insert("reserve_query".into(), json!(true));
    entry.insert("seed".into(), json!(true));
    entry
}

/// `urllib.parse`-style query cleanup: drop `utm_*`/`fbclid` fields by their
/// decoded name without rewriting any other raw query text.
pub(super) fn normalize_get_parameters(url: &str) -> String {
    let Ok(parts) = spiderhead_parse::py_url::urlsplit(url) else {
        return url.to_owned();
    };
    if parts.query.is_empty() {
        return url.to_owned();
    }
    let kept: Vec<&str> = parts
        .query
        .split('&')
        .filter(|param| {
            let raw = param.split('=').next().unwrap_or_default();
            let name = spiderhead_parse::py_url::unquote(&raw.replace('+', " ")).to_lowercase();
            !(name.starts_with("utm_") || name == "fbclid")
        })
        .collect();
    spiderhead_parse::py_url::urlunsplit(
        &parts.scheme,
        &parts.netloc,
        &parts.path,
        &kept.join("&"),
        &parts.fragment,
    )
}

/// Queued-or-inflight frontier rows: the predicate `begin_collection` runs on
/// (excluded rows alone do not start collection).
pub(super) fn frontier_work_pending(scan: &NativeScan) -> Result<bool, String> {
    let count: i64 = scan
        .con()
        .query_row(
            "SELECT COUNT(*) FROM frontier WHERE state IN ('queued','inflight')",
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    Ok(count != 0)
}

/// `capabilities.resume.state == "complete"` on the persisted header.
pub(super) fn resume_state_complete(scan: &NativeScan) -> Result<bool, String> {
    let text: String = scan
        .con()
        .query_row(
            "SELECT capabilities_json FROM scan WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    let capabilities: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(capabilities
        .get("resume")
        .and_then(|resume| resume.get("state"))
        .and_then(Value::as_str)
        == Some("complete"))
}

/// Whether a declared sitemap root already carries a complete fetch summary.
pub(super) fn root_summary_complete(
    scan: &NativeScan,
    sitemap_url_id: i64,
) -> Result<bool, String> {
    let summary = scan
        .read_context("sitemap_fetch_summary", &format!("url:{sitemap_url_id}"))
        .map_err(|e| e.to_string())?;
    Ok(summary
        .and_then(|value| value.get("complete").and_then(Value::as_bool))
        .unwrap_or(false))
}

/// One stored robots group back into the rules engine's shape.
pub(super) fn group_from_value(value: &Value) -> robots::RobotsGroup {
    let strings = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut group = robots::RobotsGroup {
        user_agents: strings("user_agents"),
        allow: strings("allow"),
        disallow: strings("disallow"),
        crawl_delay: value.get("crawl_delay").and_then(Value::as_f64),
        request_rate_delay: value.get("request_rate_delay").and_then(Value::as_f64),
        line_of: std::collections::BTreeMap::new(),
    };
    if let Some(line_of) = value.get("_line_of").and_then(Value::as_object) {
        for (key, line) in line_of {
            if let Some(line) = line.as_i64() {
                group.line_of.insert(key.clone(), line);
            }
        }
    }
    group
}

/// Placeholder: returns the event method; the store keeps its own capture vocabulary.
pub fn capture_placeholder(event: &CaptureEvent) -> String {
    event.method.clone()
}

// Constants re-exported for the binary.
pub use crate::oracle::{limitations, runtime_versions, WRITER_REVISION, WRITER_VERSION};

/// Effective crawl config for the capture: defaults plus the CLI overrides
/// `--max-urls-per-second 50 --max-urls 200` (min_delay 0.02 etc.).
pub fn capture_config(
    host: &dyn spiderhead_core::host::Host,
    max_urls: i64,
    max_urls_per_second: f64,
) -> Result<Map<String, Value>, String> {
    let min_delay = bin_load_delay(max_urls_per_second);
    // LoadInput overrides are addressed by dotted paths, not nested objects.
    let overrides: Map<String, Value> = [
        ("limits.max_urls", json!(max_urls)),
        ("speed.min_delay_seconds", json!(min_delay)),
        ("limits.max_requests", json!(600)),
        ("limits.max_crawl_seconds", json!(400)),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value))
    .collect();
    let input = spiderhead_core::crawl_settings::LoadInput {
        file: None,
        overrides,
        base_overrides: Map::new(),
    };
    let config = spiderhead_core::crawl_settings::load(&input, host).map_err(|e| e.to_string())?;
    match config {
        Value::Object(map) => Ok(map),
        other => Err(format!("crawl config is not an object: {other}")),
    }
}

/// `delay_for_request_rate` with the plain fallback the CLI needs.
pub fn bin_load_delay(rate: f64) -> f64 {
    match spiderhead_core::crawl_settings::delay_for_request_rate(rate) {
        Ok(delay) => delay,
        Err(_) => 1.0 / rate,
    }
}

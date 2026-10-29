//! Validation of a resolved crawl configuration. Every check keeps its message text, so refusals stay stable.

use fancy_regex::Regex;
use serde_json::{Map, Value};

use super::flatten;
use super::{ConfigError, DEFAULT_SEGMENT};
use crate::host::Host;
use crate::net::resolve_proxy_route;
use crate::page_actions;
use crate::settings_data::{self, MAX_REQUESTS_CEILING, MAX_URLS_CEILING};

/// Allowed robots policies.
pub const ROBOTS_POLICIES: &[&str] = &["respect", "report_only", "ignore"];
/// Allowed scope.internal values.
pub const INTERNAL_SCOPES: &[&str] = &["host", "registrable_domain"];
/// Allowed cache modes.
pub const CACHE_MODES: &[&str] = &["live", "off", "replay"];
/// Allowed render modes.
pub const RENDER_MODES: &[&str] = &["raw", "legacy_fragment", "js"];
/// Allowed render viewports.
pub const RENDER_VIEWPORTS: &[&str] = &["desktop", "mobile"];
/// Allowed render wait states.
pub const RENDER_WAIT_UNTIL: &[&str] = &["load", "domcontentloaded", "networkidle"];
/// Headless engines Playwright can launch.
pub const RENDER_ENGINES: &[&str] = &["chromium", "firefox", "webkit"];
/// Engines that can emulate a mobile viewport.
pub const RENDER_MOBILE_EMULATION_ENGINES: &[&str] = &["chromium", "webkit"];
/// Largest custom viewport dimension in pixels.
pub const MAX_VIEWPORT_DIMENSION_PX: i64 = 16384;
/// Largest simultaneous render page count.
pub const MAX_RENDER_PAGE_CONCURRENCY: i64 = 16;
/// Header names that authenticate a request and may not use the global header lane.
pub const SENSITIVE_HEADER_NAMES: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "x-api-key",
    "x-auth-token",
];

static NULL: Value = Value::Null;

/// Look up a dotted path; a missing key reads as null.
pub(super) fn at<'a>(root: &'a Value, path: &str) -> &'a Value {
    path.split('.')
        .fold(root, |node, key| node.get(key).unwrap_or(&NULL))
}

/// Compile-check a regex with the crate's regex engine.
fn regex_ok(pattern: &str) -> Result<(), String> {
    Regex::new(pattern).map(|_| ()).map_err(|e| e.to_string())
}

/// Truthiness of a JSON value, as the config rules define it.
pub(super) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `${NAME}` as an env reference: `env:NAME` with a valid identifier.
pub(super) fn env_ref(value: &str) -> Option<&str> {
    let name = value.strip_prefix("env:")?;
    let mut chars = name.chars();
    let first = chars.next()?;
    let ok = (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    ok.then_some(name)
}

fn err<T>(message: impl Into<String>) -> Result<T, ConfigError> {
    Err(ConfigError(message.into()))
}

/// Reject anything that cannot be honoured, naming the offending path.
pub fn validate(config: &Value, host: &dyn Host) -> Result<(), ConfigError> {
    let known = flatten(settings_data::defaults());
    for path in flatten(config).keys() {
        if !known.contains_key(path) {
            return err(format!(
                "unknown setting {path:?}. A setting the crawler does not read would promise behaviour that does not exist"
            ));
        }
    }

    let robots = at(config, "robots.policy").as_str().unwrap_or_default();
    if !ROBOTS_POLICIES.contains(&robots) {
        return err(format!(
            "robots.policy must be one of {ROBOTS_POLICIES:?}, got {robots:?}"
        ));
    }
    let scope_internal = at(config, "scope.internal").as_str().unwrap_or_default();
    if !INTERNAL_SCOPES.contains(&scope_internal) {
        return err(format!(
            "scope.internal must be one of {INTERNAL_SCOPES:?}, got {scope_internal:?}"
        ));
    }
    if !at(config, "scope.normalize_tracking_parameters").is_boolean() {
        return err("scope.normalize_tracking_parameters must be boolean");
    }
    let cache_mode = at(config, "cache.mode").as_str().unwrap_or_default();
    if !CACHE_MODES.contains(&cache_mode) {
        return err(format!(
            "cache.mode must be one of {CACHE_MODES:?}, got {cache_mode:?}"
        ));
    }
    let auth_entries = at(config, "http.auth")
        .as_array()
        .is_some_and(|a| !a.is_empty());
    if auth_entries && cache_mode != "off" {
        return err("http.auth requires cache.mode=off to avoid reusing authenticated responses");
    }
    if !at(config, "http.zstd").is_boolean() {
        return err("http.zstd must be boolean");
    }
    if at(config, "http.zstd").as_bool() == Some(true) {
        if !host.zstd_available() {
            return err("http.zstd requires Python 3.14+ compression.zstd or the optional zstandard package");
        }
        if cache_mode != "off" {
            return err("http.zstd requires cache.mode=off so encoded responses are not mixed");
        }
    }
    if cache_mode == "replay" && at(config, "cache.invalidate").as_bool() == Some(true) {
        return err(
            "cache.invalidate cannot be combined with cache.mode='replay': replay promises to serve on-disk entries without ever touching the network, which invalidate exists to override. Pick one: cache.mode='live' with cache.invalidate=true for a forced hard refresh, or drop invalidate to keep replaying from disk.",
        );
    }
    if config.get("resources").is_some() {
        validate_resources(config)?;
    }
    if config.get("storage").is_some() {
        validate_storage(config)?;
    }
    for key in ["include_patterns", "exclude_patterns"] {
        for pattern in at(config, &format!("scope.{key}"))
            .as_array()
            .into_iter()
            .flatten()
        {
            let text = pattern.as_str().unwrap_or_default();
            regex_ok(text).map_err(|e| {
                ConfigError(format!("scope.{key}: {text:?} is not a valid regex: {e}"))
            })?;
        }
    }
    validate_file_type_filters(at(config, "scope"))?;

    validate_limits(config)?;

    for rule in at(config, "link_position.rules")
        .as_array()
        .into_iter()
        .flatten()
    {
        let ok =
            rule.get("position").is_some_and(truthy) && rule.get("selector").is_some_and(truthy);
        if !ok {
            return err(format!(
                "link_position.rules entries need both 'position' and 'selector'; got {rule}"
            ));
        }
    }

    validate_segments(at(config, "scope"))?;
    if let Some(thresholds) = at(config, "analysis.asset_thresholds").as_object() {
        for (name, value) in thresholds {
            match value.as_i64() {
                Some(n) if n > 0 => {}
                _ => {
                    return err(format!(
                        "analysis.asset_thresholds.{name} must be a positive SQLite-sized integer"
                    ))
                }
            }
        }
    }
    if !at(config, "analysis.segments").is_array() {
        return err("analysis.segments must be a list");
    }
    let exclusions = at(config, "analysis.finding_exclusions");
    crate::finding_exclusions::validate_rules(exclusions, None)
        .map_err(|e| ConfigError(format!("analysis.finding_exclusions: {}", e.0)))?;
    crate::canonical_policy::validate_canonical_policy(
        at(config, "analysis.canonical_policy"),
        "analysis.canonical_policy",
    )
    .map_err(|e| ConfigError(e.0))?;

    validate_http_headers(config)?;
    validate_credential_headers(config, host)?;
    validate_http_auth(config, host)?;

    let route = resolve_proxy(config, host)?;
    let identity = at(config, "http.proxy_identity");
    let authenticated = at(config, "http.proxy_authenticated");
    let (Some(identity), Some(authenticated)) = (identity.as_str(), authenticated.as_bool()) else {
        return err("http.proxy route facts are invalid");
    };
    match &route {
        Some(route) if !identity.is_empty() => {
            if identity != route.identity {
                return err("http.proxy_identity differs from the configured proxy");
            }
            if authenticated != route.authenticated {
                return err("http.proxy_authenticated differs from the configured proxy");
            }
        }
        None if !identity.is_empty() => {
            return err("http.proxy_identity differs from the configured proxy")
        }
        _ => {}
    }
    if identity.is_empty() && authenticated {
        return err("http.proxy_authenticated is managed by the crawler");
    }
    let proxy_set = at(config, "http.proxy")
        .as_str()
        .is_some_and(|s| !s.is_empty());
    if proxy_set && cache_mode != "off" {
        return err("http.proxy requires cache.mode=off to avoid mixing direct and proxy evidence");
    }
    validate_rendering(at(config, "rendering"))
}

fn validate_resources(config: &Value) -> Result<(), ConfigError> {
    let resources = at(config, "resources");
    if !at(resources, "fetch").is_boolean() {
        return err("resources.fetch must be boolean");
    }
    for name in ["max_requests", "max_response_bytes"] {
        match at(resources, name).as_i64() {
            Some(n) if n >= 0 => {}
            _ => {
                return err(format!(
                    "resources.{name} must be a nonnegative SQLite-sized integer"
                ))
            }
        }
    }
    let graph = at(resources, "graph");
    let Some(graph) = graph.as_object() else {
        return err("resources.graph must be an object");
    };
    for (name, value) in graph {
        match value.as_i64() {
            Some(n) if n >= 0 => {}
            _ => {
                return err(format!(
                    "resources.graph.{name} must be a nonnegative integer"
                ))
            }
        }
    }
    let int = |name: &str| graph.get(name).and_then(Value::as_i64).unwrap_or(0);
    if int("max_requests") < 1 || int("max_bytes_per_resource") < 1 {
        return err("resources.graph request and per-resource byte limits must be positive");
    }
    if int("max_origins") < 1 {
        return err("resources.graph.max_origins must be positive");
    }
    Ok(())
}

fn validate_storage(config: &Value) -> Result<(), ConfigError> {
    let storage = at(config, "storage");
    let format = at(storage, "format_version").as_str().unwrap_or_default();
    if !crate::artifacts::SCAN_FORMATS.contains(&format) {
        return err("storage.format_version must be scan.v1 or scan.v2");
    }
    match at(storage, "capacity_profile").as_str() {
        Some("stable") | Some("experimental_synthetic") => {}
        _ => return err("storage.capacity_profile must be stable or experimental_synthetic"),
    }
    let body_mode = at(storage, "body_mode").as_str().unwrap_or_default();
    if body_mode != "off" && body_mode != "captured_entity_bytes" {
        return err("storage.body_mode must be off or captured_entity_bytes");
    }
    for name in [
        "max_body_bytes",
        "max_body_store_bytes",
        "min_free_bytes",
        "history_warning_bytes",
    ] {
        match at(storage, name).as_i64() {
            Some(n) if n >= 0 => {}
            _ => {
                return err(format!(
                    "storage.{name} must be a nonnegative SQLite-sized byte count"
                ))
            }
        }
    }
    if body_mode != "off" && at(storage, "max_body_bytes").as_i64() == Some(0) {
        return err("storage.max_body_bytes must be positive when body capture is enabled");
    }
    Ok(())
}

fn validate_limits(config: &Value) -> Result<(), ConfigError> {
    let max_urls = at(config, "limits.max_urls").as_i64();
    match max_urls {
        Some(n) if n >= 0 => {}
        _ => return err("limits.max_urls must be a nonnegative integer"),
    }
    let max_urls = max_urls.unwrap_or(0);
    if max_urls as u64 > MAX_URLS_CEILING {
        return err(format!(
            "limits.max_urls is {max_urls}, above this crawler's ceiling of {MAX_URLS_CEILING}; narrow the scope or split the work into resumable scans."
        ));
    }
    match at(config, "limits.max_depth").as_i64() {
        Some(n) if n >= -1 => {}
        _ => return err("limits.max_depth must be an integer of at least -1"),
    }
    let max_requests = at(config, "limits.max_requests").as_i64();
    match max_requests {
        Some(n) if n >= 0 => {}
        _ => return err("limits.max_requests must be a nonnegative integer"),
    }
    if max_requests.unwrap_or(0) as u64 > MAX_REQUESTS_CEILING {
        return err(format!(
            "limits.max_requests is above the crawler request ceiling of {MAX_REQUESTS_CEILING}; split the work into resumable scans instead"
        ));
    }
    if at(config, "limits.max_query_variants_per_path")
        .as_i64()
        .unwrap_or(0)
        < 0
    {
        return err("limits.max_query_variants_per_path cannot be negative");
    }
    if at(config, "speed.min_delay_seconds")
        .as_f64()
        .unwrap_or(0.0)
        < 0.0
    {
        return err("speed.min_delay_seconds cannot be negative");
    }
    if at(config, "speed.concurrency").as_i64().unwrap_or(0) < 1 {
        return err("speed.concurrency must be at least 1");
    }
    match at(config, "fetch.retry_server_errors").as_i64() {
        Some(n) if (0..=3).contains(&n) => {}
        _ => return err("fetch.retry_server_errors must be an integer between 0 and 3"),
    }
    let store = at(config, "discovery.external.store").as_bool();
    let crawl = at(config, "discovery.external.crawl").as_bool();
    if store.is_none() || crawl.is_none() {
        return err("discovery.external.store and discovery.external.crawl must be boolean");
    }
    if crawl == Some(true) && store == Some(false) {
        return err(
            "discovery.external.crawl requires discovery.external.store: destinations are checked from the recorded edges, and store=False keeps no edges",
        );
    }
    for name in [
        "max_targets",
        "max_hosts",
        "max_requests",
        "max_depth",
        "max_redirects",
    ] {
        match at(config, &format!("external_checks.{name}")).as_i64() {
            Some(n) if n >= 0 => {}
            _ => {
                return err(format!(
                    "external_checks.{name} must be a nonnegative integer"
                ))
            }
        }
    }
    if at(config, "external_checks.max_redirects")
        .as_i64()
        .unwrap_or(0)
        > 10
    {
        return err("external_checks.max_redirects cannot exceed 10 (the redirect-chain ceiling)");
    }
    let unbounded = max_urls == 0
        && matches!(at(config, "limits.max_depth").as_i64(), Some(-1) | Some(0))
        && at(config, "limits.max_crawl_seconds").as_i64().unwrap_or(0) == 0
        && at(config, "limits.max_requests").as_i64().unwrap_or(0) == 0
        && at(config, "storage.min_free_bytes").as_i64().unwrap_or(0) == 0;
    if unbounded {
        return err(
            "a crawl needs at least one budget or a positive storage.min_free_bytes reserve",
        );
    }
    Ok(())
}

/// Validate the URL-suffix and response-media filters as separate contracts.
fn validate_file_type_filters(scope: &Value) -> Result<(), ConfigError> {
    let extension =
        Regex::new(r"^[A-Za-z0-9][A-Za-z0-9_-]*$").map_err(|e| ConfigError(e.to_string()))?;
    let token = r"[A-Za-z0-9!#$%&'+\-.^_`|~]+";
    let media = Regex::new(&format!(r"^{token}/(?:{token}|\*)$"))
        .map_err(|e| ConfigError(e.to_string()))?;
    for name in ["include_extensions", "exclude_extensions"] {
        let setting = format!("scope.{name}");
        let Some(values) = scope.get(name).and_then(Value::as_array) else {
            return err(format!("{setting} must be a list of filename extensions"));
        };
        let mut seen = std::collections::BTreeSet::new();
        for value in values {
            let text = value.as_str().ok_or_else(|| {
                ConfigError(format!("{setting} entries must be strings, got {value}"))
            })?;
            let bare = text.strip_prefix('.').unwrap_or(text);
            if !extension.is_match(bare).unwrap_or(false) {
                return err(format!(
                    "{setting} entry {text:?} must be one filename suffix (letters, digits, underscore or hyphen; optional leading dot)"
                ));
            }
            let normalized = bare.to_lowercase();
            if !seen.insert(normalized.clone()) {
                return err(format!(
                    "{setting} contains duplicate suffix '{normalized}' after normalization"
                ));
            }
        }
    }
    for name in ["include_media_types", "exclude_media_types"] {
        let setting = format!("scope.{name}");
        let Some(values) = scope.get(name).and_then(Value::as_array) else {
            return err(format!("{setting} must be a list of media types"));
        };
        let mut seen = std::collections::BTreeSet::new();
        for value in values {
            let text = value.as_str().unwrap_or_default();
            if value.as_str().is_none() || !media.is_match(text).unwrap_or(false) {
                return err(format!(
                    "{setting} entry {value} must be an exact type or type wildcard such as text/html or image/*"
                ));
            }
            let normalized = text.to_lowercase();
            if !seen.insert(normalized.clone()) {
                return err(format!(
                    "{setting} contains duplicate media type '{normalized}' after normalization"
                ));
            }
        }
    }
    Ok(())
}

/// Every segment is named, ordered and matches at least one declared rule.
/// `segments_only` may name only declared segments or the default.
fn validate_segments(scope: &Value) -> Result<(), ConfigError> {
    let Some(segments) = scope.get("segments").and_then(Value::as_array) else {
        return err("scope.segments must be a list");
    };
    let mut names = std::collections::BTreeSet::new();
    for entry in segments {
        let Some(object) = entry.as_object() else {
            return err(format!(
                "scope.segments entries must be objects, got {entry}"
            ));
        };
        let mut extra: Vec<&String> = object
            .keys()
            .filter(|key| !matches!(key.as_str(), "name" | "prefix" | "host" | "pattern"))
            .collect();
        if !extra.is_empty() {
            extra.sort();
            return err(format!(
                "scope.segments entry has unknown keys {extra:?}: {entry}"
            ));
        }
        let name = object
            .get("name")
            .and_then(Value::as_str)
            .filter(|n| !n.is_empty());
        let Some(name) = name else {
            return err(format!(
                "scope.segments entry needs a non-empty 'name': {entry}"
            ));
        };
        if name == DEFAULT_SEGMENT {
            return err(format!(
                "scope.segments entry cannot be named {DEFAULT_SEGMENT:?}: that name is reserved for a URL matching none of the declared segments"
            ));
        }
        if !names.insert(name.to_owned()) {
            return err(format!("scope.segments has duplicate name {name:?}"));
        }
        let set = |key: &str| object.get(key).is_some_and(truthy);
        if !set("prefix") && !set("host") && !set("pattern") {
            return err(format!(
                "scope.segments entry {name:?} needs at least one of 'prefix', 'host' or 'pattern' to match by"
            ));
        }
        if let Some(pattern) = object
            .get("pattern")
            .and_then(Value::as_str)
            .filter(|p| !p.is_empty())
        {
            regex_ok(pattern).map_err(|e| {
                ConfigError(format!(
                    "scope.segments entry {name:?}: pattern {pattern:?} is not a valid regex: {e}"
                ))
            })?;
        }
    }
    let Some(only) = scope.get("segments_only").and_then(Value::as_array) else {
        return err("scope.segments_only must be a list");
    };
    let mut allowed = names;
    allowed.insert(DEFAULT_SEGMENT.to_owned());
    let unknown: Vec<String> = only
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_owned())
        .filter(|n| !allowed.contains(n))
        .collect();
    if !unknown.is_empty() {
        return err(format!(
            "scope.segments_only names {unknown:?} that scope.segments never declares (known: {allowed:?})"
        ));
    }
    Ok(())
}

mod credentials;
mod rendering;
use credentials::{validate_credential_headers, validate_http_auth, validate_http_headers};
use rendering::validate_rendering;

/// Resolve `http.proxy` through the net layer.
pub fn resolve_proxy(
    config: &Value,
    host: &dyn Host,
) -> Result<Option<crate::net::ProxyRoute>, ConfigError> {
    let allow_private = at(config, "http.proxy_allow_private")
        .as_bool()
        .ok_or_else(|| ConfigError("http.proxy_allow_private must be true or false".into()))?;
    let value = match at(config, "http.proxy") {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        _ => return err("http.proxy must be a URL or env:VARIABLE reference"),
    };
    resolve_proxy_route(&value, allow_private, host).map_err(ConfigError)
}

//! Crawler configuration: defaults, precedence, validation and the run manifest.
//!
//! Three rules matter more than the field list: unknown keys are refused by path, store and crawl are independent questions, and every setting that changes what an audit finds is recorded in the manifest so two reports are comparable.
//!
//! Defaults and descriptions live in [`crate::settings_data`]; this module owns load, validate and merge.
//! Settings are `serde_json::Value` trees, so dotted-path lookups work the same way for every key.

mod pynum;
mod pynum_data;
mod report;
mod validate;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};
use thiserror::Error;

use crate::envvar::read_env_named;
use crate::finding_exclusions::validate_rules;
use crate::host::Host;
use crate::settings_data::{
    self, BUDGET_REQUESTS_PER_PAGE, BUDGET_SECONDS_PER_PAGE, MAX_MATERIALIZED_URLS,
    MAX_REQUESTS_CEILING, MAX_URLS_CEILING,
};
use crate::url_normalize::is_py_space;

pub use report::{
    describe_settings, effective_request_rate, fingerprint, load_estimate, manifest, rate_fields,
    rate_warnings, redact_sensitive_headers, request_headers, resolve_credential_headers,
    RATE_WARN_RPS,
};
pub use validate::{resolve_proxy, validate};

/// Name of the segment every URL falls into when it matches no declared segment (reserved).
pub const DEFAULT_SEGMENT: &str = "default";

/// Environment overrides, applied between the file and explicit arguments. The legacy spelling
/// is the key; the reader also accepts the `SPIDERHEAD_` form (see [`read_env_named`]).
pub const ENV_OVERRIDES: &[(&str, &str)] = &[
    ("SEOHEAD_CRAWL_MAX_URLS", "limits.max_urls"),
    ("SEOHEAD_CRAWL_MAX_DEPTH", "limits.max_depth"),
    ("SEOHEAD_CRAWL_MIN_DELAY", "speed.min_delay_seconds"),
    ("SEOHEAD_CRAWL_ROBOTS", "robots.policy"),
    ("SEOHEAD_CRAWL_USER_AGENT", "http.user_agent"),
];

/// Path patterns appended to the scope once authenticated crawling is configured. These are a
/// default, not a guarantee: a caller who has audited the site may still widen the scope.
pub const DESTRUCTIVE_PATH_PATTERNS: &[&str] = &[
    r"(?i)/(log[-_]?out|sign[-_]?out)(?:/|$|\?)",
    r"(?i)/(delete|remove|destroy|unsubscribe)(?:/|$|\?)",
];

/// A configuration that cannot be trusted to mean what it says.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct ConfigError(pub String);

/// Dotted paths to leaf values. Free-form maps are leaves, not branches; `http.headers` is one.
pub fn flatten(value: &Value) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    flatten_into("", value, &mut out);
    out
}

fn flatten_into(prefix: &str, value: &Value, out: &mut BTreeMap<String, Value>) {
    let Some(map) = value.as_object() else {
        return;
    };
    for (key, item) in map {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        if item.is_object() && path != "http.headers" {
            flatten_into(&path, item, out);
        } else {
            out.insert(path, item.clone());
        }
    }
}

/// Set a dotted path in place, creating intermediate objects.
pub fn set_path(root: &mut Value, path: &str, value: Value) {
    let mut node = root;
    let parts: Vec<&str> = path.split('.').collect();
    for part in &parts[..parts.len() - 1] {
        if !node.get(*part).is_some_and(Value::is_object) {
            node[*part] = Value::Object(Map::new());
        }
        node = &mut node[*part];
    }
    node[parts[parts.len() - 1]] = value;
}

/// Deep-merge `overlay` onto `base`; non-object values replace.
pub fn merge(base: &Value, overlay: &Value) -> Value {
    let (Some(base_map), Some(overlay_map)) = (base.as_object(), overlay.as_object()) else {
        return overlay.clone();
    };
    let mut out = base_map.clone();
    for (key, value) in overlay_map {
        let merged = match out.get(key) {
            Some(existing) if existing.is_object() && value.is_object() => merge(existing, value),
            _ => value.clone(),
        };
        out.insert(key.clone(), merged);
    }
    Value::Object(out)
}

/// Coerce an environment or command-line string to the type of the default at `path`.
pub fn coerce(path: &str, raw: &str) -> Result<Value, String> {
    let defaults = flatten(settings_data::defaults());
    match defaults.get(path) {
        Some(Value::Bool(_)) => match raw.trim_matches(is_py_space).to_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(Value::Bool(true)),
            "0" | "false" | "no" | "off" => Ok(Value::Bool(false)),
            _ => Err("expected true or false".into()),
        },
        Some(Value::Number(number)) if number.is_f64() => {
            pynum::py_float(raw).ok_or_else(|| "could not convert string to float".into())
        }
        Some(Value::Number(_)) => {
            pynum::py_int(raw).ok_or_else(|| "invalid literal for int() with base 10".into())
        }
        Some(Value::Array(_)) => Ok(Value::Array(
            raw.split(',')
                .map(|item| item.trim_matches(is_py_space))
                .filter(|item| !item.is_empty())
                .map(|item| Value::String(item.to_owned()))
                .collect(),
        )),
        Some(Value::Object(_)) => match serde_json::from_str::<Value>(raw) {
            Ok(Value::Object(map)) if map.values().all(Value::is_string) => Ok(Value::Object(map)),
            _ => Err("expected a JSON object with string keys and values".into()),
        },
        _ => Ok(Value::String(raw.to_owned())),
    }
}

/// Turns one `path=value` argument into a validated override.
pub fn parse_setting_assignment(text: &str) -> Result<(String, Value), ConfigError> {
    let malformed = || {
        // A proxy URL may carry inline credentials: never echo it back.
        if text.to_lowercase().contains("proxy") {
            ConfigError("--set expects PATH=VALUE for http.proxy".into())
        } else {
            ConfigError(format!("--set expects PATH=VALUE, got {text:?}"))
        }
    };
    let Some((path, raw)) = text.split_once('=') else {
        return Err(malformed());
    };
    let path = path.trim();
    if path.is_empty() {
        return Err(malformed());
    }
    let known = flatten(settings_data::defaults());
    if !known.contains_key(path) {
        let leaf = path.rsplit('.').next().unwrap_or(path);
        let near = known.keys().find(|k| k.rsplit('.').next() == Some(leaf));
        let hint = match near {
            Some(near) => format!("; did you mean {near}?"),
            None => "; see --config-help".to_owned(),
        };
        return Err(ConfigError(format!("unknown setting {path:?}{hint}")));
    }
    match coerce(path, raw) {
        Ok(value) => Ok((path.to_owned(), value)),
        Err(_) if path == "http.proxy" => Err(ConfigError("http.proxy value is invalid".into())),
        Err(reason) => Err(ConfigError(format!(
            "{path}={raw:?} is not valid: {reason}"
        ))),
    }
}

/// The `speed.min_delay_seconds` that caps a crawl at `rate` requests per second.
pub fn delay_for_request_rate(rate: f64) -> Result<f64, ConfigError> {
    if rate <= 0.0 {
        return Err(ConfigError(
            "--max-urls-per-second must be greater than 0".into(),
        ));
    }
    Ok(1.0 / rate)
}

/// How many URLs a crawl may fetch. An over-large budget is refused, never quietly reduced.
pub fn checked_url_budget(max_urls: u64, materialized: bool) -> Result<u64, ConfigError> {
    if max_urls == 0 {
        if materialized {
            return Err(ConfigError(
                "an unlimited URL population requires a native SQLite site scan".into(),
            ));
        }
        return Ok(0);
    }
    let budget = max_urls.max(1);
    if budget > MAX_URLS_CEILING {
        return Err(ConfigError(format!(
            "max_urls is {budget}, above this crawler's ceiling of {MAX_URLS_CEILING}; narrow the scope or split the work into resumable scans"
        )));
    }
    if materialized && budget > MAX_MATERIALIZED_URLS {
        return Err(ConfigError(format!(
            "materialized legacy collection ceiling is {MAX_MATERIALIZED_URLS} URLs; use a native SQLite scan for an explicit URL list or site crawl for larger populations"
        )));
    }
    Ok(budget)
}

/// Fills unset request and time budgets from an explicit page budget, in place.
pub fn bind_budgets_to_page_budget(settings: &mut Value, explicit: &BTreeSet<String>) {
    let pages = settings
        .pointer("/limits/max_urls")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if pages == 0 {
        return;
    }
    let defaults = settings_data::defaults();
    let default_requests = defaults.pointer("/limits/max_requests").cloned();
    let default_seconds = defaults.pointer("/limits/max_crawl_seconds").cloned();
    let current_requests = settings.pointer("/limits/max_requests").cloned();
    let current_seconds = settings.pointer("/limits/max_crawl_seconds").cloned();
    if !explicit.contains("limits.max_requests") && current_requests == default_requests {
        let value = (BUDGET_REQUESTS_PER_PAGE * pages).min(MAX_REQUESTS_CEILING);
        set_path(settings, "limits.max_requests", Value::from(value));
    }
    if !explicit.contains("limits.max_crawl_seconds") && current_seconds == default_seconds {
        set_path(
            settings,
            "limits.max_crawl_seconds",
            Value::from(BUDGET_SECONDS_PER_PAGE * pages),
        );
    }
}

/// Inputs to [`load`], in the order they are applied.
#[derive(Debug, Default)]
pub struct LoadInput<'a> {
    /// `(label, JSON text)` of the config file, if one was given.
    pub file: Option<(&'a str, &'a str)>,
    /// Explicit dotted overrides; a `null` value is skipped.
    pub overrides: Map<String, Value>,
    /// Base overrides applied before the file; a `null` value is skipped.
    pub base_overrides: Map<String, Value>,
}

/// Resolve defaults, base overrides, file, environment, then explicit overrides.
pub fn load(input: &LoadInput<'_>, host: &dyn Host) -> Result<Value, ConfigError> {
    let mut config = settings_data::defaults().clone();
    let defaults = flatten(settings_data::defaults());

    for (setting, value) in &input.base_overrides {
        if value.is_null() {
            continue;
        }
        if value.is_object() && !defaults.get(setting).is_some_and(Value::is_object) {
            return Err(ConfigError(format!(
                "base override {setting:?} is a mapping; use a dotted setting path instead"
            )));
        }
        set_path(&mut config, setting, value.clone());
    }

    if let Some((label, text)) = input.file {
        let parsed: Value = serde_json::from_str(text)
            .map_err(|e| ConfigError(format!("config {label:?} is not valid JSON: {e}")))?;
        if !parsed.is_object() {
            return Err(ConfigError(format!(
                "config {label:?} must contain an object"
            )));
        }
        config = merge(&config, &parsed);
    }

    for (name, setting) in ENV_OVERRIDES {
        let Some((variable, raw)) = read_env_named(host, name) else {
            continue;
        };
        let value = coerce(setting, &raw).map_err(|e| {
            ConfigError(format!(
                "{variable}={raw:?} is not valid for {setting}: {e}"
            ))
        })?;
        set_path(&mut config, setting, value);
    }

    for (setting, value) in &input.overrides {
        if value.is_null() {
            continue;
        }
        if value.is_object() && !defaults.get(setting).is_some_and(Value::is_object) {
            let key = value
                .as_object()
                .and_then(|m| m.keys().next())
                .map_or("key", String::as_str);
            return Err(ConfigError(format!(
                "override {setting:?} is a mapping; use dotted paths such as {setting}.{key} so sibling defaults survive"
            )));
        }
        set_path(&mut config, setting, value.clone());
    }

    let http_auth = config
        .pointer("/http/auth")
        .and_then(Value::as_array)
        .is_some_and(|a| !a.is_empty());
    let credentials = config
        .pointer("/http/credential_headers")
        .and_then(Value::as_array)
        .is_some_and(|a| !a.is_empty());
    if credentials || http_auth {
        let mut patterns: Vec<Value> = config
            .pointer("/scope/exclude_patterns")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for pattern in DESTRUCTIVE_PATH_PATTERNS {
            if !patterns.iter().any(|p| p.as_str() == Some(pattern)) {
                patterns.push(Value::from(*pattern));
            }
        }
        set_path(
            &mut config,
            "scope.exclude_patterns",
            Value::Array(patterns),
        );
    }

    let exclusions = config
        .pointer("/analysis/finding_exclusions")
        .cloned()
        .unwrap_or(Value::Null);
    let rules = validate_rules(&exclusions, None)
        .map_err(|e| ConfigError(format!("analysis.finding_exclusions: {}", e.0)))?;
    set_path(
        &mut config,
        "analysis.finding_exclusions",
        Value::Array(
            rules
                .iter()
                .map(crate::finding_exclusions::Rule::to_value)
                .collect(),
        ),
    );

    let mut http = config
        .get("http")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut robots = config
        .get("robots")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    crate::user_agents::apply_defaults(&mut http, &mut robots)
        .map_err(|_| ConfigError("invalid user agent setting".into()))?;
    config["http"] = Value::Object(http);
    config["robots"] = Value::Object(robots);

    validate(&config, host)?;
    let route = resolve_proxy(&config, host)?;
    set_path(
        &mut config,
        "http.proxy_identity",
        Value::from(
            route
                .as_ref()
                .map(|r| r.identity.clone())
                .unwrap_or_default(),
        ),
    );
    set_path(
        &mut config,
        "http.proxy_authenticated",
        Value::Bool(route.as_ref().is_some_and(|r| r.authenticated)),
    );
    Ok(config)
}

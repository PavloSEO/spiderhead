//! Run manifest, settings description, fingerprint and rate reporting, plus credential redaction helpers.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::flatten;
use super::validate::{at, env_ref, resolve_proxy, SENSITIVE_HEADER_NAMES};
use crate::common::python_json;
use crate::host::Host;
use crate::settings_data::{self, RESULTS_AFFECTING};

/// Above this worst-case rate the crawl still runs; the operator is told, not stopped.
pub const RATE_WARN_RPS: f64 = 2.0;

/// Replaces the value of every credential-looking header with `"REDACTED"`. Non-object input passes through unchanged.
pub fn redact_sensitive_headers(headers: &Value) -> Value {
    let Some(map) = headers.as_object() else {
        return headers.clone();
    };
    Value::Object(
        map.iter()
            .map(|(name, value)| {
                let sensitive = SENSITIVE_HEADER_NAMES.contains(&name.to_lowercase().as_str());
                (
                    name.clone(),
                    if sensitive {
                        json!("REDACTED")
                    } else {
                        value.clone()
                    },
                )
            })
            .collect(),
    )
}

/// Host and header names survive into the manifest; the values never do.
fn redact_credential_headers(entries: &Value) -> Value {
    Value::Array(
        entries
            .as_array()
            .into_iter()
            .flatten()
            .map(|entry| {
                let headers: Map<String, Value> = entry
                    .get("headers")
                    .and_then(Value::as_object)
                    .into_iter()
                    .flatten()
                    .map(|(name, _)| (name.clone(), json!("REDACTED")))
                    .collect();
                json!({"host": entry.get("host").cloned().unwrap_or(json!("")), "headers": headers})
            })
            .collect(),
    )
}

/// Keep the authentication method and host, never the environment references.
fn redact_http_auth(entries: &Value) -> Value {
    Value::Array(
        entries
            .as_array()
            .into_iter()
            .flatten()
            .map(|entry| {
                json!({
                    "host": entry.get("host").cloned().unwrap_or(json!("")),
                    "scheme": entry.get("scheme").cloned().unwrap_or(json!("")),
                })
            })
            .collect(),
    )
}

/// The resolved values of every setting that can change what was found.
pub fn manifest(
    config: &Value,
    host: &dyn Host,
) -> Result<BTreeMap<String, Value>, super::ConfigError> {
    let flat = flatten(config);
    let mut out = BTreeMap::new();
    let mut paths: Vec<&str> = RESULTS_AFFECTING.to_vec();
    paths.sort_unstable();
    for path in paths {
        let Some(value) = flat.get(path) else {
            continue;
        };
        let rendered = match path {
            "http.headers" => redact_sensitive_headers(value),
            "http.credential_headers" => redact_credential_headers(value),
            "http.auth" => redact_http_auth(value),
            "http.proxy" => {
                let mut identity = at(config, "http.proxy_identity")
                    .as_str()
                    .unwrap_or("")
                    .to_owned();
                let mut authenticated = at(config, "http.proxy_authenticated")
                    .as_bool()
                    .unwrap_or(false);
                let truthy = value.as_str().is_some_and(|s| !s.is_empty());
                if identity.is_empty() && truthy {
                    let route = resolve_proxy(config, host)?;
                    identity = route
                        .as_ref()
                        .map(|r| r.identity.clone())
                        .unwrap_or_default();
                    authenticated = route.as_ref().is_some_and(|r| r.authenticated);
                }
                json!({
                    "mode": if identity.is_empty() { "direct" } else { "proxy" },
                    "endpoint": if identity.is_empty() { Value::Null } else { json!(identity) },
                    "authenticated": authenticated,
                })
            }
            _ => value.clone(),
        };
        out.insert(path.to_owned(), rendered);
    }
    Ok(out)
}

/// Every configurable setting with its type, default, classification and description.
pub fn describe_settings() -> Vec<Value> {
    flatten(settings_data::defaults())
        .into_iter()
        .map(|(path, default)| {
            let type_name = match &default {
                Value::Bool(_) => "bool",
                Value::Number(n) if n.is_f64() => "float",
                Value::Number(_) => "int",
                Value::String(_) => "str",
                Value::Array(_) => "list",
                Value::Object(_) => "dict",
                Value::Null => "NoneType",
            };
            json!({
                "path": path,
                "type": type_name,
                "default": default,
                "results_affecting": RESULTS_AFFECTING.contains(&path.as_str()),
                "description": settings_data::description(&path).unwrap_or(""),
            })
        })
        .collect()
}

/// Stable identity for "this configuration", hashed over the manifest.
pub fn fingerprint(config: &Value, host: &dyn Host) -> Result<String, super::ConfigError> {
    let manifest: Map<String, Value> = manifest(config, host)?.into_iter().collect();
    let payload = python_json(&Value::Object(manifest));
    let digest = Sha256::digest(payload.as_bytes());
    Ok(digest.iter().take(8).map(|b| format!("{b:02x}")).collect())
}

/// Worst-case requests per second: `1 / min_delay`, or infinity without a delay.
pub fn effective_request_rate(config: &Value) -> f64 {
    let delay = at(config, "speed.min_delay_seconds")
        .as_f64()
        .unwrap_or(0.0);
    if delay > 0.0 {
        1.0 / delay
    } else {
        f64::INFINITY
    }
}

/// Warnings about the worst-case rate; empty at or under [`RATE_WARN_RPS`].
pub fn rate_warnings(config: &Value) -> Vec<String> {
    let rate = effective_request_rate(config);
    if rate <= RATE_WARN_RPS {
        return vec![];
    }
    let shown = if rate.is_infinite() {
        "unbounded".to_owned()
    } else {
        format!("{rate:.2}")
    };
    vec![format!(
        "effective rate {shown} req/s is above {RATE_WARN_RPS} req/s; confirm the host tolerates it"
    )]
}

/// Configured rate, a duration floor, and a null disk estimate, since disk use cannot be known before the run.
pub fn load_estimate(config: &Value) -> Value {
    let rate = effective_request_rate(config);
    let budget = at(config, "limits.max_urls").as_f64().unwrap_or(0.0);
    let duration = (budget > 0.0 && rate > 0.0 && rate.is_finite()).then(|| budget / rate);
    let duration_reason = match duration {
        Some(_) => Value::Null,
        None => json!("a finite URL population and finite request rate are required"),
    };
    json!({
        "format": "crawl-load-estimate.v1",
        "request_rate_rps": if rate.is_infinite() { Value::Null } else { json!(rate) },
        "request_rate_state": if rate.is_infinite() { "unbounded" } else { "configured_limit" },
        "duration_floor_seconds": duration,
        "duration_reason": duration_reason,
        "disk_estimate_bytes": Value::Null,
        "disk_reason": "retained page, link and body sizes vary; configured storage caps are limits, not a measured estimate",
        "body_storage_budget_bytes": at(config, "storage.max_body_store_bytes").clone(),
    })
}

/// Configured rate, warnings and nullable pre-run capacity estimates.
pub fn rate_fields(config: &Value) -> Value {
    let rate = effective_request_rate(config);
    json!({
        "effective_max_requests_per_second": if rate.is_infinite() { json!("unbounded") } else { json!(rate) },
        "warnings": rate_warnings(config),
        "load_estimate": load_estimate(config),
    })
}

/// Headers bound to `host`, with their environment references resolved fresh each call
/// Environment references are resolved on every call. A redirect to another host gets nothing back.
pub fn resolve_credential_headers(
    entries: &[Value],
    host_name: &str,
    env: &dyn Host,
) -> Map<String, Value> {
    let wanted = host_name.to_lowercase();
    let mut resolved = Map::new();
    for entry in entries {
        if entry
            .get("host")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase()
            != wanted
        {
            continue;
        }
        for (name, reference) in entry
            .get("headers")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            if let Some(var) = env_ref(reference.as_str().unwrap_or("")) {
                resolved.insert(name.clone(), json!(env.var(var).unwrap_or_default()));
            }
        }
    }
    resolved
}

/// Explicit non-secret request headers, with opt-in zstd negotiation.
pub fn request_headers(config: &Value) -> Option<Map<String, Value>> {
    let mut headers = at(config, "http.headers")
        .as_object()
        .cloned()
        .unwrap_or_default();
    if at(config, "http.zstd").as_bool() == Some(true) {
        let accept_key = headers
            .keys()
            .find(|name| name.eq_ignore_ascii_case("accept-encoding"))
            .cloned()
            .unwrap_or_else(|| "Accept-Encoding".to_owned());
        let current = headers
            .get(&accept_key)
            .and_then(Value::as_str)
            .unwrap_or("gzip, deflate")
            .to_owned();
        let mut accepted: Vec<String> = current
            .split(',')
            .map(|p| p.trim().to_lowercase())
            .collect();
        if !accepted.iter().any(|a| a == "zstd") {
            accepted.push("zstd".to_owned());
        }
        headers.insert(accept_key, json!(accepted.join(", ")));
    }
    (!headers.is_empty()).then_some(headers)
}

//! The `crawl.settings.manifest` projection: the resolved value of every setting
//! that can change what a crawl found, with credential values redacted to names.
//!
//! Kept local: the crawl crate is not a dependency of this crate.

use std::collections::BTreeMap;
use std::net::ToSocketAddrs;

use serde_json::{json, Map, Value};

const SENSITIVE_HEADER_NAMES: [&str; 5] = [
    "authorization",
    "proxy-authorization",
    "cookie",
    "x-api-key",
    "x-auth-token",
];

use spiderhead_core::settings_data::RESULTS_AFFECTING;

/// Flatten nested settings to dotted paths; free-form maps are leaves.
fn flatten(mapping: &Map<String, Value>, prefix: &str, out: &mut BTreeMap<String, Value>) {
    for (key, value) in mapping {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        if value.is_object() && path != "http.headers" {
            if let Value::Object(child) = value {
                flatten(child, &path, out);
            }
        } else {
            out.insert(path, value.clone());
        }
    }
}

/// Redact sensitive header values, keeping their names.
fn redact_sensitive_headers(headers: &Value) -> Value {
    let Value::Object(map) = headers else {
        return headers.clone();
    };
    Value::Object(
        map.iter()
            .map(|(name, value)| {
                let redacted = SENSITIVE_HEADER_NAMES
                    .iter()
                    .any(|s| name.eq_ignore_ascii_case(s));
                (
                    name.clone(),
                    if redacted {
                        Value::String("REDACTED".into())
                    } else {
                        value.clone()
                    },
                )
            })
            .collect(),
    )
}

/// Redact credential-bearing headers.
fn redact_credential_headers(entries: &Value) -> Value {
    let Value::Array(items) = entries else {
        return entries.clone();
    };
    Value::Array(
        items
            .iter()
            .map(|entry| {
                let host = entry
                    .get("host")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let headers: Map<String, Value> = entry
                    .get("headers")
                    .and_then(Value::as_object)
                    .map(|m| {
                        m.keys()
                            .map(|k| (k.clone(), Value::String("REDACTED".into())))
                            .collect()
                    })
                    .unwrap_or_default();
                json!({"host": host, "headers": headers})
            })
            .collect(),
    )
}

/// Redact HTTP auth credentials.
fn redact_http_auth(entries: &Value) -> Value {
    let Value::Array(items) = entries else {
        return entries.clone();
    };
    Value::Array(
        items
            .iter()
            .map(|entry| {
                json!({
                    "host": entry.get("host").and_then(Value::as_str).unwrap_or_default(),
                    "scheme": entry.get("scheme").and_then(Value::as_str).unwrap_or_default(),
                })
            })
            .collect(),
    )
}

/// Resolve the proxy route to the identity the manifest needs: `http://host:port`
/// and an authenticated flag. Errors are returned so the caller marks the settings
/// projection unavailable.
fn resolve_proxy_identity(config: &Map<String, Value>) -> Result<Option<(String, bool)>, String> {
    let http = config.get("http").and_then(Value::as_object);
    let Some(http) = http else {
        return Ok(None);
    };
    let value = http.get("proxy").cloned().unwrap_or(Value::Null);
    let allow_private = http.get("proxy_allow_private");
    if !matches!(allow_private, Some(Value::Bool(_))) {
        return Err("http.proxy_allow_private must be true or false".into());
    }
    let value = match &value {
        Value::String(s) if s.is_empty() => return Ok(None),
        Value::String(s) => s.clone(),
        _ => return Err("http.proxy must be a URL or env:VARIABLE reference".into()),
    };
    let env_ref = regex::Regex::new(r"^env:([A-Za-z_][A-Za-z0-9_]*)$").ok();
    let reference = env_ref.and_then(|re| re.captures(&value).map(|c| c[1].to_owned()));
    let from_env = reference.is_some();
    if value.starts_with("env:") && !from_env {
        return Err("http.proxy needs a valid env:VARIABLE reference".into());
    }
    let raw = match &reference {
        Some(name) => spiderhead_core::envvar::read_env(&spiderhead_core::host::SystemHost, name)
            .unwrap_or_default(),
        None => value.clone(),
    };
    if raw.is_empty() {
        return Err("http.proxy environment reference is missing or empty".into());
    }
    if raw.chars().any(|c| (c as u32) < 33 || (c as u32) == 127) {
        return Err("http.proxy URL contains whitespace or control characters".into());
    }
    let parts = url::Url::parse(&raw).map_err(|_| "http.proxy URL is malformed".to_string())?;
    if parts.scheme() != "http" {
        return Err("http.proxy supports only an http:// forward proxy".into());
    }
    let host = parts.host_str().unwrap_or_default().to_owned();
    let port = parts.port();
    let Some(port) = port else {
        return Err("http.proxy requires a host and explicit port".into());
    };
    if host.is_empty() {
        return Err("http.proxy requires a host and explicit port".into());
    }
    if parts.path() != "" && parts.path() != "/"
        || parts.query().is_some()
        || parts.fragment().is_some()
    {
        return Err("http.proxy URL cannot have a path, query, or fragment".into());
    }
    let username = parts.username();
    let password = parts.password();
    let authenticated = if !username.is_empty() || password.is_some() {
        if !from_env {
            return Err("http.proxy inline authentication is forbidden; use env:VARIABLE".into());
        }
        if username.is_empty() || password.unwrap_or_default().is_empty() {
            return Err("http.proxy authentication needs both username and password".into());
        }
        let joined = format!("{}{}", username, password.unwrap_or_default());
        let bad_pct = joined
            .as_bytes()
            .windows(1)
            .enumerate()
            .filter(|(_, w)| w[0] == b'%')
            .any(|(i, _)| {
                let tail = &joined.as_bytes()[i + 1..];
                tail.len() < 2 || !tail[0].is_ascii_hexdigit() || !tail[1].is_ascii_hexdigit()
            });
        if bad_pct {
            return Err("http.proxy authentication has invalid percent encoding".into());
        }
        let user_dec = urlencoding_decode(username);
        let pass_dec = urlencoding_decode(password.unwrap_or_default());
        if user_dec
            .chars()
            .chain(pass_dec.chars())
            .any(|c| (c as u32) < 33 || (c as u32) == 127)
        {
            return Err("http.proxy authentication contains control characters".into());
        }
        if user_dec.contains(':') {
            return Err("http.proxy username cannot contain a colon".into());
        }
        true
    } else {
        false
    };
    // DNS pin: resolve the proxy host the way the OS resolver does; any failure
    // surfaces as the same ValueError.
    let records = format!("{host}:{port}")
        .to_socket_addrs()
        .map_err(|_| "http.proxy host could not be resolved safely".to_string())?;
    let addresses: Vec<_> = records.collect();
    if addresses.is_empty() {
        return Err("http.proxy host could not be resolved safely".into());
    }
    let allow_private = matches!(allow_private, Some(Value::Bool(true)));
    if !allow_private
        && addresses
            .iter()
            .any(|a| !crate::py::is_public_address(&a.ip().to_string()))
    {
        return Err(
            "http.proxy resolves to a private address; set http.proxy_allow_private=true".into(),
        );
    }
    Ok(Some((
        format!("http://{}:{port}", host.to_lowercase()),
        authenticated,
    )))
}

/// Percent-decode a proxy credential part.
fn urlencoding_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() + 1 {
            if let Ok(hex) = u8::from_str_radix(
                std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or(""),
                16,
            ) {
                out.push(hex);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Build the crawl settings manifest.
pub fn manifest(config: &Value) -> Result<Value, String> {
    let Some(obj) = config.as_object() else {
        return Err("settings projection failed".into());
    };
    let mut flat = BTreeMap::new();
    flatten(obj, "", &mut flat);
    let mut out = Map::new();
    for path in RESULTS_AFFECTING.iter() {
        let Some(value) = flat.get(*path) else {
            continue;
        };
        let value = match *path {
            "http.headers" => redact_sensitive_headers(value),
            "http.credential_headers" => redact_credential_headers(value),
            "http.auth" => redact_http_auth(value),
            "http.proxy" => {
                let http = config.get("http").and_then(Value::as_object);
                let identity = http
                    .and_then(|h| h.get("proxy_identity"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let authenticated = http
                    .and_then(|h| h.get("proxy_authenticated"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let (identity, authenticated) = if identity.is_empty() && value.is_truthy_proxy() {
                    match resolve_proxy_identity(obj)? {
                        Some((ident, auth)) => (ident, auth),
                        None => (String::new(), false),
                    }
                } else {
                    (identity, authenticated)
                };
                json!({
                    "mode": if identity.is_empty() { "direct" } else { "proxy" },
                    "endpoint": if identity.is_empty() { Value::Null } else { json!(identity) },
                    "authenticated": authenticated,
                })
            }
            _ => value.clone(),
        };
        out.insert((*path).to_owned(), value);
    }
    Ok(Value::Object(out))
}

trait TruthyProxy {
    fn is_truthy_proxy(&self) -> bool;
}

impl TruthyProxy for Value {
    /// Truthiness of the proxy value: a non-empty string counts as set.
    fn is_truthy_proxy(&self) -> bool {
        crate::py::truthy(Some(self))
    }
}

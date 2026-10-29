//! Certificate Transparency (crt.sh): subdomain discovery from public TLS logs.
//!
//! Free, keyless and not SLA-backed: a flaky
//! response is `ok: false`, never "zero subdomains". `name_value` may hold several SANs on
//! separate lines; `*.example.com` folds back to its base domain.

use crate::http::{Request, Transport, TransportError};
use crate::{pyerr, pystr};
use serde_json::{json, Value};
use spiderhead_core::user_agents::RECON_USER_AGENT;
use std::collections::BTreeSet;

/// Base URL of the crt.sh certificate search service.
pub const HOST: &str = "https://crt.sh/";
/// Per-request timeout in seconds.
pub const TIMEOUT: f64 = 30.0;

/// Errors that `subdomains` lets propagate: invalid input, and a body that is not UTF-8.
/// (the decode happens inside the fetch seam, outside the catch).
#[derive(Debug)]
pub struct CrtshError {
    /// Error class name reported to callers.
    pub class: &'static str,
    /// Error message.
    pub message: String,
}

impl CrtshError {
    /// Error class name reported to callers.
    pub fn python_class(&self) -> &'static str {
        self.class
    }
}

impl std::fmt::Display for CrtshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CrtshError {}

fn raise<T>(class: &'static str, message: impl Into<String>) -> Result<T, CrtshError> {
    Err(CrtshError {
        class,
        message: message.into(),
    })
}

/// Message shape for a body that is not valid UTF-8, e.g. `'utf-8' codec can't decode ...`.
fn decode_error(body: &[u8], err: std::str::Utf8Error) -> CrtshError {
    let position = err.valid_up_to();
    let byte = body[position];
    let reason = if err.error_len().is_some() {
        "invalid start byte"
    } else {
        "unexpected end of data"
    };
    CrtshError {
        class: "UnicodeDecodeError",
        message: format!(
            "'utf-8' codec can't decode byte 0x{byte:02x} in position {position}: {reason}"
        ),
    }
}

/// Return every distinct hostname under `domain` named in a logged certificate
/// `domain` is stripped and lowercased before use.
pub async fn subdomains<T: Transport>(transport: &T, domain: &str) -> Result<Value, CrtshError> {
    if domain.is_empty() {
        return raise("ValueError", "domain required");
    }
    let domain = pystr::strip(domain).to_lowercase();
    let query = crate::http::urlencode(&[
        ("q".to_owned(), format!("%.{domain}")),
        ("output".to_owned(), "json".to_owned()),
    ]);
    let request = Request::following("GET", format!("{HOST}?{query}"))
        .header("User-Agent", RECON_USER_AGENT)
        .timeout(TIMEOUT);
    let raw = match transport.send(&request).await {
        Ok(response) => match std::str::from_utf8(&response.body) {
            Ok(text) => text.to_owned(),
            Err(e) => return Err(decode_error(&response.body, e)),
        },
        Err(e) => {
            let text = match &e {
                TransportError::Http(r) => pyerr::http_error(r.status),
                TransportError::Connect(kind) => pyerr::url_error(kind),
                TransportError::InvalidRequest(_) => pyerr::url_error("invalid request"),
            };
            return Ok(json!({
                "ok": false,
                "domain": domain,
                "error": format!("crt.sh request failed: {text}"),
            }));
        }
    };
    if pystr::strip(&raw).is_empty() {
        return Ok(json!({"ok": true, "domain": domain, "count": 0, "subdomains": []}));
    }
    let rows: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            // crt.sh serves an HTML error/maintenance page under load instead of its JSON API.
            return Ok(json!({
                "ok": false,
                "domain": domain,
                "error": "crt.sh returned a response that is not JSON (the service may be overloaded)",
            }));
        }
    };
    let Value::Array(rows) = rows else {
        return Ok(json!({
            "ok": false,
            "domain": domain,
            "error": "crt.sh returned a JSON body that is not an array",
        }));
    };
    if rows.is_empty() {
        return Ok(json!({"ok": true, "domain": domain, "count": 0, "subdomains": []}));
    }
    let mut names = BTreeSet::new();
    let suffix = format!(".{domain}");
    for row in &rows {
        let Some(map) = row.as_object() else {
            return Ok(malformed(&domain));
        };
        if !map.contains_key("common_name") && !map.contains_key("name_value") {
            return Ok(malformed(&domain));
        }
        for field in ["common_name", "name_value"] {
            let value = map.get(field).cloned().unwrap_or(Value::Null);
            // Missing or falsy fields become the empty string.
            let text = if pystr::truthy(Some(&value)) {
                pystr::repr_value(&value)
            } else {
                String::new()
            };
            for candidate in pystr::splitlines(&text) {
                let cleaned = pystr::strip(&candidate).to_lowercase();
                let name = cleaned.strip_prefix("*.").unwrap_or(&cleaned);
                if !name.is_empty() && (name == domain || name.ends_with(&suffix)) {
                    names.insert(name.to_owned());
                }
            }
        }
    }
    let subdomains: Vec<String> = names.into_iter().collect();
    Ok(json!({
        "ok": true,
        "domain": domain,
        "count": subdomains.len(),
        "subdomains": subdomains,
    }))
}

fn malformed(domain: &str) -> Value {
    json!({
        "ok": false,
        "domain": domain,
        "error": "crt.sh response contains a malformed row",
    })
}

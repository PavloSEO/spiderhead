//! IndexNow: push changed URLs to the participating search engines in one call.
//!
//! Google has not joined IndexNow (as of 2026); the `not_adopted_by` field says so wherever the
//! result travels. The key is a self-generated, site-hosted token, not a provider secret.

use std::path::Path;

use serde_json::{json, Value};
use spiderhead_core::host::Host;
use spiderhead_parse::py_url;
use thiserror::Error;

use crate::credentials;
use crate::http::{Request, Transport, TransportError};
use crate::{pyerr, pyjson, pystr};

/// Public IndexNow endpoint.
pub const HOST: &str = "https://api.indexnow.org/indexnow";
/// Per-request timeout in seconds.
pub const TIMEOUT: f64 = 30.0;
/// Largest number of URLs accepted in one submission.
pub const MAX_URLS_PER_BATCH: usize = 10_000;
/// `NOT_ADOPTED_BY` — engines that ignore the push, stated so callers do not assume coverage.
pub const NOT_ADOPTED_BY: &[&str] = &["Google"];

/// Errors raised before the request is built.
#[derive(Debug, Error)]
#[error("{message}")]
pub struct IndexnowError {
    /// Stable error class name reported to callers.
    class: &'static str,
    /// Error message text.
    message: String,
}

impl IndexnowError {
    /// Error class name of the matching failure.
    pub fn python_class(&self) -> &'static str {
        self.class
    }
}

fn raise<T>(class: &'static str, message: &str) -> Result<T, IndexnowError> {
    Err(IndexnowError {
        class,
        message: message.to_owned(),
    })
}

fn status_message(status: u16) -> Option<&'static str> {
    Some(match status {
        400 => "invalid request: check the URL list and host",
        403 => "key not valid: the key or its published key-location file could not be verified",
        422 => "one or more URLs do not belong to the submitted host or do not match the key",
        429 => "too many requests: IndexNow is rate limiting this key",
        _ => return None,
    })
}

/// Hostname check: IDNA 2008 encoding, so `faß.de` stays distinct from `fass.de`.
/// A character that cannot be encoded, or that a URL would percent-encode, fails the label check,
/// so an encoding failure and a percent-encoded name reject with the same error.
fn hostname(value: &str) -> Result<String, IndexnowError> {
    if value.chars().any(|c| (c as u32) < 0x20 || c == ':') {
        return raise("InvalidURL", "invalid host");
    }
    let ascii = idna::domain_to_ascii(&value.to_lowercase()).map_err(|_| IndexnowError {
        class: "InvalidURL",
        message: "invalid host".to_owned(),
    })?;
    let name = ascii.strip_suffix('.').unwrap_or(&ascii);
    let label_ok = |label: &str| {
        let ok = (1..=63).contains(&label.len())
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-');
        ok
    };
    if name.len() > 253 || !name.split('.').all(label_ok) {
        return raise("ValueError", "invalid hostname");
    }
    Ok(name.to_owned())
}

/// Host and port parts of a split URL.
/// `port` is `Err(())` when the port is not a valid number.
struct Netloc<'a> {
    username: Option<&'a str>,
    password: Option<&'a str>,
    hostname: Option<String>,
    port: Result<Option<u16>, ()>,
}

fn netloc_parts(netloc: &str) -> Netloc<'_> {
    let hostinfo = netloc.rsplit('@').next().unwrap_or("");
    let userinfo = netloc.rfind('@').map(|i| &netloc[..i]);
    let (username, password) = match userinfo {
        Some(info) => match info.split_once(':') {
            Some((u, p)) => (Some(u), Some(p)),
            None => (Some(info), None),
        },
        None => (None, None),
    };
    let (host, port_part) = if hostinfo.contains('[') {
        match hostinfo
            .split_once('[')
            .and_then(|(_, rest)| rest.split_once(']'))
        {
            Some((h, after)) => (
                h,
                after
                    .strip_prefix(':')
                    .filter(|p| !p.is_empty())
                    .map(str::to_owned),
            ),
            None => ("", None),
        }
    } else {
        match hostinfo.split_once(':') {
            Some((h, p)) => (
                h,
                if p.is_empty() {
                    None
                } else {
                    Some(p.to_owned())
                },
            ),
            None => (hostinfo, None),
        }
    };
    let hostname = if host.is_empty() {
        None
    } else {
        Some(host.to_lowercase())
    };
    let port = match port_part {
        None => Ok(None),
        Some(p) => {
            if p.bytes().all(|b| b.is_ascii_digit()) {
                match p.parse::<u32>() {
                    Ok(v) if v <= 65535 => Ok(Some(v as u16)),
                    _ => Err(()),
                }
            } else {
                Err(())
            }
        }
    };
    Netloc {
        username,
        password,
        hostname,
        port,
    }
}

fn bad_url(url: &str, host: &str) -> bool {
    if url
        .chars()
        .any(|c| pystr::isspace(c) || (c as u32) < 32 || (127..160).contains(&(c as u32)))
    {
        return true;
    }
    let parsed = match py_url::urlsplit(url) {
        Ok(p) => p,
        Err(_) => return true,
    };
    if parsed.scheme != "http" && parsed.scheme != "https" {
        return true;
    }
    if url.contains('\\') {
        return true;
    }
    if bad_percent(url) {
        return true;
    }
    let parts = netloc_parts(&parsed.netloc);
    if parts.hostname.is_none() || parts.username.is_some() || parts.password.is_some() {
        return true;
    }
    if parsed.netloc.ends_with(':') {
        return true;
    }
    match parts.port {
        Err(()) => return true,
        Ok(Some(0)) => return true,
        _ => {}
    }
    match parts.hostname.as_deref().map(hostname) {
        Some(Ok(h)) => h != host,
        _ => true,
    }
}

fn bad_percent(url: &str) -> bool {
    // Reject a `%` that is not followed by two hex digits.
    let bytes = url.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'%' {
            let ok = i + 2 < bytes.len()
                && bytes[i + 1].is_ascii_hexdigit()
                && bytes[i + 2].is_ascii_hexdigit();
            if !ok {
                return true;
            }
        }
    }
    false
}

/// `submit(urls, host=...)`: one POST to the IndexNow endpoint; a 200/202 acknowledges receipt,
/// never indexing.
pub async fn submit<T: Transport>(
    transport: &T,
    host: &dyn Host,
    root: &Path,
    urls: &[String],
    host_arg: &str,
    key: Option<&str>,
    key_location: Option<&str>,
) -> Result<Value, IndexnowError> {
    if urls.is_empty() {
        return raise("ValueError", "urls required");
    }
    if host_arg.is_empty() {
        return raise("ValueError", "host required");
    }
    if urls.len() > MAX_URLS_PER_BATCH {
        return Ok(json!({
            "ok": false,
            "error": format!(
                "IndexNow accepts at most {MAX_URLS_PER_BATCH} URLs per batch; got {}",
                urls.len()
            ),
        }));
    }
    let host_name = match hostname(host_arg) {
        Ok(h) => h,
        Err(_) => {
            return Ok(json!({
                "ok": false,
                "error": "host must be a bare hostname without a port or path",
            }));
        }
    };
    for (position, url) in urls.iter().enumerate() {
        if bad_url(url, &host_name) {
            return Ok(json!({
                "ok": false,
                "error": format!(
                    "URL {} must be an absolute HTTP(S) URL belonging to host",
                    position + 1
                ),
            }));
        }
    }
    let submission_key = match key.filter(|k| !k.is_empty()) {
        Some(k) => k.to_owned(),
        None => match credentials::indexnow_key(host, root) {
            Ok(k) => k,
            Err(e) => {
                if e.python_class() == "MissingCredential" {
                    return Ok(json!({"ok": false, "error": e.to_string()}));
                }
                return Err(IndexnowError {
                    class: e.python_class(),
                    message: e.to_string(),
                });
            }
        },
    };
    let mut payload = vec![
        ("host".to_owned(), Value::String(host_name)),
        ("key".to_owned(), Value::String(submission_key)),
        (
            "urlList".to_owned(),
            Value::Array(urls.iter().map(|u| json!(u)).collect()),
        ),
    ];
    if let Some(loc) = key_location {
        payload.push(("keyLocation".to_owned(), json!(loc)));
    }
    let body = pyjson::dumps(&Value::Object(payload.into_iter().collect()));
    let request = Request::new("POST", HOST)
        .header("Content-Type", "application/json; charset=utf-8")
        .body(body.as_bytes().to_vec())
        .timeout(TIMEOUT);
    let (status, body_text) = match transport.send(&request).await {
        Ok(response) => match std::str::from_utf8(&response.body) {
            Ok(text) => (response.status, text.to_owned()),
            Err(err) => {
                let position = err.valid_up_to();
                let byte = response.body.get(position).copied().unwrap_or_default();
                let reason = if err.error_len().is_some() {
                    "invalid start byte"
                } else {
                    "unexpected end of data"
                };
                return raise(
                    "UnicodeDecodeError",
                    &format!(
                        "'utf-8' codec can't decode byte 0x{byte:02x} in position {position}: {reason}"
                    ),
                );
            }
        },
        Err(TransportError::Http(response)) => (
            response.status,
            String::from_utf8_lossy(&response.body).into_owned(),
        ),
        Err(TransportError::Connect(kind)) => {
            return Ok(json!({
                "ok": false,
                "error": format!("IndexNow submission failed: {}", pyerr::url_error(&kind)),
            }));
        }
        Err(TransportError::InvalidRequest(message)) => {
            return Ok(json!({
                "ok": false,
                "error": format!("IndexNow submission failed: {message}"),
            }));
        }
    };
    let ok = status == 200 || status == 202;
    let mut result = json!({
        "ok": ok,
        "status": status,
        "submitted": urls.len(),
        "not_adopted_by": NOT_ADOPTED_BY,
    });
    if ok {
        result["message"] = json!(if status == 202 {
            "URLs received; key validation pending. Receipt does not guarantee indexing."
        } else {
            "URLs received. Receipt does not guarantee indexing."
        });
    } else {
        let clipped: String = body_text.chars().take(300).collect();
        result["error"] = json!(status_message(status)
            .map(str::to_owned)
            .or(if clipped.is_empty() {
                Some(format!("HTTP {status}"))
            } else {
                Some(clipped)
            })
            .unwrap_or_default());
    }
    Ok(result)
}

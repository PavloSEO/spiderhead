//! Wayback Machine CDX API: when a URL changed, and what it looked like before.
//!
//! No key and no account: one request per call, since the public endpoint self-throttles.
//! A URL with no snapshot is `ok` with an empty list.

use crate::http::{Request, Transport, TransportError};
use crate::{pyerr, pystr};
use serde_json::{json, Map, Value};
use spiderhead_core::user_agents::RECON_USER_AGENT;

/// CDX API endpoint.
pub const HOST: &str = "https://web.archive.org/cdx/search/cdx";
/// Per-request timeout in seconds.
pub const TIMEOUT: f64 = 20.0;

const REQUIRED_HEADER_FIELDS: [&str; 2] = ["timestamp", "original"];

/// Errors that propagate out of `history`: invalid input, and a type or overflow error when
/// `limit` cannot be read as an integer.
#[derive(Debug)]
pub struct WaybackError {
    /// Stable error class name reported to callers.
    pub class: &'static str,
    /// Error message text.
    pub message: String,
}

impl WaybackError {
    /// Error class name.
    pub fn python_class(&self) -> &'static str {
        self.class
    }
}

impl std::fmt::Display for WaybackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for WaybackError {}

fn raise<T>(class: &'static str, message: impl Into<String>) -> Result<T, WaybackError> {
    Err(WaybackError {
        class,
        message: message.into(),
    })
}

/// Return every recorded snapshot of `url`, oldest first.
///
/// `limit`/`from_date`/`to_date` accept CDX-compatible values; `limit` is read as an integer, and an
/// unreadable value is an invalid-input or type error. A fetch failure is an `ok: false` envelope, not an exception.
pub async fn history<T: Transport>(
    transport: &T,
    url: &str,
    limit: Option<&Value>,
    from_date: Option<&str>,
    to_date: Option<&str>,
) -> Result<Value, WaybackError> {
    if url.is_empty() {
        return raise("ValueError", "url required");
    }
    let mut params: Vec<(String, String)> = vec![
        ("url".to_owned(), url.to_owned()),
        ("output".to_owned(), "json".to_owned()),
    ];
    if let Some(limit) = limit.filter(|v| pystr::truthy(Some(v))) {
        let n = pystr::to_python_int(limit)
            .map_err(|(class, message)| WaybackError { class, message })?;
        params.push(("limit".to_owned(), n.to_string()));
    }
    if let Some(from) = from_date.filter(|v| !v.is_empty()) {
        params.push(("from".to_owned(), from.to_owned()));
    }
    if let Some(to) = to_date.filter(|v| !v.is_empty()) {
        params.push(("to".to_owned(), to.to_owned()));
    }
    let endpoint = format!("{HOST}?{}", crate::http::urlencode(&params));
    let request = Request::following("GET", &endpoint)
        .header("User-Agent", RECON_USER_AGENT)
        .timeout(TIMEOUT);
    let raw = match transport.send(&request).await {
        Ok(response) => match std::str::from_utf8(&response.body) {
            Ok(text) => text.to_owned(),
            Err(err) => {
                let position = err.valid_up_to();
                let byte = response.body[position];
                let reason = if err.error_len().is_some() {
                    "invalid start byte"
                } else {
                    "unexpected end of data"
                };
                return raise(
                    "UnicodeDecodeError",
                    format!(
                        "'utf-8' codec can't decode byte 0x{byte:02x} in position {position}: {reason}"
                    ),
                );
            }
        },
        Err(TransportError::Http(r)) => {
            return Ok(json!({
                "ok": false,
                "url": url,
                "error": format!("Wayback CDX request failed: {}", pyerr::http_error(r.status)),
            }));
        }
        Err(TransportError::Connect(kind)) => {
            return Ok(json!({
                "ok": false,
                "url": url,
                "error": format!("Wayback CDX request failed: {}", pyerr::url_error(&kind)),
            }));
        }
        Err(TransportError::InvalidRequest(_)) => {
            return Ok(json!({
                "ok": false,
                "url": url,
                "error": format!("Wayback CDX request failed: {}", pyerr::url_error("invalid request")),
            }));
        }
    };
    if pystr::strip(&raw).is_empty() {
        // The CDX server returns a fully empty body when nothing matches.
        return Ok(json!({"ok": true, "url": url, "count": 0, "snapshots": []}));
    }
    let rows: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            return Ok(json!({
                "ok": false,
                "url": url,
                "error": "Wayback CDX returned a response that is not JSON",
            }));
        }
    };
    let Value::Array(rows) = rows else {
        return Ok(json!({
            "ok": false,
            "url": url,
            "error": "Wayback CDX returned a JSON body that is not an array",
        }));
    };
    if rows.is_empty() {
        return Ok(json!({"ok": true, "url": url, "count": 0, "snapshots": []}));
    }
    let (header, data_rows) = rows.split_first().unwrap_or((&Value::Null, &[]));
    let header_ok = header
        .as_array()
        .map(|h| {
            h.iter().all(Value::is_string)
                && REQUIRED_HEADER_FIELDS
                    .iter()
                    .all(|f| h.iter().any(|v| v.as_str() == Some(f)))
        })
        .unwrap_or(false);
    if !header_ok {
        return Ok(json!({
            "ok": false,
            "url": url,
            "error": "Wayback CDX response header row has an unexpected shape",
        }));
    }
    let header: Vec<String> = header
        .as_array()
        .map(|h| {
            h.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let mut snapshots = Vec::new();
    for row in data_rows {
        let Some(fields) = row.as_array() else {
            return Ok(json!({
                "ok": false,
                "url": url,
                "error": "Wayback CDX response contains a malformed row",
            }));
        };
        // Pair header names with row values; stops at the shorter side, and a repeated name keeps the last value.
        let mut record = Map::new();
        for (index, field) in header.iter().enumerate() {
            if let Some(value) = fields.get(index) {
                record.insert(field.clone(), value.clone());
            }
        }
        let original = record
            .get("original")
            .map(pystr::repr_value)
            .unwrap_or_else(|| url.to_owned());
        let timestamp = record
            .get("timestamp")
            .map(pystr::repr_value)
            .unwrap_or_default();
        record.insert(
            "archived_url".to_owned(),
            Value::String(format!(
                "https://web.archive.org/web/{timestamp}/{original}"
            )),
        );
        snapshots.push(Value::Object(record));
    }
    Ok(json!({
        "ok": true,
        "url": url,
        "count": snapshots.len(),
        "snapshots": snapshots,
    }))
}

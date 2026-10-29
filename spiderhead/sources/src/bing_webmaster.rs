//! Read-only Bing Webmaster JSON REST adapter.
//!
//! Only the fixed HTTPS `/webmaster/api.svc/json` resource is used; SOAP and POX are not implemented.
//! Every operation is a bounded GET; transport, HTTP and credential failures are
//! returned as result envelopes, never raised.

use crate::credentials::{self, CredentialError};
use crate::http::{Request, Transport, TransportError};
use crate::pystr;
use serde_json::{json, Map, Value};
use spiderhead_core::host::Host;
use std::path::Path;

/// Base URL of the Bing Webmaster JSON REST API.
pub const HOST: &str = "https://ssl.bing.com/webmaster/api.svc/json";
/// Per-request timeout in seconds.
pub const TIMEOUT: f64 = 30.0;

const METHODS: [(&str, &str); 6] = [
    ("sites", "GetUserSites"),
    ("crawl", "GetCrawlIssues"),
    ("links", "GetLinkCounts"),
    ("url_links", "GetUrlLinks"),
    ("keywords", "GetQueryStats"),
    ("search_performance", "GetQueryStats"),
];

/// Input validation failures, raised before any request. Transport and credential failures
/// are envelopes, so they never appear here.
#[derive(Debug, thiserror::Error)]
pub enum BingWebmasterError {
    /// Input rejected before credentials or any request.
    #[error("{0}")]
    Invalid(String),
    /// Other credential store failures propagate, with their own error kind.
    /// A missing credential becomes a `not_configured` envelope instead.
    #[error("{0}")]
    Credential(#[from] CredentialError),
}

impl BingWebmasterError {
    /// Error class name reported to callers.
    pub fn python_class(&self) -> &'static str {
        match self {
            BingWebmasterError::Invalid(_) => "ValueError",
            BingWebmasterError::Credential(e) => e.python_class(),
        }
    }
}

fn fail(error: &str) -> Value {
    json!({"ok": false, "state": "failed", "error": error})
}

/// Options for `collect`, after the operation name.
#[derive(Debug, Default)]
pub struct Call<'a> {
    /// `site_url` — required for every operation except `sites`.
    pub site_url: &'a str,
    /// Page number as a JSON value, so non-integer input is rejected; None means 0.
    pub page: Option<&'a Value>,
    /// `link` — required for `url_links`.
    pub link: Option<&'a str>,
    /// `api_key` overrides the stored credential when non-empty.
    pub api_key: Option<&'a str>,
}

/// Read site, crawl, link, keyword or search-performance evidence over the JSON REST API.
///
/// Missing credentials yield a `not_configured` envelope; HTTP and transport failures yield
/// `failed` envelopes that never echo the apikey query parameter.
pub async fn collect<T: Transport>(
    host: &dyn Host,
    root: &Path,
    transport: &T,
    operation: &str,
    call: Call<'_>,
) -> Result<Value, BingWebmasterError> {
    let Call {
        site_url,
        page,
        link,
        api_key,
    } = call;
    let Some(method) = METHODS
        .iter()
        .find(|(name, _)| *name == operation)
        .map(|(_, m)| *m)
    else {
        return Err(BingWebmasterError::Invalid(
            "unsupported Bing Webmaster JSON operation".to_owned(),
        ));
    };
    if operation != "sites" && site_url.is_empty() {
        return Err(BingWebmasterError::Invalid(
            "site_url is required for this Bing Webmaster operation".to_owned(),
        ));
    }
    let page_value = page.cloned().unwrap_or_else(|| json!(0));
    let page_i = pystr::as_strict_i64(Some(&page_value));
    if page_i.map(|p| p >= 0) != Some(true) {
        return Err(BingWebmasterError::Invalid(
            "page must be a non-negative integer".to_owned(),
        ));
    }
    let page_i = page_i.unwrap_or_default();
    if operation == "url_links" && (link.map(str::is_empty) != Some(false) || page_i > 32767) {
        return Err(BingWebmasterError::Invalid(
            "url_links requires link and page in 0..32767".to_owned(),
        ));
    }
    let key = match api_key.filter(|k| !k.is_empty()) {
        Some(k) => k.to_owned(),
        None => match credentials::bing_webmaster_key(host, root) {
            Ok(k) => k,
            Err(CredentialError::Missing(message)) => {
                return Ok(json!({
                    "ok": false,
                    "state": "not_configured",
                    "verified": false,
                    "error": message,
                }));
            }
            Err(e) => return Err(e.into()),
        },
    };
    let mut params: Vec<(String, String)> = vec![("apikey".to_owned(), key)];
    if !site_url.is_empty() {
        params.push(("siteUrl".to_owned(), site_url.to_owned()));
    }
    if operation == "links" || operation == "url_links" {
        params.push(("page".to_owned(), page_i.to_string()));
    }
    if operation == "url_links" {
        params.push((
            "link".to_owned(),
            crate::pyjson::dumps(&Value::String(link.unwrap_or_default().to_owned())),
        ));
    }
    let url = format!("{HOST}/{method}?{}", crate::http::urlencode(&params));
    let request = Request::get(&url).timeout(TIMEOUT);
    let text = match transport.send(&request).await {
        Ok(response) => match String::from_utf8(response.body) {
            Ok(text) => text,
            Err(_) => return Ok(fail("Bing Webmaster request failed")),
        },
        Err(TransportError::Http(response)) => {
            return Ok(json!({
                "ok": false,
                "state": "failed",
                "status": response.status,
                "error": format!("Bing Webmaster HTTP {}", response.status),
            }));
        }
        Err(_) => return Ok(fail("Bing Webmaster request failed")),
    };
    let body: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return Ok(fail("Bing Webmaster request failed")),
    };
    let data = body.as_object().and_then(|m| m.get("d")).cloned();
    let paged = operation == "links" || operation == "url_links";
    let list_key = if operation == "url_links" {
        "Details"
    } else {
        "Links"
    };
    let rows = if paged && matches!(data, Some(Value::Object(_))) {
        data.as_ref()
            .and_then(Value::as_object)
            .and_then(|m| m.get(list_key))
            .cloned()
            .unwrap_or(Value::Null)
    } else {
        data.clone().unwrap_or(Value::Null)
    };
    let valid_rows = matches!(&rows, Value::Array(items) if items.iter().all(Value::is_object));
    if !valid_rows {
        return Ok(fail("malformed Bing Webmaster JSON response"));
    }
    let mut page_out = Map::new();
    if paged {
        let total_pages = data
            .as_ref()
            .and_then(Value::as_object)
            .and_then(|m| m.get("TotalPages"))
            .and_then(|v| pystr::as_strict_i64(Some(v)));
        let malformed = match total_pages {
            Some(total) => {
                total < 0
                    || (total == 0
                        && (!rows.as_array().map(|r| r.is_empty()).unwrap_or(true) || page_i != 0))
                    || (total > 0 && page_i >= total)
            }
            None => true,
        };
        if malformed {
            return Ok(fail("malformed Bing Webmaster pagination"));
        }
        let total = total_pages.unwrap_or_default();
        page_out.insert("page".to_owned(), json!(page_i));
        page_out.insert("total_pages".to_owned(), json!(total));
        page_out.insert(
            "next_page".to_owned(),
            if page_i + 1 < total {
                json!(page_i + 1)
            } else {
                Value::Null
            },
        );
    }
    let rows_len = rows.as_array().map(|r| r.len()).unwrap_or_default();
    let partial = paged
        && page_out
            .get("total_pages")
            .and_then(Value::as_i64)
            .map(|t| t > 1)
            .unwrap_or(false);
    let mut out = Map::new();
    out.insert("ok".to_owned(), Value::Bool(true));
    out.insert(
        "state".to_owned(),
        Value::String(if partial { "partial" } else { "complete" }.to_owned()),
    );
    out.insert("operation".to_owned(), Value::String(operation.to_owned()));
    out.insert("data".to_owned(), data.unwrap_or(Value::Null));
    out.insert("rows".to_owned(), rows);
    out.insert("returned".to_owned(), json!(rows_len));
    out.insert("truncated".to_owned(), Value::Bool(partial));
    for (k, v) in page_out {
        out.insert(k, v);
    }
    out.insert("read_only".to_owned(), Value::Bool(true));
    Ok(Value::Object(out))
}

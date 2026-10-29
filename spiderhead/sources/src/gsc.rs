//! Google Search Console client: what Google observed for a property, not what a crawl inferred.
//!
//! Search Console says whether Google indexed a URL and how it ranked. Two request shapes exist:
//! Search Analytics (`searchAnalytics/query`, paginated) and URL Inspection
//! (`urlInspection/index:inspect`), plus property listing and sitemap status.
//!
//! Dates are `YYYY-MM-DD` calendar dates in Pacific Time. The default range is the last *completed*
//! 28-day window ending yesterday in Pacific Time. The legacy labels `28daysAgo`/`today` resolve to
//! the same window.

use serde_json::{json, Map, Value};
use spiderhead_core::host::Host;
use std::path::Path;

use crate::credentials::{self, CredentialError};
use crate::http::{quote, Request, Transport, TransportError};
use crate::oauth::{self, OAuthError};
use crate::{pyerr, pyjson, pystr, pytime};

mod inspect;
mod pages;
mod service_account;

pub use inspect::inspect_urls;
pub use pages::{search_analytics_page, search_analytics_pages, PageArgs, PagesArgs};
pub use service_account::service_account_access_token;

mod transport;
pub(crate) use transport::*;
mod service_account_auth;
pub use service_account_auth::*;

/// `SEARCH_ANALYTICS_HOST` — the webmasters/v3 API root.
pub const SEARCH_ANALYTICS_HOST: &str = "https://www.googleapis.com/webmasters/v3";
/// `INSPECTION_HOST` — the URL Inspection API root.
pub const INSPECTION_HOST: &str = "https://searchconsole.googleapis.com/v1";
/// `TIMEOUT` — whole-request seconds for every Search Console call.
pub const TIMEOUT: f64 = 30.0;
/// `DEFAULT_WINDOW_DAYS` — the completed-window length the default range resolves to.
pub const DEFAULT_WINDOW_DAYS: i64 = 28;
/// The only scope this client requests and accepts.
pub const READONLY_SCOPE: &str = "https://www.googleapis.com/auth/webmasters.readonly";
/// Largest number of URLs one inspection call accepts.
pub const MAX_INSPECTION_URLS: usize = 50;
/// Longest accepted inspection URL, in UTF-8 bytes.
pub const MAX_INSPECTION_URL_BYTES: usize = 8192;
/// Row cap for one paginated Search Analytics read.
pub const MAX_ANALYTICS_ROWS: i64 = 25_000;
/// `MAX_RESPONSE_BYTES` — the local read budget for any single response (8 MiB).
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
/// `MAX_START_ROW` — the largest `startRow` the archive page entry accepts.
pub const MAX_START_ROW: i64 = 50_000;

const LEGACY_START_LABEL: &str = "28daysAgo";
const LEGACY_END_LABEL: &str = "today";
/// Dimensions the archive query accepts.
pub const ARCHIVE_DIMENSIONS: [&str; 6] = [
    "date",
    "query",
    "page",
    "country",
    "device",
    "searchAppearance",
];
/// Search types the API accepts.
pub const SEARCH_TYPES: [&str; 6] = ["web", "image", "video", "news", "discover", "googleNews"];
/// Aggregation modes the API accepts.
pub const AGGREGATION_TYPES: [&str; 3] = ["auto", "byPage", "byProperty"];

/// The errors this client raises as exceptions (vs. returning a `{"ok": false}` envelope).
#[derive(Debug, thiserror::Error)]
pub enum GscError {
    /// Invalid argument, rejected before any request.
    #[error("{0}")]
    Invalid(String),
    /// A response exceeded the local read budget.
    #[error("{0}")]
    TooLarge(String),
    /// Any other failure; the class name tells callers which kind.
    #[error("{1}")]
    Wrapped(&'static str, String),
}

impl GscError {
    /// Stable error class name reported to callers.
    pub fn python_class(&self) -> &'static str {
        match self {
            GscError::Invalid(_) => "ValueError",
            GscError::TooLarge(_) => "ResponseTooLarge",
            GscError::Wrapped(class, _) => class,
        }
    }
}

impl From<CredentialError> for GscError {
    fn from(e: CredentialError) -> Self {
        GscError::Wrapped(e.python_class(), e.to_string())
    }
}

impl From<OAuthError> for GscError {
    fn from(e: OAuthError) -> Self {
        GscError::Wrapped(e.python_class(), e.to_string())
    }
}

/// `default_date_range` — `(start, end)` of the last completed inclusive 28-day window:
/// `end` is yesterday in Pacific Time, `start` 27 days before that.
pub fn default_date_range(now_micros: i64) -> (String, String) {
    let (y, m, d) = pytime::pacific_date(now_micros);
    let end_days = pytime::days_from_civil(y, m, d) - 1;
    let start_days = end_days - (DEFAULT_WINDOW_DAYS - 1);
    (iso_days(start_days), iso_days(end_days))
}

fn iso_days(days: i64) -> String {
    let (y, m, d) = pytime::civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

fn is_iso_date(value: Option<&str>) -> bool {
    let Some(value) = value else { return false };
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    pytime::fromisoformat(value)
        .map(|p| !p.aware)
        .unwrap_or(false)
}

/// Single-quoted date text for error messages; `None` is shown bare.
fn repr_date(value: Option<&str>) -> String {
    match value {
        None => "None".to_owned(),
        Some(text) => {
            let single = !text.contains('\'') || text.contains('"');
            let quote_mark = if single { '\'' } else { '"' };
            let mut out = String::with_capacity(text.len() + 2);
            out.push(quote_mark);
            for ch in text.chars() {
                match ch {
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if c == quote_mark => {
                        out.push('\\');
                        out.push(c);
                    }
                    c => out.push(c),
                }
            }
            out.push(quote_mark);
            out
        }
    }
}

/// Resolve the date range: missing bounds or the legacy labels select the default completed window.
/// Any other value passes through for validation.
fn resolve_date_range(
    start: Option<&str>,
    end: Option<&str>,
    now: i64,
) -> (Option<String>, Option<String>) {
    if (start.is_none() && end.is_none())
        || (start == Some(LEGACY_START_LABEL) && end == Some(LEGACY_END_LABEL))
    {
        let (start, end) = default_date_range(now);
        return (Some(start), Some(end));
    }
    (start.map(str::to_owned), end.map(str::to_owned))
}

/// Error message for a malformed or reversed date range, else `None`.
pub(crate) fn validate_date_range(start: Option<&str>, end: Option<&str>) -> Option<String> {
    if !is_iso_date(start) || !is_iso_date(end) {
        return Some(format!(
            "start_date and end_date must be YYYY-MM-DD calendar dates in Pacific Time, \
             got {} and {}",
            repr_date(start),
            repr_date(end),
        ));
    }
    let (s, e) = (start.unwrap_or_default(), end.unwrap_or_default());
    if s > e {
        return Some(format!(
            "start_date must not be after end_date: {} > {}",
            repr_date(Some(s)),
            repr_date(Some(e)),
        ));
    }
    None
}

/// Search Analytics endpoint for a property; the site URL is percent-encoded with no safe characters.
pub(crate) fn analytics_endpoint(site_url: &str) -> String {
    format!(
        "{SEARCH_ANALYTICS_HOST}/sites/{}/searchAnalytics/query",
        quote(site_url, "")
    )
}

/// `search_analytics` — rows a verified property earned in search, single bounded page.
pub async fn search_analytics<T: Transport>(
    host: &dyn Host,
    root: &Path,
    site_url: &str,
    args: &Analytics<'_>,
    transport: &T,
) -> Result<Value, GscError> {
    if site_url.is_empty() {
        return Err(GscError::Invalid("site_url required".to_owned()));
    }
    let (start_date, end_date) = resolve_date_range(
        args.start_date,
        args.end_date,
        args.now.unwrap_or_else(pytime::now_micros),
    );
    if let Some(error) = validate_date_range(start_date.as_deref(), end_date.as_deref()) {
        return Ok(json!({"ok": false, "error": error}));
    }
    let (bearer, token_error) =
        acquire_token(host, root, args.token, transport, args.service_account).await?;
    let Some(bearer) = bearer else {
        return Ok(json!({"ok": false, "error": token_error}));
    };
    let payload = json!({
        "startDate": start_date,
        "endDate": end_date,
        // Any falsy dimensions value means the default `query` dimension.
        "dimensions": match args.dimensions.as_ref().filter(|d| py_truthy(d)) {
            Some(dims) => dims.clone(),
            None => json!(["query"]),
        },
        "rowLimit": args.row_limit,
    });
    let raw = match send(
        transport,
        "POST",
        &analytics_endpoint(site_url),
        Some(&payload),
        &bearer,
    )
    .await
    {
        Fetch::Body(text) => text,
        Fetch::Http(response) => {
            return Ok(json!({
                "ok": false,
                "error": api_error(&response, &bearer)?,
                "status": response.status,
            }))
        }
        Fetch::TooLarge => {
            return Ok(json!({
                "ok": false,
                "error": "Search Console response exceeded the 8 MiB limit",
                "state": "response_too_large",
            }))
        }
        Fetch::Connect(kind) => {
            return Ok(json!({
                "ok": false,
                "error": format!("Search Console request failed: {}", transport_text(&kind)),
            }))
        }
        Fetch::Decode(message) => {
            return Err(GscError::Wrapped("UnicodeDecodeError", message));
        }
    };
    let Some(body) = response_object(&raw) else {
        return Ok(json!({"ok": false, "error": "Search Console malformed response"}));
    };
    let rows_valid = match body.get("rows") {
        None => Vec::new(),
        Some(Value::Array(rows)) if rows.iter().all(Value::is_object) => rows.clone(),
        Some(_) => return Ok(json!({"ok": false, "error": "Search Console malformed response"})),
    };
    let rows: Vec<Value> = rows_valid
        .iter()
        .map(|row| {
            json!({
                "keys": row.get("keys").cloned().unwrap_or(Value::Null),
                "clicks": row.get("clicks").cloned().unwrap_or(Value::Null),
                "impressions": row.get("impressions").cloned().unwrap_or(Value::Null),
                "ctr": row.get("ctr").cloned().unwrap_or(Value::Null),
                "position": row.get("position").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    Ok(json!({
        "ok": true,
        "site_url": site_url,
        "period": format!("{}..{}", start_date.unwrap_or_default(), end_date.unwrap_or_default()),
        "count": rows.len(),
        "rows": rows,
    }))
}

/// The keyword arguments of `search_analytics`.
pub struct Analytics<'a> {
    /// `start_date`/`end_date` — `None`/`None` resolves the completed 28-day window.
    pub start_date: Option<&'a str>,
    /// `end_date`.
    pub end_date: Option<&'a str>,
    /// `dimensions` — any falsy value defaults to `["query"]`.
    pub dimensions: Option<Value>,
    /// `row_limit` — sent through as-is; this function performs no range check.
    pub row_limit: Value,
    /// `token` — an explicit bearer that skips credential lookup.
    pub token: Option<&'a Value>,
    /// Fixed clock for the default date window, as epoch microseconds.
    pub now: Option<i64>,
    /// Service-account substitute (see [`ServiceAccount`]).
    pub service_account: ServiceAccount<'a>,
}

/// `inspect_url` — Google's own indexing verdict for one URL.
pub async fn inspect_url<T: Transport>(
    host: &dyn Host,
    root: &Path,
    site_url: &str,
    inspection_url: &str,
    token: Option<&Value>,
    service_account: ServiceAccount<'_>,
    transport: &T,
) -> Result<Value, GscError> {
    if site_url.is_empty() || inspection_url.is_empty() {
        return Err(GscError::Invalid(
            "site_url and inspection_url required".to_owned(),
        ));
    }
    let (bearer, token_error) =
        acquire_token(host, root, token, transport, service_account).await?;
    let Some(bearer) = bearer else {
        return Ok(json!({"ok": false, "error": token_error}));
    };
    let payload = json!({"inspectionUrl": inspection_url, "siteUrl": site_url});
    let url = format!("{INSPECTION_HOST}/urlInspection/index:inspect");
    let raw = match send(transport, "POST", &url, Some(&payload), &bearer).await {
        Fetch::Body(text) => text,
        Fetch::Http(response) => {
            return Ok(json!({
                "ok": false,
                "error": api_error(&response, &bearer)?,
                "status": response.status,
            }))
        }
        Fetch::TooLarge => {
            return Ok(json!({
                "ok": false,
                "error": "Search Console response exceeded the 8 MiB limit",
                "state": "response_too_large",
            }))
        }
        Fetch::Connect(kind) => {
            return Ok(json!({
                "ok": false,
                "error": format!("Search Console request failed: {}", transport_text(&kind)),
            }))
        }
        Fetch::Decode(message) => {
            return Err(GscError::Wrapped("UnicodeDecodeError", message));
        }
    };
    let Some(body) = response_object(&raw) else {
        return Ok(json!({"ok": false, "error": "Search Console malformed response"}));
    };
    let Some(inspection) = body.get("inspectionResult").filter(|v| v.is_object()) else {
        return Ok(json!({"ok": false, "error": "Search Console malformed response"}));
    };
    // A missing or null `indexStatusResult` is treated as an empty object.
    let index_status = match inspection.get("indexStatusResult") {
        None | Some(Value::Null) => Value::Object(Map::new()),
        Some(other) => other.clone(),
    };
    let Some(index_status) = index_status.as_object() else {
        return Ok(json!({"ok": false, "error": "Search Console malformed response"}));
    };
    Ok(json!({
        "ok": true,
        "site_url": site_url,
        "inspection_url": inspection_url,
        "verdict": index_status.get("verdict").cloned().unwrap_or(Value::Null),
        "coverage_state": index_status.get("coverageState").cloned().unwrap_or(Value::Null),
        "indexing_state": index_status.get("indexingState").cloned().unwrap_or(Value::Null),
        "last_crawl_time": index_status.get("lastCrawlTime").cloned().unwrap_or(Value::Null),
        "google_canonical": index_status.get("googleCanonical").cloned().unwrap_or(Value::Null),
        "user_canonical": index_status.get("userCanonical").cloned().unwrap_or(Value::Null),
    }))
}

/// `discover_properties` — list properties the principal can access; an omitted
/// `siteEntry` means an empty list (a fresh service account), not malformed data.
pub async fn discover_properties<T: Transport>(
    host: &dyn Host,
    root: &Path,
    token: Option<&Value>,
    service_account: ServiceAccount<'_>,
    transport: &T,
) -> Result<Value, GscError> {
    let (bearer, token_error) =
        acquire_token(host, root, token, transport, service_account).await?;
    let Some(bearer) = bearer else {
        return Ok(json!({
            "ok": false,
            "state": "not_configured",
            "verified": false,
            "error": token_error,
        }));
    };
    let body = match send(
        transport,
        "GET",
        &format!("{SEARCH_ANALYTICS_HOST}/sites"),
        None,
        &bearer,
    )
    .await
    {
        Fetch::Body(text) => response_object(&text),
        Fetch::Http(response) => {
            return Ok(json!({
                "ok": false,
                "state": "verification_failed",
                "verified": false,
                "error": api_error(&response, &bearer)?,
                "status": response.status,
            }))
        }
        Fetch::TooLarge => {
            return Ok(json!({
                "ok": false,
                "state": "verification_failed",
                "verified": false,
                "error": "Search Console response exceeded the 8 MiB limit",
            }))
        }
        Fetch::Connect(kind) => {
            return Ok(json!({
                "ok": false,
                "state": "verification_failed",
                "verified": false,
                "error": transport_text(&kind),
            }))
        }
        // Undecodable bodies are not caught here; the decode error propagates.
        Fetch::Decode(message) => return Err(GscError::Wrapped("UnicodeDecodeError", message)),
    };
    // Google omits siteEntry entirely for a principal with no properties.
    let entries = body
        .as_ref()
        .map(|b| b.get("siteEntry").cloned().unwrap_or_else(|| json!([])));
    let valid = entries
        .as_ref()
        .and_then(Value::as_array)
        .map(|list| list.iter().all(Value::is_object))
        .unwrap_or(false);
    if !valid {
        return Ok(json!({
            "ok": false,
            "state": "verification_failed",
            "verified": false,
            "error": "malformed GSC property response",
        }));
    }
    let properties: Vec<Value> = entries
        .as_ref()
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| {
            json!({
                "site_url": entry.get("siteUrl").cloned().unwrap_or(Value::Null),
                "permission_level": entry.get("permissionLevel").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    Ok(json!({
        "ok": true,
        "verified": true,
        "properties": properties,
        "scopes": [READONLY_SCOPE],
    }))
}

/// `sitemap_status` — read the selected property's submitted sitemap status.
pub async fn sitemap_status<T: Transport>(
    host: &dyn Host,
    root: &Path,
    site_url: &str,
    token: Option<&Value>,
    service_account: ServiceAccount<'_>,
    transport: &T,
) -> Result<Value, GscError> {
    if site_url.is_empty() {
        return Err(GscError::Invalid("site_url required".to_owned()));
    }
    let (bearer, token_error) =
        acquire_token(host, root, token, transport, service_account).await?;
    let Some(bearer) = bearer else {
        return Ok(json!({
            "ok": false,
            "state": "not_configured",
            "verified": false,
            "error": token_error,
        }));
    };
    let endpoint = format!(
        "{SEARCH_ANALYTICS_HOST}/sites/{}/sitemaps",
        quote(site_url, "")
    );
    let body = match send(transport, "GET", &endpoint, None, &bearer).await {
        Fetch::Body(text) => response_object(&text),
        Fetch::Http(response) => {
            return Ok(json!({
                "ok": false,
                "state": "failed",
                "error": pyerr::http_error(response.status),
            }))
        }
        Fetch::TooLarge => {
            return Ok(json!({
                "ok": false,
                "state": "failed",
                "error": "Search Console response exceeded the 8 MiB limit",
            }))
        }
        Fetch::Connect(kind) => {
            return Ok(json!({
                "ok": false,
                "state": "failed",
                "error": transport_text(&kind),
            }))
        }
        Fetch::Decode(message) => return Err(GscError::Wrapped("UnicodeDecodeError", message)),
    };
    let sitemaps = body.as_ref().and_then(|b| b.get("sitemap"));
    let valid = sitemaps
        .map(|s| {
            s.as_array()
                .map(|l| l.iter().all(Value::is_object))
                .unwrap_or(false)
        })
        .unwrap_or(false);
    if !valid {
        return Ok(json!({
            "ok": false,
            "state": "failed",
            "error": "malformed GSC sitemap response",
        }));
    }
    let sitemaps = sitemaps
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(json!({
        "ok": true,
        "state": "complete",
        "sitemaps": sitemaps,
        "returned": sitemaps.len(),
    }))
}

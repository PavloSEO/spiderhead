//! Read-only Yandex Webmaster REST adapter with injected transport support.
//!
//! The injected transport is a `(method, url, payload, token) -> body text` callable.
//! Injected and live transports both implement [`WmCall`], so tests can replay recorded requests.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use serde_json::{json, Map, Value};
use spiderhead_core::host::Host;

use crate::credentials::{yandex_webmaster_token, CredentialError};
use crate::http::{
    exponential_pause, quote, retry_slots, Request, Sleeper, Transport, TransportError,
};
use crate::{pyerr, pyjson, pystr, pytime};

mod errors;
pub use errors::*;
mod values;
use values::*;
mod collect;
pub use collect::*;
mod statistics;
pub use statistics::*;

/// Base URL of the Yandex Webmaster API v4.
pub const HOST: &str = "https://api.webmaster.yandex.net/v4";
/// Request timeout in seconds.
pub const TIMEOUT: f64 = 30.0;
/// Hard row limit for paged sample lists.
pub const MAX_ROWS: i64 = 50_000;
/// Page size for query analytics requests.
pub const QUERY_ANALYTICS_PAGE: i64 = 500;

/// Operation to API v4 path below `/user/{user_id}`; `{host}` and `{query}` are placeholders.
/// Fixed order, so the unsupported-operation error lists operations consistently.
pub const OPERATIONS: &[(&str, &str)] = &[
    ("hosts", "/hosts"),
    ("indexing", "/hosts/{host}/indexing/samples"),
    ("crawl", "/hosts/{host}/search-urls/events/samples"),
    ("sitemaps", "/hosts/{host}/sitemaps"),
    ("search_performance", "/hosts/{host}/search-queries/popular"),
    ("summary", "/hosts/{host}/summary"),
    ("diagnostics", "/hosts/{host}/diagnostics"),
    ("sqi_history", "/hosts/{host}/sqi-history"),
    ("search_history", "/hosts/{host}/search-queries/all/history"),
    (
        "query_history",
        "/hosts/{host}/search-queries/{query}/history",
    ),
    (
        "in_search_history",
        "/hosts/{host}/search-urls/in-search/history",
    ),
    ("events_history", "/hosts/{host}/search-urls/events/history"),
    ("indexing_history", "/hosts/{host}/indexing/history"),
    ("important_urls", "/hosts/{host}/important-urls"),
    (
        "broken_links_samples",
        "/hosts/{host}/links/internal/broken/samples",
    ),
    (
        "broken_links_history",
        "/hosts/{host}/links/internal/broken/history",
    ),
    (
        "external_links_history",
        "/hosts/{host}/links/external/history",
    ),
    (
        "external_links_samples",
        "/hosts/{host}/links/external/samples",
    ),
];

/// Paged sample lists: operation maps to the list key and the page-size cap.
const PAGED: &[(&str, (&str, i64))] = &[
    ("search_performance", ("queries", 500)),
    ("crawl", ("samples", 100)),
    ("indexing", ("samples", 100)),
    ("broken_links_samples", ("links", 100)),
    ("external_links_samples", ("links", 100)),
];

const QUERY_ANALYTICS_PATH: &str = "/hosts/{host}/query-analytics/list";
const QUERY_FIELDS: &[&str] = &["IMPRESSIONS", "CLICKS", "CTR", "POSITION", "DEMAND"];
const QUERY_ANALYTICS_RETRIES: usize = 3;

/// Default query parameters per operation, applied before caller parameters.
fn default_params(operation: &str) -> Option<(&'static str, &'static str)> {
    match operation {
        "search_performance" => Some(("order_by", "TOTAL_SHOWS")),
        _ => None,
    }
}

/// Async form of the `(method, url, payload, token) -> body text` transport.
/// Plain closures and this trait both work with `collect`.
pub trait WmCall {
    /// Send one request; resolves to the response body or a [`WmError`].
    fn send<'s>(
        &'s mut self,
        method: &'s str,
        url: &'s str,
        payload: Option<&'s Value>,
        token: &'s Value,
    ) -> Pin<Box<dyn Future<Output = Result<String, WmError>> + Send + 's>>;
}

/// Synchronous injected transport, used in tests.
pub type WmTransport<'a> =
    dyn Fn(&str, &str, Option<&Value>, &Value) -> Result<String, WmError> + Send + Sync + 'a;

/// Adapts an injected [`WmTransport`] to the [`WmCall`] trait.
pub struct SyncWmCall<'x>(pub &'x WmTransport<'x>);

impl WmCall for SyncWmCall<'_> {
    fn send<'s>(
        &'s mut self,
        method: &'s str,
        url: &'s str,
        payload: Option<&'s Value>,
        token: &'s Value,
    ) -> Pin<Box<dyn Future<Output = Result<String, WmError>> + Send + 's>> {
        let result = self.0(method, url, payload, token);
        Box::pin(std::future::ready(result))
    }
}

/// Live send over the shared HTTP client, without following redirects.
pub struct LiveWmCall<'x, T: Transport>(pub &'x T);

impl<T: Transport> WmCall for LiveWmCall<'_, T> {
    fn send<'s>(
        &'s mut self,
        method: &'s str,
        url: &'s str,
        payload: Option<&'s Value>,
        token: &'s Value,
    ) -> Pin<Box<dyn Future<Output = Result<String, WmError>> + Send + 's>> {
        Box::pin(async move {
            let mut request = Request::new(method, url)
                .header("Authorization", format!("OAuth {}", str_value(token)))
                .header("Content-Type", "application/json")
                .timeout(TIMEOUT);
            if let Some(payload) = payload {
                request = request.body(pyjson::dumps(payload).into_bytes());
            }
            match self.0.send(&request).await {
                Ok(response) => match pystr::decode_utf8_strict(&response.body) {
                    Ok(text) => Ok(text),
                    Err(message) => Err(WmError::Value(message)),
                },
                Err(TransportError::Http(response)) => Err(WmError::Http {
                    code: i64::from(response.status),
                    reason: pyerr::reason_phrase(response.status).to_owned(),
                    body: String::from_utf8_lossy(&response.body).into_owned(),
                }),
                Err(TransportError::Connect(kind)) => Err(WmError::Url(pyerr::url_error(&kind))),
                Err(TransportError::InvalidRequest(kind)) => Err(WmError::Value(kind)),
            }
        })
    }
}

/// Current date as ISO `YYYY-MM-DD`, for the rolling window of `url_queries`.
pub type Today<'a> = dyn Fn() -> String + 'a;

/// Dependencies every adapter call needs.
pub struct WmEnv<'a, T: Transport> {
    /// Credential host.
    pub host: &'a dyn Host,
    /// Credential config root.
    pub root: &'a Path,
    /// HTTP boundary for live sends.
    pub transport: &'a T,
    /// Sleep used for retry backoff in `url_queries`.
    pub sleeper: &'a Sleeper,
    /// Current date; `None` uses the host clock.
    pub today: Option<&'a Today<'a>>,
    /// Overrides the token lookup; `None` reads the credential store.
    pub token: Option<&'a dyn Fn() -> Result<String, CredentialError>>,
}

impl<'a, T: Transport> WmEnv<'a, T> {
    /// The production environment.
    pub fn live(host: &'a dyn Host, root: &'a Path, transport: &'a T) -> Self {
        WmEnv {
            host,
            root,
            transport,
            sleeper: &Sleeper::Real,
            today: None,
            token: None,
        }
    }

    /// Current date as an ISO date string.
    pub(crate) fn today(&self) -> String {
        match self.today {
            Some(today) => today(),
            None => {
                let (year, month, day) = pytime::pacific_date(pytime::now_micros());
                format!("{year:04}-{month:02}-{day:02}")
            }
        }
    }
}

/// Bounded Yandex query evidence for one URL or a capped set of URLs.
pub async fn url_queries<T: Transport>(
    host_id: &Value,
    args: &UrlQueriesArgs<'_>,
    env: &WmEnv<'_, T>,
) -> Result<Value, WmError> {
    match args.transport {
        Some(send) => {
            let mut call = SyncWmCall(send);
            url_queries_inner(host_id, args, env, &mut call).await
        }
        None => {
            let mut call = LiveWmCall(env.transport);
            url_queries_inner(host_id, args, env, &mut call).await
        }
    }
}

async fn url_queries_inner<T: Transport>(
    host_id: &Value,
    args: &UrlQueriesArgs<'_>,
    env: &WmEnv<'_, T>,
    call: &mut dyn WmCall,
) -> Result<Value, WmError> {
    let max_urls = strict_bounded(args.max_urls, 100);
    if !pystr::truthy(Some(host_id))
        || (pystr::truthy(args.url) && pystr::truthy(args.url_contains))
        || max_urls.is_none()
    {
        return Err(WmError::value_error(
            "host_id, one optional URL filter, and max_urls in 1..50000 are required",
        ));
    }
    let max_urls = max_urls.unwrap_or(100);
    let max_queries = strict_bounded(args.max_queries_per_url, QUERY_ANALYTICS_PAGE);
    let max_queries = match max_queries {
        Some(v) => v,
        None => {
            return Err(WmError::value_error(
                "max_queries_per_url must be in 1..50000",
            ))
        }
    };
    if pystr::truthy(args.start_date) != pystr::truthy(args.end_date) {
        return Err(WmError::value_error("pass both start_date and end_date"));
    }
    let mut requested_days: Option<Vec<String>> = None;
    if pystr::truthy(args.start_date) && pystr::truthy(args.end_date) {
        let (start_raw, end_raw) = (args.start_date, args.end_date);
        let start_text = match start_raw {
            Some(Value::String(text)) => text.as_str(),
            _ => return Err(WmError::type_error("fromisoformat: argument must be str")),
        };
        let end_text = match end_raw {
            Some(Value::String(text)) => text.as_str(),
            _ => return Err(WmError::type_error("fromisoformat: argument must be str")),
        };
        let start = match pytime::date_fromisoformat_err(start_text) {
            Ok(days) => days,
            Err(_) => {
                return Err(WmError::value_error(
                    "start_date and end_date must be ISO dates",
                ))
            }
        };
        let end = match pytime::date_fromisoformat_err(end_text) {
            Ok(days) => days,
            Err(_) => {
                return Err(WmError::value_error(
                    "start_date and end_date must be ISO dates",
                ))
            }
        };
        if start > end {
            return Err(WmError::value_error(
                "start_date must not be after end_date",
            ));
        }
        let today_days = pytime::date_fromisoformat(&env.today()).unwrap_or_default();
        if end < today_days - 14 {
            return Ok(json!({
                "ok": false,
                "state": "unavailable",
                "reason": "requested_date_outside_provider_rolling_window",
            }));
        }
        let mut days = Vec::new();
        for offset in 0..=(end - start) {
            let (year, month, day) = pytime::civil_from_days(start + offset);
            days.push(format!("{year:04}-{month:02}-{day:02}"));
        }
        requested_days = Some(days);
    }
    let bearer = match bearer(env, args.token)? {
        Ok(bearer) => bearer,
        Err(message) => {
            return Ok(json!({
                "ok": false,
                "state": "not_configured",
                "verified": false,
                "error": message,
            }))
        }
    };

    let outcome: Result<Value, WmError> = async {
        let user = if pystr::truthy(args.user_id) {
            args.user_id.cloned().unwrap_or(Value::Null)
        } else {
            Value::String(resolve_user_id(call, &bearer).await?)
        };
        let user_args = CollectArgs {
            user_id: Some(&user),
            token: Some(&bearer),
            ..CollectArgs::default()
        };
        let host_listing = collect_inner("hosts", &user_args, env, call).await?;
        if !pystr::truthy(host_listing.get("ok")) {
            return Ok(json!({
                "ok": false,
                "state": "verification_failed",
                "error": "Webmaster host verification failed",
            }));
        }
        let data = host_listing.get("data").cloned().unwrap_or(json!({}));
        let hosts = match &data {
            Value::Object(map) => map.get("hosts").cloned(),
            other => return Err(WmError::attribute_get(other)),
        };
        let hosts = match hosts {
            Some(Value::Array(items))
                if items.iter().all(|item| matches!(item, Value::Object(_))) =>
            {
                items
            }
            _ => {
                return Ok(json!({
                    "ok": false,
                    "state": "verification_failed",
                    "error": "malformed Webmaster host list",
                }))
            }
        };
        // Host IDs must be hashable; an unhashable one is rejected here.
        for item in &hosts {
            if let Some(bad) = item.get("host_id") {
                if matches!(bad, Value::Array(_) | Value::Object(_)) {
                    return Err(WmError::type_error(format!(
                        "cannot use '{}' as a set element (unhashable type: '{}')",
                        pystr::type_name(bad),
                        pystr::type_name(bad)
                    )));
                }
            }
        }
        // An unhashable host ID cannot be a set member, so reject it before the lookup.
        if matches!(host_id, Value::Array(_) | Value::Object(_)) {
            return Err(WmError::type_error(format!(
                "cannot use '{}' as a set element (unhashable type: '{}')",
                pystr::type_name(host_id),
                pystr::type_name(host_id)
            )));
        }
        let granted = hosts.iter().any(|item| {
            crate::metrika::py_eq(
                item.get("host_id").unwrap_or(&Value::Null),
                host_id,
            )
        });
        if !granted {
            return Ok(json!({
                "ok": false,
                "state": "not_granted",
                "host_id": host_id,
            }));
        }
        let host_str = match host_id {
            Value::String(text) => text.clone(),
            other => {
                return Err(WmError::type_error(format!(
                    "replace() argument 2 must be str, not {}",
                    pystr::type_name(other)
                )))
            }
        };
        let endpoint = format!(
            "{HOST}/user/{}{}",
            str_value(&user),
            QUERY_ANALYTICS_PATH.replace("{host}", &host_str)
        );
        let url_contains = args.url_contains.cloned().unwrap_or(Value::Null);
        let url_value = if pystr::truthy(args.url) {
            args.url.cloned().unwrap_or(Value::Null)
        } else {
            url_contains.clone()
        };
        let (entries, urls_truncated) = query_analytics_request(
            call,
            &endpoint,
            "URL",
            &url_value,
            max_urls,
            &bearer,
            &url_contains,
            env.sleeper,
        )
        .await?;
        let mut urls: Vec<String> = Vec::new();
        for item in &entries {
            let text_indicator = item.get("text_indicator").cloned().unwrap_or(json!({}));
            let value = match &text_indicator {
                Value::Object(map) => map.get("value").cloned().unwrap_or(Value::Null),
                other => return Err(WmError::attribute_get(other)),
            };
            urls.push(match value {
                Value::String(text) if !text.is_empty() => text,
                _ => String::new(),
            });
        }
        if !urls.iter().all(|u| !u.is_empty()) {
            return Err(WmError::value_error(
                "malformed Yandex Webmaster URL result",
            ));
        }
        let mut rows: Vec<Value> = Vec::new();
        for page in &urls {
            let (queries, query_truncated) = query_analytics_request(
                call,
                &endpoint,
                "QUERY",
                &Value::String(page.clone()),
                max_queries,
                &bearer,
                &url_contains,
                env.sleeper,
            )
            .await?;
            for item in &queries {
                let text_indicator = item.get("text_indicator").cloned().unwrap_or(json!({}));
                let query = match &text_indicator {
                    Value::Object(map) => map.get("value").cloned().unwrap_or(Value::Null),
                    other => return Err(WmError::attribute_get(other)),
                };
                let statistics = item.get("statistics").cloned().unwrap_or(Value::Null);
                let (query, statistics) = match (query, statistics) {
                    (Value::String(query), Value::Array(statistics)) => (query, statistics),
                    _ => {
                        return Err(WmError::value_error(
                            "malformed Yandex Webmaster query result",
                        ))
                    }
                };
                let mut daily = daily_statistics(&statistics)?;
                if let Some(days) = &requested_days {
                    daily.retain(|entry| {
                        entry
                            .get("date")
                            .and_then(|v| v.as_str())
                            .is_some_and(|d| days.iter().any(|day| day == d))
                    });
                }
                if !daily.is_empty() {
                    rows.push(json!({
                        "url": page,
                        "query": query,
                        "daily": daily,
                        "truncated": query_truncated,
                    }));
                }
            }
        }
        let mut observed_days: Vec<String> = rows
            .iter()
            .flat_map(|row| {
                row.get("daily")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|entry| entry.get("date").and_then(|v| v.as_str()).map(str::to_owned))
                    .collect::<Vec<_>>()
            })
            .collect();
        observed_days.sort();
        observed_days.dedup();
        let coverage = match &requested_days {
            Some(days) => {
                let unobserved: Vec<String> = days
                    .iter()
                    .filter(|day| !observed_days.contains(day))
                    .cloned()
                    .collect();
                json!({
                    "requested_days": days,
                    "observed_days": observed_days,
                    "unobserved_days": unobserved,
                    "state": "partial_or_unknown",
                })
            }
            None => Value::Null,
        };
        let any_truncated = urls_truncated
            || rows
                .iter()
                .any(|row| matches!(row.get("truncated"), Some(Value::Bool(true))));
        Ok(json!({
            "ok": true,
            "state": if any_truncated { "partial" } else { "complete" },
            "host_id": host_id,
            "property_access": "verified",
            "result_state": if rows.is_empty() { "empty" } else { "observed" },
            "rows": rows,
            "returned_urls": urls.len(),
            "returned_queries": rows.len(),
            "truncated": any_truncated,
            "coverage": coverage,
            "scope": "Yandex Webmaster query analytics; provider retention and metric attribution apply",
        }))
    }
    .await;

    match outcome {
        Ok(result) => Ok(result),
        Err(WmError::Http { code, body, .. }) => {
            let mut error_code = format!("HTTP_{code}");
            if let Ok(payload) = serde_json::from_str::<Value>(&body) {
                if let Some(Value::String(code)) =
                    payload.get("error_code").filter(|_| payload.is_object())
                {
                    error_code = code.clone();
                }
            }
            Ok(json!({
                "ok": false,
                "state": "failed",
                "error_code": error_code,
            }))
        }
        Err(err) if err.queries_catches() => Ok(json!({
            "ok": false,
            "state": "failed",
            "error": "Yandex Webmaster query analytics failed",
        })),
        Err(err) => Err(err),
    }
}

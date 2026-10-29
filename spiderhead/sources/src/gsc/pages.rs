//! `search_analytics_pages` and `search_analytics_page` — the bounded/paginated archive
//! queries of the webmasters/v3 API.

use serde_json::{json, Value};
use spiderhead_core::host::Host;
use std::path::Path;

use super::{
    acquire_token, analytics_endpoint, py_truthy, resolve_date_range, response_object, send,
    transport_text, validate_date_range, Fetch, GscError, ServiceAccount, AGGREGATION_TYPES,
    ARCHIVE_DIMENSIONS, MAX_ANALYTICS_ROWS, MAX_START_ROW, SEARCH_TYPES,
};
use crate::http::Transport;
use crate::{pyerr, pystr, pytime};

/// `search_analytics_pages` — paginate bounded Search Analytics rows with explicit
/// truncation and quota limits.
pub async fn search_analytics_pages<T: Transport>(
    host: &dyn Host,
    root: &Path,
    site_url: &str,
    args: &PagesArgs<'_>,
    transport: &T,
) -> Result<Value, GscError> {
    if site_url.is_empty()
        || !(1..=25_000).contains(&args.row_limit)
        || !(1..=MAX_ANALYTICS_ROWS).contains(&args.max_rows)
    {
        return Err(GscError::Invalid(
            "site_url and bounded row limits are required".to_owned(),
        ));
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
        return Ok(json!({
            "ok": false,
            "state": "not_configured",
            "verified": false,
            "error": token_error,
        }));
    };
    let endpoint = analytics_endpoint(site_url);
    // Any falsy dimensions value means the default `page` dimension.
    let dimensions = match args.dimensions.as_ref().filter(|d| py_truthy(d)) {
        Some(dims) => dims.clone(),
        None => json!(["page"]),
    };
    let mut rows: Vec<Value> = Vec::new();
    let mut offset: i64 = 0;
    while offset < args.max_rows {
        let row_limit = args.row_limit.min(args.max_rows - offset);
        let payload = json!({
            "startDate": start_date,
            "endDate": end_date,
            "dimensions": dimensions,
            "rowLimit": row_limit,
            "startRow": offset,
        });
        let body = match send(transport, "POST", &endpoint, Some(&payload), &bearer).await {
            Fetch::Body(text) => response_object(&text),
            Fetch::Http(response) => {
                return Ok(json!({
                    "ok": false,
                    "state": "failed",
                    "error": pyerr::http_error(response.status),
                    "returned": rows.len(),
                }))
            }
            Fetch::Connect(kind) => {
                return Ok(json!({
                    "ok": false,
                    "state": "failed",
                    "error": transport_text(&kind),
                    "returned": rows.len(),
                }))
            }
            // Oversized and undecodable responses are not caught here; they propagate as errors.
            Fetch::TooLarge => {
                return Err(GscError::TooLarge(
                    "Search Console response exceeded the 8 MiB limit".to_owned(),
                ))
            }
            Fetch::Decode(message) => return Err(GscError::Wrapped("UnicodeDecodeError", message)),
        };
        // An empty body is malformed; a non-empty object without `rows` is an empty, completed page.
        let batch = body
            .as_ref()
            .filter(|b| !b.is_empty())
            .map(|b| b.get("rows").cloned().unwrap_or_else(|| json!([])))
            .filter(|v| {
                v.as_array()
                    .map(|l| l.iter().all(Value::is_object))
                    .unwrap_or(false)
            });
        let Some(batch) = batch.and_then(|v| v.as_array().cloned()) else {
            return Ok(json!({
                "ok": false,
                "state": "failed",
                "error": "malformed GSC analytics response",
            }));
        };
        let batch_len = batch.len() as i64;
        rows.extend(batch);
        if batch_len < row_limit {
            break;
        }
        offset += batch_len;
    }
    Ok(json!({
        "ok": true,
        "state": if (rows.len() as i64) < args.max_rows { "complete" } else { "partial" },
        "period": {"start_date": start_date, "end_date": end_date},
        "dimensions": dimensions,
        "rows": rows,
        "returned": rows.len(),
        "truncated": rows.len() as i64 == args.max_rows,
        "quota_mode": "provider row limit; bounded locally",
    }))
}

/// The keyword arguments of `search_analytics_pages`.
pub struct PagesArgs<'a> {
    /// `start_date`/`end_date` — `None`/`None` resolves the completed 28-day window.
    pub start_date: Option<&'a str>,
    /// `end_date`.
    pub end_date: Option<&'a str>,
    /// `dimensions` — any falsy value defaults to `["page"]`.
    pub dimensions: Option<Value>,
    /// `row_limit` — 1..=25_000 (default 1_000).
    pub row_limit: i64,
    /// `max_rows` — 1..=25_000 (default [`MAX_ANALYTICS_ROWS`]).
    pub max_rows: i64,
    /// `token` — an explicit bearer.
    pub token: Option<&'a Value>,
    /// Fixed clock for the default date window, as epoch microseconds.
    pub now: Option<i64>,
    /// Injected replacement for the service-account token exchange.
    pub service_account: ServiceAccount<'a>,
}

/// `search_analytics_page` — one archive page with no implicit pagination or retries.
/// Failures never echo provider messages or exception text, because either may include
/// authentication material.
pub async fn search_analytics_page<T: Transport>(
    host: &dyn Host,
    root: &Path,
    site_url: &str,
    args: &PageArgs<'_>,
    transport: &T,
) -> Result<Value, GscError> {
    let invalid = "Invalid Search Console page request";
    if !site_url_is_usable(site_url) {
        return Ok(failure(
            "invalid_argument",
            &format!("{invalid}: site_url is required"),
            None,
        ));
    }
    if validate_date_range(args.start_date, args.end_date).is_some() {
        return Ok(failure(
            "invalid_argument",
            &format!("{invalid}: use an ordered YYYY-MM-DD date range"),
            None,
        ));
    }
    // Absent `row_limit` takes the default; an explicit non-integer (string, bool, float, null) or an out-of-range value is invalid.
    let row_limit = match args.row_limit {
        None => MAX_ANALYTICS_ROWS,
        Some(v) => {
            match pystr::as_strict_i64(Some(v)).filter(|n| (1..=MAX_ANALYTICS_ROWS).contains(n)) {
                Some(n) => n,
                None => {
                    return Ok(failure(
                        "invalid_argument",
                        &format!("{invalid}: row_limit is 1..25000; start_row is 0..50000"),
                        None,
                    ))
                }
            }
        }
    };
    let start_row = match args.start_row {
        None => 0,
        Some(v) => {
            match pystr::as_strict_i64(Some(v)).filter(|n| (0..=MAX_START_ROW).contains(n)) {
                Some(n) => n,
                None => {
                    return Ok(failure(
                        "invalid_argument",
                        &format!("{invalid}: row_limit is 1..25000; start_row is 0..50000"),
                        None,
                    ))
                }
            }
        }
    };
    let search_type = args.search_type.and_then(Value::as_str).unwrap_or("web");
    let aggregation_type = args
        .aggregation_type
        .and_then(Value::as_str)
        .unwrap_or("auto");
    let data_state = args.data_state.and_then(Value::as_str).unwrap_or("final");
    if !SEARCH_TYPES.contains(&search_type)
        || !AGGREGATION_TYPES.contains(&aggregation_type)
        || !matches!(data_state, "final" | "all")
        || (matches!(search_type, "discover" | "googleNews") && aggregation_type == "byProperty")
    {
        return Ok(failure(
            "invalid_argument",
            &format!("{invalid}: unsupported type, aggregation, or data state"),
            None,
        ));
    }
    // Absent dimensions become an empty list, which must then pass a strict list check.
    let dimensions_value = args.dimensions.clone().unwrap_or_else(|| json!([]));
    let dimensions = match dimensions_value.as_array() {
        Some(list) if dimensions_valid(list) => list.clone(),
        _ => {
            return Ok(failure(
                "invalid_argument",
                &format!("{invalid}: invalid or repeated dimensions"),
                None,
            ))
        }
    };
    if let Some(groups) = &args.dimension_filter_groups {
        if let Some(message) = filter_groups_error(groups) {
            return Ok(failure(
                "invalid_argument",
                &format!("{invalid}: {message}"),
                None,
            ));
        }
    }
    if args.token.is_some_and(|t| !t.is_string()) {
        return Ok(failure(
            "invalid_argument",
            &format!("{invalid}: token must be a string"),
            None,
        ));
    }
    let bearer = match acquire_token(host, root, args.token, transport, args.service_account).await
    {
        Ok((Some(bearer), _)) if !bearer.is_empty() => bearer,
        _ => {
            return Ok(failure(
                "not_configured",
                "Search Console authentication is unavailable",
                None,
            ))
        }
    };
    let endpoint = analytics_endpoint(site_url);
    let mut payload = json!({
        "startDate": args.start_date,
        "endDate": args.end_date,
        "dimensions": dimensions,
        "type": search_type,
        "aggregationType": aggregation_type,
        "dataState": data_state,
        "rowLimit": row_limit,
        "startRow": start_row,
    });
    if let Some(groups) = &args.dimension_filter_groups {
        payload
            .as_object_mut()
            .map(|m| m.insert("dimensionFilterGroups".to_owned(), groups.clone()));
    }
    let raw = match send(transport, "POST", &endpoint, Some(&payload), &bearer).await {
        Fetch::Body(text) => text,
        Fetch::Http(response) => {
            let error_body = response_object(&String::from_utf8_lossy(&response.body));
            return Ok(api_failure(
                error_body.as_ref(),
                Some(i64::from(response.status)),
            ));
        }
        // Transport, oversized and undecodable answers all collapse into the same failure.
        Fetch::Connect(_) | Fetch::TooLarge | Fetch::Decode(_) => {
            return Ok(failure(
                "transport_error",
                "Search Console request failed before a valid response",
                None,
            ))
        }
    };
    let malformed = || {
        failure(
            "malformed_response",
            "Search Console malformed analytics response",
            Some(200),
        )
    };
    let Some(body) = response_object(&raw) else {
        return Ok(malformed());
    };
    if body.contains_key("error") || body.contains_key("errors") {
        return Ok(api_failure(Some(&body), None));
    }
    let batch = body.get("rows").cloned().unwrap_or_else(|| json!([]));
    let metadata = body.get("metadata").cloned().unwrap_or_else(|| json!({}));
    let aggregation = body.get("responseAggregationType").cloned();
    let aggregation_bad = aggregation
        .as_ref()
        .map(|a| {
            !a.is_null()
                && !a
                    .as_str()
                    .map(|s| AGGREGATION_TYPES.contains(&s))
                    .unwrap_or(false)
        })
        .unwrap_or(false);
    if !batch.is_array()
        || batch.as_array().map(|b| b.len() as i64).unwrap_or_default() > row_limit
        || !metadata.is_object()
        || aggregation_bad
    {
        return Ok(malformed());
    }
    let mut rows = Vec::new();
    for row in batch.as_array().cloned().unwrap_or_default() {
        let Some(row) = row.as_object() else {
            return Ok(malformed());
        };
        let keys = row.get("keys").cloned().unwrap_or_else(|| json!([]));
        let keys_ok = keys
            .as_array()
            .map(|k| k.len() == dimensions.len() && k.iter().all(Value::is_string))
            .unwrap_or(false);
        if !keys_ok {
            return Ok(malformed());
        }
        for metric in ["clicks", "impressions", "ctr", "position"] {
            match row.get(metric) {
                None | Some(Value::Null) if matches!(metric, "ctr" | "position") => continue,
                other => {
                    let ok = other
                        .and_then(Value::as_f64)
                        .map(|f| f.is_finite() && f >= 0.0)
                        .unwrap_or(false);
                    if !ok {
                        return Ok(malformed());
                    }
                }
            }
        }
        rows.push(json!({
            "keys": keys,
            "clicks": row.get("clicks").cloned().unwrap_or(Value::Null),
            "impressions": row.get("impressions").cloned().unwrap_or(Value::Null),
            "ctr": row.get("ctr").cloned().unwrap_or(Value::Null),
            "position": row.get("position").cloned().unwrap_or(Value::Null),
        }));
    }
    Ok(json!({
        "ok": true,
        "rows": rows,
        "response_aggregation_type": aggregation.unwrap_or(Value::Null),
        "metadata": metadata,
        "error": Value::Null,
        "status": 200,
        "reason": Value::Null,
    }))
}

fn failure(reason: &str, error: &str, status: Option<i64>) -> Value {
    json!({
        "ok": false,
        "rows": [],
        "response_aggregation_type": Value::Null,
        "metadata": {},
        "error": error,
        "status": status,
        "reason": reason,
    })
}

fn site_url_is_usable(site_url: &str) -> bool {
    !pystr::strip(site_url).is_empty()
}

fn dimensions_valid(dimensions: &[Value]) -> bool {
    let mut seen = std::collections::BTreeSet::new();
    dimensions.iter().all(|d| {
        d.as_str()
            .map(|s| ARCHIVE_DIMENSIONS.contains(&s) && seen.insert(s))
            .unwrap_or(false)
    })
}

/// Validate `dimension_filter_groups`; `Some(message)` names one of three distinct failures.
fn filter_groups_error(groups: &Value) -> Option<&'static str> {
    if !groups.is_array() {
        return Some("filter groups must be a JSON list");
    }
    for group in groups.as_array().cloned().unwrap_or_default() {
        let Some(group) = group.as_object() else {
            return Some("malformed filter group");
        };
        let group_type_ok = match group.get("groupType") {
            None => true,
            Some(v) => v == &json!("and"),
        };
        let filters_ok = group
            .get("filters")
            .and_then(Value::as_array)
            .map(|f| !f.is_empty())
            .unwrap_or(false);
        if group
            .keys()
            .any(|k| !matches!(k.as_str(), "groupType" | "filters"))
            || !group_type_ok
            || !filters_ok
        {
            return Some("malformed filter group");
        }
        for item in group
            .get("filters")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let Some(item) = item.as_object() else {
                return Some("malformed dimension filter");
            };
            let keys: std::collections::BTreeSet<&str> = item.keys().map(String::as_str).collect();
            let expected: std::collections::BTreeSet<&str> =
                ["dimension", "operator", "expression"]
                    .into_iter()
                    .collect();
            if keys != expected
                || item
                    .get("dimension")
                    .and_then(Value::as_str)
                    .map(|d| !ARCHIVE_DIMENSIONS.contains(&d))
                    .unwrap_or(true)
                || item
                    .get("operator")
                    .and_then(Value::as_str)
                    .map(|o| {
                        !matches!(
                            o,
                            "contains"
                                | "equals"
                                | "notContains"
                                | "notEquals"
                                | "includingRegex"
                                | "excludingRegex"
                        )
                    })
                    .unwrap_or(true)
                || item.get("expression").and_then(Value::as_str).is_none()
            {
                return Some("malformed dimension filter");
            }
        }
    }
    None
}

/// `api_failure` — the stable local failure for provider-side errors. `status` is the HTTP
/// code or the body's own `error.code`; `reason` is a local code or an allowlisted Google
/// reason — provider `message` text never leaves this function.
pub(crate) fn api_failure(
    body: Option<&serde_json::Map<String, Value>>,
    status: Option<i64>,
) -> Value {
    const KNOWN: [&str; 13] = [
        "quotaExceeded",
        "rateLimitExceeded",
        "userRateLimitExceeded",
        "dailyLimitExceeded",
        "servingLimitExceeded",
        "forbidden",
        "insufficientPermissions",
        "authError",
        "keyInvalid",
        "accessNotConfigured",
        "notFound",
        "invalid",
        "badRequest",
    ];
    let mut status = status;
    let mut reason = "api_error";
    if let Some(error) = body.and_then(|b| b.get("error")).and_then(Value::as_object) {
        if status.is_none() {
            status = pystr::as_strict_i64(error.get("code")).or(status);
        }
        if let Some(details) = error.get("errors").and_then(Value::as_array) {
            for detail in details {
                if let Some(candidate) = detail
                    .as_object()
                    .and_then(|d| d.get("reason"))
                    .and_then(Value::as_str)
                    .filter(|r| KNOWN.contains(r))
                {
                    reason = candidate;
                    break;
                }
            }
        }
    }
    json!({
        "ok": false,
        "rows": [],
        "response_aggregation_type": Value::Null,
        "metadata": {},
        "error": "Search Console API request failed",
        "status": status,
        "reason": reason,
    })
}

/// The keyword arguments of `search_analytics_page`; `Option` members are `None` when the
/// JSON call carried `null`, which the same validations reject.
pub struct PageArgs<'a> {
    /// `start_date` — required ISO date (`None` = explicit null → invalid).
    pub start_date: Option<&'a str>,
    /// `end_date`.
    pub end_date: Option<&'a str>,
    /// `dimensions` — `None` requests property totals; a non-list is invalid.
    pub dimensions: Option<Value>,
    /// `search_type` (`web` when omitted; an explicit non-string member is invalid).
    pub search_type: Option<&'a Value>,
    /// `aggregation_type` (`auto` when omitted).
    pub aggregation_type: Option<&'a Value>,
    /// `data_state` (`final` when omitted).
    pub data_state: Option<&'a Value>,
    /// `row_limit` — 1..=25_000 (`None` takes the default 25_000).
    pub row_limit: Option<&'a Value>,
    /// `start_row` — 0..=50_000 (`None` takes the default 0).
    pub start_row: Option<&'a Value>,
    /// `dimension_filter_groups` — a JSON list of filter groups.
    pub dimension_filter_groups: Option<Value>,
    /// `token` — an explicit bearer; a non-string token is `invalid_argument`.
    pub token: Option<&'a Value>,
    /// Injected replacement for the service-account token exchange.
    pub service_account: ServiceAccount<'a>,
}

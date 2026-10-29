//! `sources_db` provider fetchers — the row-shaping adapters between a provider
//! client's result envelope and the store's `{ok, rows, complete, error}` contract.
//!
//! Each fetcher is a pure transformation driven by an injected seam, so every branch runs offline.
//! Live client wiring lives in `sync_ops`.

use serde_json::{json, Map, Value};

use super::{day_list, number};
use crate::pystr;

/// A provider-adapter failure carrying an error class name and message.
/// `sync` renders it as `provider adapter raised <class>`; direct callers see the raw message.
#[derive(Debug)]
pub struct FetcherFault {
    /// Error class name.
    pub class: String,
    /// Error message text.
    pub message: String,
}

impl FetcherFault {
    /// Member lookup on a value that is not an object.
    fn attribute_get(value: &Value) -> Self {
        FetcherFault {
            class: "AttributeError".to_owned(),
            message: format!(
                "'{}' object has no attribute 'get'",
                pystr::type_name(value)
            ),
        }
    }

    /// A required key is missing; the message quotes the key.
    fn key(name: &str) -> Self {
        FetcherFault {
            class: "KeyError".to_owned(),
            message: pystr::repr_str(name),
        }
    }

    /// A value of the wrong type, with its standard message.
    fn type_error(message: impl Into<String>) -> Self {
        FetcherFault {
            class: "TypeError".to_owned(),
            message: message.into(),
        }
    }
}

/// Retryable only on transport failures and 5xx statuses.
/// A boolean status counts as a number and is never 5xx.
pub(crate) fn failure(result: &Value) -> Value {
    let status = result.get("status");
    let is_int = match status {
        Some(Value::Bool(_)) => true,
        Some(value) => pystr::is_py_int(value),
        _ => false,
    };
    let over_500 = match status {
        Some(Value::Number(n)) => n
            .as_i64()
            .map(|v| v >= 500)
            .unwrap_or_else(|| n.as_u64().map(|u| u >= 500).unwrap_or(false)),
        // `true` and `false` are never 5xx, even though they count as numbers.
        _ => false,
    };
    json!({
        "ok": false,
        "error": result.get("error").cloned().unwrap_or_default(),
        "status": status.cloned().unwrap_or_default(),
        "retry": !is_int || over_500,
    })
}

/// Read the `ok` member; a value that is not an object is an error.
fn ok_of(result: &Value) -> Result<bool, FetcherFault> {
    match result {
        Value::Object(_) => Ok(pystr::truthy(result.get("ok"))),
        other => Err(FetcherFault::attribute_get(other)),
    }
}

/// Iterate a member: a non-iterable value is a type error; object keys and string characters iterate,
/// and each then fails as a non-object row.
fn iterate_rows(member: &Value) -> Result<Vec<Value>, FetcherFault> {
    match member {
        Value::Array(rows) => Ok(rows.clone()),
        Value::Object(rows) => Ok(rows.keys().cloned().map(Value::String).collect()),
        Value::String(text) => Ok(text.chars().map(|c| Value::String(c.to_string())).collect()),
        other => Err(FetcherFault::type_error(format!(
            "'{}' object is not iterable",
            pystr::type_name(other)
        ))),
    }
}

/// `_fetch_gsc`'s transformation of a `search_analytics_pages` result — the live GSC
/// call itself sits in `sync_ops` where transport is available.
pub fn fetch_gsc_result(result: &Value) -> Result<Value, FetcherFault> {
    if !ok_of(result)? {
        return Ok(failure(result));
    }
    let member = match result.get("rows") {
        Some(member) => member,
        None => return Err(FetcherFault::key("rows")),
    };
    let mut rows = Vec::new();
    for row in iterate_rows(member)? {
        let Some(object) = row.as_object() else {
            return Err(FetcherFault::attribute_get(&row));
        };
        match object.get("keys").and_then(Value::as_array) {
            Some(keys) if keys.len() == 3 => {
                let mut out = Map::new();
                out.insert("date".to_owned(), keys[0].clone());
                out.insert("query".to_owned(), keys[1].clone());
                out.insert("page".to_owned(), keys[2].clone());
                for metric in ["clicks", "impressions", "ctr", "position"] {
                    out.insert(
                        metric.to_owned(),
                        object.get(metric).cloned().unwrap_or_default(),
                    );
                }
                rows.push(Value::Object(out));
            }
            _ => {
                return Ok(json!({
                    "ok": false,
                    "error": "malformed GSC dimension keys",
                    "retry": false,
                }));
            }
        }
    }
    // Complete only when the state is `complete` (the default) and the result is not truncated.
    let state_complete = match result.get("state") {
        None => true,
        Some(Value::String(s)) => s == "complete",
        Some(_) => false,
    };
    Ok(json!({
        "ok": true,
        "rows": rows,
        "complete": state_complete && !pystr::truthy(result.get("truncated")),
    }))
}

/// Transform a GA4 `run_report` result: `date` arrives as `YYYYMMDD` and is reformatted;
/// `state` has no default, so a missing state is not complete.
pub fn fetch_ga4_result(result: &Value) -> Result<Value, FetcherFault> {
    if !ok_of(result)? {
        return Ok(failure(result));
    }
    let member = match result.get("rows") {
        Some(member) => member,
        None => return Err(FetcherFault::key("rows")),
    };
    let mut rows = Vec::new();
    for row in iterate_rows(member)? {
        // A missing `date` is a key error; a non-indexable row is a type error. A list `date` is passed through unchanged.
        let date_value = match &row {
            Value::Object(object) => match object.get("date") {
                Some(value) => value,
                None => return Err(FetcherFault::key("date")),
            },
            Value::Array(_) => {
                return Err(FetcherFault::type_error(
                    "list indices must be integers or slices, not str",
                ))
            }
            other => {
                return Err(FetcherFault::type_error(format!(
                    "'{}' object is not subscriptable",
                    pystr::type_name(other)
                )))
            }
        };
        let formatted = match date_value {
            Value::String(date) => {
                let chars: Vec<char> = date.chars().collect();
                let slice =
                    |a: usize, b: usize| -> String { chars.iter().skip(a).take(b - a).collect() };
                json!(format!("{}-{}-{}", slice(0, 4), slice(4, 6), slice(6, 8)))
            }
            Value::Array(list) => {
                // Slicing a list `date` yields list text in the reformatted value.
                let repr = |a: usize, b: usize| -> String {
                    let sub: Vec<Value> = list.iter().skip(a).take(b - a).cloned().collect();
                    pystr::repr_value(&Value::Array(sub))
                };
                json!(format!("{}-{}-{}", repr(0, 4), repr(4, 6), repr(6, 8)))
            }
            other => {
                return Err(FetcherFault::type_error(format!(
                    "'{}' object is not subscriptable",
                    pystr::type_name(other)
                )))
            }
        };
        let object = row.as_object().cloned().unwrap_or_default();
        let mut out = Map::new();
        out.insert("date".to_owned(), formatted);
        for (target, key) in [
            ("landing_page", "landingPagePlusQueryString"),
            ("channel", "sessionDefaultChannelGroup"),
            ("sessions", "sessions"),
            ("engaged_sessions", "engagedSessions"),
            ("users", "totalUsers"),
        ] {
            out.insert(
                target.to_owned(),
                object.get(key).cloned().unwrap_or_default(),
            );
        }
        rows.push(Value::Object(out));
    }
    let complete = matches!(result.get("state"), Some(Value::String(s)) if s == "complete");
    Ok(json!({"ok": true, "rows": rows, "complete": complete}))
}

/// Failures from the Metrika report call, mapped to envelope fields.
#[derive(Debug)]
pub enum MetrikaFail {
    /// Missing credential: the envelope carries the error message.
    MissingCredential(String),
    /// Metrika API error: the envelope carries its message and status.
    /// Boxed: `Value` pairs make the variant too large for `Result` returns.
    Metrika {
        message: Box<Value>,
        status: Box<Value>,
    },
}

/// `_fetch_metrika` — takes the `report` outcome (body or adapter failure) and shapes
/// the `{date, traffic_source, landing_page, visits, users, bounce_rate}` rows.
pub fn fetch_metrika_result(result: Result<Value, MetrikaFail>) -> Value {
    let body = match result {
        Ok(body) => body,
        Err(MetrikaFail::MissingCredential(message)) => {
            return json!({"ok": false, "error": message});
        }
        Err(MetrikaFail::Metrika { message, status }) => {
            return json!({"ok": false, "error": message, "status": status});
        }
    };
    let data = match body.as_object() {
        Some(object) => object.get("data"),
        None => None,
    };
    let Some(data) = data.and_then(Value::as_array) else {
        return json!({"ok": false, "error": "malformed Metrika report data", "retry": false});
    };
    let mut rows = Vec::new();
    for row in data {
        let malformed = match row.as_object() {
            Some(object) => {
                let dimensions = object.get("dimensions").and_then(Value::as_array);
                let metrics = object.get("metrics").and_then(Value::as_array);
                match (dimensions, metrics) {
                    (Some(dimensions), Some(metrics))
                        if dimensions.len() == 3
                            && dimensions.iter().all(|item| {
                                item.as_object()
                                    .map(|item| item.contains_key("name"))
                                    .unwrap_or(false)
                            })
                            && metrics.len() == 3 =>
                    {
                        let dims: Vec<Value> =
                            dimensions.iter().map(|item| item["name"].clone()).collect();
                        let mut out = Map::new();
                        out.insert("date".to_owned(), dims[0].clone());
                        out.insert("traffic_source".to_owned(), dims[1].clone());
                        out.insert("landing_page".to_owned(), dims[2].clone());
                        out.insert("visits".to_owned(), metrics[0].clone());
                        out.insert("users".to_owned(), metrics[1].clone());
                        out.insert("bounce_rate".to_owned(), metrics[2].clone());
                        rows.push(Value::Object(out));
                        false
                    }
                    _ => true,
                }
            }
            None => true,
        };
        if malformed {
            return json!({"ok": false, "error": "malformed Metrika report row", "retry": false});
        }
    }
    let complete = !pystr::truthy(body.get("capped")) && !pystr::truthy(body.get("sampled"));
    json!({"ok": true, "rows": rows, "complete": complete})
}

/// `yandex_webmaster.collect` — `(operation, host_id, params, paginate)` seam.
pub type WebmasterCall<'a> =
    dyn FnMut(&str, &str, &Value, bool) -> Result<Value, FetcherFault> + 'a;

const WEBMASTER_INDICATORS: [&str; 4] = [
    "TOTAL_SHOWS",
    "TOTAL_CLICKS",
    "AVG_SHOW_POSITION",
    "AVG_CLICK_POSITION",
];

/// Read `data` from a result: a missing key or a non-object result is an error.
fn result_data(result: &Value) -> Result<&Map<String, Value>, FetcherFault> {
    match result.get("data") {
        None => Err(FetcherFault::key("data")),
        Some(Value::Object(data)) => Ok(data),
        Some(other) => Err(FetcherFault::attribute_get(other)),
    }
}

/// The per-day outcome of `_fetch_webmaster`'s shaping — shared between the scripted
/// seam and the live `yandex_webmaster.collect` adapter in `sync_ops`.
pub(crate) enum WebmasterDay {
    /// Return this envelope immediately (provider failure or malformed body).
    Return(Value),
    /// Append these rows; `complete` folds into the running flag.
    Rows {
        /// Flattened `{date, query, shows, clicks, show_position, click_position}` rows.
        rows: Vec<Value>,
        /// `not truncated and count == len(queries)` for this day.
        complete: bool,
    },
}

/// One `search_performance` day of `_fetch_webmaster` — validates the envelope and
/// flattens its query rows.
pub(crate) fn webmaster_day(result: &Value, day: &str) -> Result<WebmasterDay, FetcherFault> {
    if !ok_of(result)? {
        return Ok(WebmasterDay::Return(failure(result)));
    }
    let data = result_data(result)?;
    let queries = data.get("queries");
    let count = data.get("count");
    let shape_ok = queries.map(Value::is_array).unwrap_or(false)
        && count.map(pystr::is_py_int).unwrap_or(false)
        && count
            .and_then(|count| {
                count
                    .as_i64()
                    .or_else(|| count.as_u64().and_then(|u| i64::try_from(u).ok()))
            })
            .map(|count| {
                count >= queries.and_then(Value::as_array).map(Vec::len).unwrap_or(0) as i64
            })
            .unwrap_or(false)
        && queries
            .and_then(Value::as_array)
            .map(|queries| queries.iter().all(Value::is_object))
            .unwrap_or(false);
    if !shape_ok {
        return Ok(WebmasterDay::Return(
            json!({"ok": false, "error": "malformed Webmaster queries", "retry": false}),
        ));
    }
    let queries = queries
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let count_i = count.and_then(|c| {
        c.as_i64()
            .or_else(|| c.as_u64().and_then(|u| i64::try_from(u).ok()))
    });
    let complete = !pystr::truthy(result.get("truncated"))
        && count_i.unwrap_or_default() == queries.len() as i64;
    let mut rows = Vec::new();
    for item in &queries {
        let object = item.as_object().cloned().unwrap_or_default();
        // A falsy `indicators` member counts as an empty object.
        let values = object
            .get("indicators")
            .filter(|v| pystr::truthy(Some(v)))
            .cloned()
            .unwrap_or_else(|| json!({}));
        if !values.is_object() || object.get("query_text").is_none_or(Value::is_null) {
            return Ok(WebmasterDay::Return(json!({
                "ok": false, "error": "malformed Webmaster query row", "retry": false
            })));
        }
        let values = values.as_object().cloned().unwrap_or_default();
        let mut out = Map::new();
        out.insert("date".to_owned(), json!(day));
        out.insert(
            "query".to_owned(),
            object.get("query_text").cloned().unwrap_or_default(),
        );
        for (target, key) in [
            ("shows", "TOTAL_SHOWS"),
            ("clicks", "TOTAL_CLICKS"),
            ("show_position", "AVG_SHOW_POSITION"),
            ("click_position", "AVG_CLICK_POSITION"),
        ] {
            out.insert(
                target.to_owned(),
                values.get(key).cloned().unwrap_or_default(),
            );
        }
        rows.push(Value::Object(out));
    }
    Ok(WebmasterDay::Rows { rows, complete })
}

/// `_fetch_webmaster` — per-day `search_performance` calls with the provider's own
/// pagination; rows flatten to `{date, query, shows, clicks, show_position, click_position}`.
pub fn fetch_webmaster(
    collect: &mut WebmasterCall<'_>,
    resource: &str,
    start: &str,
    end: &str,
) -> Result<Value, FetcherFault> {
    let days = day_list(start, end).map_err(|err| FetcherFault {
        class: "ValueError".to_owned(),
        message: err.to_string(),
    })?;
    let mut rows = Vec::new();
    let mut complete = true;
    for day in days {
        let params = json!({
            "query_indicator": WEBMASTER_INDICATORS,
            "date_from": day,
            "date_to": day,
        });
        let result = collect("search_performance", resource, &params, true)?;
        match webmaster_day(&result, &day)? {
            WebmasterDay::Return(envelope) => return Ok(envelope),
            WebmasterDay::Rows {
                rows: day_rows,
                complete: day_complete,
            } => {
                rows.extend(day_rows);
                complete = complete && day_complete;
            }
        }
    }
    Ok(json!({"ok": true, "rows": rows, "complete": complete}))
}

/// One operation of `_fetch_webmaster_history` — `Ok(None)` continues, `Ok(Some)`
/// short-circuits with the envelope. `totals` accumulates `{(day\0indicator): point}`.
pub(crate) fn webmaster_history_op(
    result: &Value,
    operation: &str,
    totals: &mut Map<String, Value>,
) -> Result<Option<Value>, FetcherFault> {
    if !ok_of(result)? {
        return Ok(Some(failure(result)));
    }
    let data = result_data(result)?;
    let series: Vec<(String, Value)> = if operation == "in_search_history" {
        match data.get("history").and_then(Value::as_array) {
            Some(history) => vec![("IN_SEARCH".to_owned(), Value::Array(history.clone()))],
            None => {
                return Ok(Some(json!({
                    "ok": false, "error": "malformed Webmaster history", "retry": false
                })));
            }
        }
    } else {
        match data.get("indicators").and_then(Value::as_object) {
            Some(indicators) => indicators
                .iter()
                .map(|(name, points)| (name.clone(), points.clone()))
                .collect(),
            None => {
                return Ok(Some(json!({
                    "ok": false, "error": "malformed Webmaster indicators", "retry": false
                })));
            }
        }
    };
    for (indicator, points) in &series {
        let Some(points) = points.as_array() else {
            return Ok(Some(json!({
                "ok": false, "error": "malformed Webmaster points", "retry": false
            })));
        };
        for point in points {
            let object = point.as_object().cloned().unwrap_or_default();
            if !object.contains_key("date") || !object.contains_key("value") {
                return Ok(Some(json!({
                    "ok": false, "error": "malformed Webmaster point", "retry": false
                })));
            }
            let day: String = pystr::repr_value(&object["date"])
                .chars()
                .take(10)
                .collect();
            let value = match number(object.get("value")) {
                Ok(Some(value)) => value,
                Ok(None) => {
                    return Ok(Some(json!({
                        "ok": false, "error": "missing Webmaster value", "retry": false
                    })));
                }
                Err(_) => {
                    return Ok(Some(json!({
                        "ok": false, "error": "malformed Webmaster value", "retry": false
                    })));
                }
            };
            let key = format!("{day}\u{0}{indicator}");
            // Crawl counts arrive in several intra-day batches and are summed; a
            // pages-in-search snapshot is a level — the day's latest value is kept.
            let prior = if operation == "indexing_history" {
                totals
                    .get(&key)
                    .and_then(|entry| entry.get("total").and_then(Value::as_f64))
                    .unwrap_or(0.0)
            } else {
                0.0
            };
            let total = if operation == "indexing_history" {
                prior + value
            } else {
                value
            };
            totals.insert(
                key,
                json!({"date": day, "indicator": indicator, "total": total}),
            );
        }
    }
    Ok(None)
}

/// `totals` -> the sorted `{date, indicator, value}` rows (shared by both drivers).
pub(crate) fn webmaster_history_rows(totals: &Map<String, Value>) -> Vec<Value> {
    let mut pairs: Vec<(String, String, f64)> = totals
        .values()
        .filter_map(|entry| {
            Some((
                entry.get("date")?.as_str()?.to_owned(),
                entry.get("indicator")?.as_str()?.to_owned(),
                entry.get("total")?.as_f64()?,
            ))
        })
        .collect();
    // Rows sort by the (date, indicator) text pair.
    pairs.sort_by(|a, b| (a.0.as_str(), a.1.as_str()).cmp(&(b.0.as_str(), b.1.as_str())));
    pairs
        .into_iter()
        .map(|(date, indicator, total)| {
            json!({"date": date, "indicator": indicator, "value": total})
        })
        .collect()
}

/// `_fetch_webmaster_history` — daily totals: shows/clicks, pages in search, and
/// crawled pages by HTTP class.
pub fn fetch_webmaster_history(
    collect: &mut WebmasterCall<'_>,
    resource: &str,
    start: &str,
    end: &str,
) -> Result<Value, FetcherFault> {
    let mut totals: Map<String, Value> = Map::new();
    for (operation, extra) in [
        (
            "search_history",
            Some(json!(["TOTAL_SHOWS", "TOTAL_CLICKS"])),
        ),
        ("indexing_history", None),
        ("in_search_history", None),
    ] {
        let mut params = Map::new();
        params.insert("date_from".to_owned(), json!(start));
        params.insert("date_to".to_owned(), json!(end));
        if let Some(indicators) = extra {
            params.insert("query_indicator".to_owned(), indicators);
        }
        let result = collect(operation, resource, &Value::Object(params), false)?;
        if let Some(envelope) = webmaster_history_op(&result, operation, &mut totals)? {
            return Ok(envelope);
        }
    }
    Ok(json!({
        "ok": true,
        "rows": webmaster_history_rows(&totals),
        "complete": true
    }))
}

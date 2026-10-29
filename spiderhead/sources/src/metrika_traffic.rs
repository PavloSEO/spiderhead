//! Collect one Yandex Metrica traffic document.
//!
//! The document answers a monthly dashboard's questions — organic visits vs the
//! previous period and the year before, where visitors came from, which pages
//! they landed on, and how search sits among all channels — calculated once so
//! a renderer only formats it. Missing data carries `status` + `reason`, never
//! a silent zero; the `client` is injected so collection runs offline in tests.

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;

use serde_json::{json, Map, Value};

use crate::metrika::{is_number, py_eq, py_get, py_str, py_subscript, MetrikaFail};
use crate::{pystr, pytime};

mod request;
use request::*;
mod collector;
use collector::*;
mod sections;
use sections::*;
mod gsc;
pub use gsc::*;

/// Document schema marker.
pub const SCHEMA: &str = "spiderhead.metrika-traffic/1";
/// `lang` values `validate_request` accepts.
pub const LANGUAGES: [&str; 2] = ["en", "ru"];
/// `document["source"]`.
pub const SOURCE: &str = "yandex_metrika";
/// `date1`-`date2` span limit.
pub const MAX_PERIOD_DAYS: i64 = 366;
/// `top` bound.
pub const MAX_TOP: i64 = 100;
/// Comparison lookup fetch size.
pub const LOOKUP_LIMIT: i64 = 1000;
/// `robots_high` warning threshold.
pub const ROBOT_WARNING_PCT: f64 = 5.0;
/// Reporting API row numbering starts at 1.
pub const API_OFFSET: i64 = 1;

const WINDOW_MONTHS: [i64; 3] = [3, 6, 12];

/// `(document key, API metric)` — order is the KPI card order.
const SUMMARY_METRICS: [(&str, &str); 7] = [
    ("users", "ym:s:users"),
    ("visits", "ym:s:visits"),
    ("pageviews", "ym:s:pageviews"),
    ("page_depth", "ym:s:pageDepth"),
    ("bounce_rate", "ym:s:bounceRate"),
    ("avg_visit_duration_seconds", "ym:s:avgVisitDurationSeconds"),
    ("new_visitors_pct", "ym:s:percentNewVisitors"),
];

const REPEAT_METRIC: (&str, &str) = (
    "repeat_visitors_2_3_pct",
    "ym:s:upTo3VisitsPerUserPercentage",
);

const KPI_ORDER: [&str; 9] = [
    "users",
    "visits",
    "pageviews",
    "page_depth",
    "visits_per_day",
    "bounce_rate",
    "avg_visit_duration_seconds",
    "repeat_visitors_2_3_pct",
    "new_visitors_pct",
];

/// `(block key, dimension or @role, default row limit, uses the traffic filter)`.
const BREAKDOWNS: [(&str, &str, Option<i64>, bool); 9] = [
    ("search_engines", "@engine", Some(10), true),
    ("cities", "ym:s:regionCity", Some(10), true),
    ("countries", "ym:s:regionCountry", Some(10), true),
    ("devices", "ym:s:deviceCategory", Some(10), true),
    ("age", "ym:s:ageInterval", Some(10), true),
    ("gender", "ym:s:gender", Some(10), true),
    ("landing_pages", "ym:s:startURL", None, true),
    ("search_phrases", "@phrase", None, true),
    ("channels", "@source", Some(20), false),
];

/// Injected report call `(params, limit, offset)`; the live adapter maps it onto
/// [`crate::metrika::MetrikaClient::report`] with pagination off. The futures are
/// awaited on the caller's thread, so `Send` is not required of an adapter.
pub type ReportFuture<'a> = Pin<Box<dyn Future<Output = Result<Value, MetrikaFail>> + 'a>>;
/// The injected client seam: `(params, limit, offset)`.
pub type ReportCall<'a> = dyn Fn(Map<String, Value>, i64, i64) -> ReportFuture<'a> + 'a;
/// Injected Search Console fetch for `(date1, date2)`; returns the body or fails.
pub type GscFetch<'a> = dyn Fn(&str, &str) -> Result<Value, String> + 'a;

/// The keyword arguments of `build_traffic_document`.
pub struct TrafficInput<'a> {
    /// `counter_id` — int, `"1,2"` text, or a list.
    pub counter_id: Value,
    /// `date1` — a `YYYY-MM-DD` string.
    pub date1: Value,
    /// `date2` — a `YYYY-MM-DD` string.
    pub date2: Value,
    /// `attribution` — `last_significant` (default) or `last_click`.
    pub attribution: Value,
    /// `traffic` — `organic` (default) or `all`.
    pub traffic: Value,
    /// `lang` — `en` (default) or `ru`.
    pub lang: Value,
    /// `top` — rows shown per unbounded breakdown, `1..=100`.
    pub top: Value,
    /// `filters` — an optional Metrica filter expression.
    pub filters: Option<Value>,
    /// `site_label` — carried into the document as-is.
    pub site_label: Option<Value>,
    /// `gsc_rows` — supplied Search Console rows; `None` falls back to `gsc_fetch`.
    /// Kept as a raw value so the container is validated here; an object input is rejected.
    pub gsc_rows: Option<Value>,
    /// `gsc_fetch` — an optional injected Search Console fetcher.
    pub gsc_fetch: Option<&'a GscFetch<'a>>,
    /// `now` — injected epoch seconds for `generated_at`.
    pub now: Option<i64>,
}

/// `validate_request` — validate the collection arguments without collecting.
/// Callers validate before any credential or network work, in the same order
/// the document pipeline applies.
pub fn validate_traffic_input(input: &TrafficInput) -> Result<(), MetrikaFail> {
    validate_request(input).map(|_| ())
}

/// `build_traffic_document` — collect the traffic document for one period.
pub async fn build_traffic_document(
    input: &TrafficInput<'_>,
    client: &ReportCall<'_>,
) -> Result<Value, MetrikaFail> {
    let request = validate_request(input)?;
    let supplied_gsc = match input.gsc_rows.as_ref() {
        Some(rows) => Some(normalize_gsc_rows(rows)?),
        None => None,
    };
    let periods = comparison_periods(request.start, request.end);
    let mut collector = Collector::new(client, &request);

    let mut blocks = Map::new();
    blocks.insert(
        "summary".to_owned(),
        summary(&mut collector, &periods).await?,
    );
    if let Some(auth_error) = &collector.auth_error {
        return Ok(json!({
            "schema": SCHEMA,
            "ok": false,
            "error": auth_error,
            "counter_ids": request.ids,
            "requests": collector.requests,
        }));
    }
    let robots = robots(&mut collector, periods[0]).await?;
    blocks.insert("daily".to_owned(), daily(&mut collector, &periods).await?);
    blocks.insert(
        "windows".to_owned(),
        windows(&mut collector, request.end).await?,
    );
    for (key, spec, limit, filtered) in BREAKDOWNS {
        let limit = match limit {
            Some(0) | None => request.top,
            Some(limit) => limit,
        };
        blocks.insert(
            key.to_owned(),
            breakdown(&mut collector, &periods, spec, limit, filtered).await?,
        );
    }
    blocks.insert(
        "gsc_queries".to_owned(),
        gsc_block(supplied_gsc, input.gsc_fetch, periods[0])?,
    );

    let mut warnings = Vec::new();
    if robots.get("status") == Some(&json!("ok")) {
        let robot_pct = robots.get("robot_pct").and_then(Value::as_f64);
        if robot_pct.unwrap_or(0.0) >= ROBOT_WARNING_PCT {
            warnings.push(json!({
                "code": "robots_high",
                "value": num(robot_pct),
                "message": format!(
                    "Metrica classified {}% of visits as robots (above the warning \
                     threshold); human traffic may be overstated elsewhere",
                    robot_pct.map(pyjson_repr).unwrap_or_else(|| "null".to_owned())
                ),
            }));
        }
    }
    let summary_metrics = blocks["summary"]
        .get("metrics")
        .cloned()
        .unwrap_or(json!({}));
    let visits = summary_metrics
        .get("visits")
        .and_then(|entry| entry.get("value"))
        .and_then(Value::as_f64);
    if blocks["summary"].get("status") == Some(&json!("ok")) && !visits.is_some_and(|v| v != 0.0) {
        warnings.push(json!({
            "code": "no_visits",
            "message": "no visits matched the filter in this period; check the counter \
                        and dates",
        }));
    }
    let mut unavailable: Vec<String> = blocks
        .iter()
        .filter(|(_, block)| block.get("status") == Some(&json!("unavailable")))
        .map(|(key, _)| key.clone())
        .collect();
    unavailable.sort();
    if !unavailable.is_empty() {
        warnings.push(json!({
            "code": "blocks_unavailable",
            "blocks": unavailable,
            "message": "some report blocks could not be collected; each states its reason",
        }));
    }
    let ok = blocks["summary"].get("status") == Some(&json!("ok"));
    let mut document = Map::new();
    document.insert("schema".to_owned(), json!(SCHEMA));
    document.insert("ok".to_owned(), json!(ok));
    document.insert("source".to_owned(), json!(SOURCE));
    document.insert(
        "generated_at".to_owned(),
        json!(pytime::isoformat_seconds(
            input.now.unwrap_or_else(pytime::now_seconds) * 1_000_000
        )),
    );
    document.insert("counter_ids".to_owned(), json!(request.ids));
    document.insert(
        "site_label".to_owned(),
        input.site_label.clone().unwrap_or(Value::Null),
    );
    document.insert("lang".to_owned(), json!(request.lang));
    document.insert("period".to_owned(), span(periods[0]));
    document.insert(
        "comparisons".to_owned(),
        json!({
            "previous": span(periods[1]),
            "year_ago": span(periods[2]),
        }),
    );
    document.insert("attribution".to_owned(), json!(request.attribution));
    document.insert("traffic".to_owned(), json!(request.traffic));
    document.insert(
        "methodology".to_owned(),
        json!({
            "attribution_dimensions": {
                "source": collector.roles.0,
                "engine": collector.roles.1,
                "phrase": collector.roles.2,
            },
            "traffic_filter": collector.traffic_filter,
            "api_offset": API_OFFSET,
            "robots": robots,
            "requests": collector.requests,
            "lookup_limit": LOOKUP_LIMIT,
        }),
    );
    document.insert("warnings".to_owned(), Value::Array(warnings));
    // The summary's reason is read before `blocks` moves into the document.
    let error_value = match blocks["summary"].get("reason") {
        Some(reason) if pystr::truthy(Some(reason)) => reason.clone(),
        _ => json!("summary unavailable"),
    };
    document.insert("blocks".to_owned(), Value::Object(blocks));
    if !ok {
        document.insert("error".to_owned(), error_value);
    }
    Ok(Value::Object(document))
}

/// Float text in the robots warning, in shortest round-trip form.
fn pyjson_repr(value: f64) -> String {
    crate::pyjson::float_repr(value)
}

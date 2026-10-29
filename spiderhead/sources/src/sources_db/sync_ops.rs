//! `sources_db.sync` — fetch missing or incomplete days for one source/resource.

use std::path::Path;

use serde_json::{json, Map, Value};
use spiderhead_core::host::Host;

use super::fetchers::{
    fetch_ga4_result, fetch_gsc_result, fetch_metrika_result, webmaster_day, webmaster_history_op,
    webmaster_history_rows, FetcherFault, MetrikaFail as FetchMetrikaFail, WebmasterDay,
};
use super::{
    chunks, day_list, failure_state, invalid, mark_failed, mark_requested, requested_period,
    source_spec, store, SourcesError, RETRIES, RETRY_PAUSE,
};
use crate::ga4;
use crate::gsc;
use crate::http::{linear_pause, retry_slots, Transport};
use crate::metrika;
use crate::yandex_webmaster::{self, CollectArgs};
use crate::{pystr, pytime};

/// `Fetcher` — `(resource, start, end) -> {"ok","rows","complete",…}` or a fault.
pub type Fetcher<'a> = dyn FnMut(&str, &str, &str) -> Result<Value, FetcherFault> + 'a;

/// `sync`'s keyword arguments.
pub struct SyncArgs<'a> {
    /// First requested ISO date (`None` = `end - 27` days).
    pub start_date: Option<&'a str>,
    /// Last requested ISO date (`None` = the source's lag boundary).
    pub end_date: Option<&'a str>,
    /// Re-fetch days already marked complete.
    pub force: bool,
    /// Fixed clock as epoch microseconds; drives the lag boundary and every
    /// `fetched_at` / `last_attempt_at` stamp.
    pub now: Option<i64>,
}

fn stamp(now: Option<i64>) -> String {
    pytime::isoformat_seconds(now.unwrap_or_else(pytime::now_micros))
}

/// `_fetch_gsc` — one bounded daily `["date","query","page"]` grain through the GSC client.
async fn fetch_gsc<T: Transport>(
    host: &dyn Host,
    root: &Path,
    transport: &T,
    resource: &str,
    start: &str,
    end: &str,
) -> Result<Value, FetcherFault> {
    let args = gsc::PagesArgs {
        start_date: Some(start),
        end_date: Some(end),
        dimensions: Some(json!(["date", "query", "page"])),
        row_limit: 1_000,
        max_rows: gsc::MAX_ANALYTICS_ROWS,
        token: None,
        now: None,
        service_account: None,
    };
    let result = gsc::search_analytics_pages(host, root, resource, &args, transport)
        .await
        .map_err(|err| FetcherFault {
            class: err.python_class().to_owned(),
            message: err.to_string(),
        })?;
    fetch_gsc_result(&result)
}

/// `_fetch_ga4` — `run_report` over the session dims/mets, then the transform.
async fn fetch_ga4<T: Transport>(
    host: &dyn Host,
    root: &Path,
    transport: &T,
    resource: &str,
    start: &str,
    end: &str,
) -> Result<Value, FetcherFault> {
    let env = ga4::Ga4Env::live(host, root, transport);
    let args = ga4::ReportArgs {
        max_rows: None,
        token: None,
        transport: None,
    };
    let result = ga4::run_report(
        resource,
        start,
        end,
        &[
            "date",
            "landingPagePlusQueryString",
            "sessionDefaultChannelGroup",
        ],
        &["sessions", "engagedSessions", "totalUsers"],
        &args,
        &env,
    )
    .await
    .map_err(|err| FetcherFault {
        class: err.python_class().to_owned(),
        message: err.to_string(),
    })?;
    fetch_ga4_result(&result)
}

/// `_fetch_metrika`: a paginated report call (limit 1000), then the
/// `{date, traffic_source, landing_page, ...}` transform. `MissingCredential` and
/// `MetrikaError` map to envelope fields; any other failure propagates unchanged.
async fn fetch_metrika<T: Transport>(
    host: &dyn Host,
    root: &Path,
    transport: &T,
    resource: &str,
    start: &str,
    end: &str,
) -> Result<Value, FetcherFault> {
    let env = metrika::MetrikaEnv::live(host, root, transport);
    let params = json!({
        "ids": resource,
        "date1": start,
        "date2": end,
        "dimensions": "ym:s:date,ym:s:lastTrafficSource,ym:s:startURL",
        "metrics": "ym:s:visits,ym:s:users,ym:s:bounceRate",
    });
    let map_fail = |fail: metrika::MetrikaFail| -> Result<FetchMetrikaFail, FetcherFault> {
        match fail {
            metrika::MetrikaFail::Metrika(exc) => Ok(FetchMetrikaFail::Metrika {
                message: Box::new(json!(exc.message)),
                status: Box::new(json!(exc.status)),
            }),
            metrika::MetrikaFail::MissingCredential(message) => {
                Ok(FetchMetrikaFail::MissingCredential(message))
            }
            other => Err(FetcherFault {
                class: other.python_class().to_owned(),
                message: other.to_string(),
            }),
        }
    };
    let outcome = match metrika::MetrikaClient::new(&env, None) {
        Ok(client) => match client.report(&params, true, 1000, 1).await {
            Ok(body) => Ok(body),
            Err(fail) => Err(map_fail(fail)?),
        },
        Err(fail) => Err(map_fail(fail)?),
    };
    Ok(fetch_metrika_result(outcome))
}

/// `_fetch_webmaster` — per-day `search_performance` through the live
/// `yandex_webmaster.collect`, shaped by the shared `webmaster_day` step.
async fn fetch_webmaster<T: Transport>(
    host: &dyn Host,
    root: &Path,
    transport: &T,
    resource: &str,
    start: &str,
    end: &str,
) -> Result<Value, FetcherFault> {
    let env = yandex_webmaster::WmEnv::live(host, root, transport);
    let days = day_list(start, end).map_err(|err| FetcherFault {
        class: "ValueError".to_owned(),
        message: err.to_string(),
    })?;
    let mut rows = Vec::new();
    let mut complete = true;
    for day in days {
        let params = json!({
            "query_indicator": ["TOTAL_SHOWS", "TOTAL_CLICKS", "AVG_SHOW_POSITION", "AVG_CLICK_POSITION"],
            "date_from": day,
            "date_to": day,
        });
        let host_id = json!(resource);
        let args = yandex_webmaster::CollectArgs {
            host_id: Some(&host_id),
            params: Some(&params),
            paginate: Some(&json!(true)),
            ..CollectArgs::default()
        };
        let result = yandex_webmaster::collect("search_performance", &args, &env)
            .await
            .map_err(|err| FetcherFault {
                class: err.python_class().to_owned(),
                message: err.to_string(),
            })?;
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

/// `_fetch_webmaster_history` — `search_history`/`indexing_history`/`in_search_history`
/// through the live `yandex_webmaster.collect`, shaped by `webmaster_history_op`.
async fn fetch_webmaster_history<T: Transport>(
    host: &dyn Host,
    root: &Path,
    transport: &T,
    resource: &str,
    start: &str,
    end: &str,
) -> Result<Value, FetcherFault> {
    let env = yandex_webmaster::WmEnv::live(host, root, transport);
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
        let params = Value::Object(params);
        let host_id = json!(resource);
        let args = yandex_webmaster::CollectArgs {
            host_id: Some(&host_id),
            params: Some(&params),
            ..CollectArgs::default()
        };
        let result = yandex_webmaster::collect(operation, &args, &env)
            .await
            .map_err(|err| FetcherFault {
                class: err.python_class().to_owned(),
                message: err.to_string(),
            })?;
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

/// The adapter a source uses when no `fetcher` is injected. `gsc`/`ga4`/`metrika`/
/// `webmaster`/`webmaster_history` are wired; the other fetchers exist (in `fetchers`)
/// but their live clients are not wired yet. They surface as
/// `provider adapter raised AdapterUnavailable`, the same shape a provider call failure produces.
async fn default_fetch<T: Transport>(
    host: &dyn Host,
    root: &Path,
    transport: &T,
    source: &str,
    resource: &str,
    start: &str,
    end: &str,
) -> Result<Value, FetcherFault> {
    match source {
        "gsc" => fetch_gsc(host, root, transport, resource, start, end).await,
        "ga4" => fetch_ga4(host, root, transport, resource, start, end).await,
        "metrika" => fetch_metrika(host, root, transport, resource, start, end).await,
        "webmaster" => fetch_webmaster(host, root, transport, resource, start, end).await,
        "webmaster_history" => {
            fetch_webmaster_history(host, root, transport, resource, start, end).await
        }
        _ => Err(FetcherFault {
            class: "AdapterUnavailable".to_owned(),
            message: "adapter not wired".to_owned(),
        }),
    }
}

fn base_result(source: &str, resource: &str, error: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("ok".to_owned(), json!(false));
    out.insert("source".to_owned(), json!(source));
    out.insert("resource".to_owned(), json!(resource));
    out.insert("error".to_owned(), error.clone());
    out
}

/// The environment the built-in adapters need: credential resolution and
/// transport for the live path, plus a sleep substitute for retry backoff.
pub struct SyncEnv<'a, T: Transport> {
    /// Environment/DNS/IO seam.
    pub host: &'a dyn Host,
    /// Credentials root for token acquisition.
    pub root: &'a Path,
    /// HTTP transport.
    pub transport: &'a T,
    /// Sleep substitute for retry backoff (`None` sleeps for real).
    pub sleeper: Option<&'a dyn Fn(f64)>,
}

/// `sync` — fetch missing or incomplete days for one source/resource and store them.
///
/// `fetcher` replaces the built-in adapter for the source; `None` runs the built-in adapter.
pub async fn sync<T: Transport>(
    path: &Path,
    source: &str,
    resource: &Value,
    args: &SyncArgs<'_>,
    fetcher: Option<&mut Fetcher<'_>>,
    env: &SyncEnv<'_, T>,
) -> Result<Value, SourcesError> {
    let mut fetcher = fetcher;
    let spec = source_spec(source)?;
    // A missing or non-string resource is an invalid-input error, so the parameter arrives as a JSON value.
    let resource = match resource.as_str() {
        Some(text) if !text.is_empty() => text,
        _ => return invalid(format!("resource is required ({})", spec.resource_hint)),
    };
    let (start, end, available_end) =
        requested_period(source, args.start_date, args.end_date, args.now)?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let connection = super::connect(path, true)?;
    let requested = day_list(&start, &end)?;
    mark_requested(&connection, source, resource, &requested, &available_end)?;
    let available: Vec<String> = requested
        .iter()
        .filter(|day| day.as_str() <= available_end.as_str())
        .cloned()
        .collect();
    let lagged: Vec<String> = requested
        .iter()
        .filter(|day| day.as_str() > available_end.as_str())
        .cloned()
        .collect();
    let done: std::collections::BTreeSet<String> = connection
        .prepare(
            "SELECT date FROM sync_log WHERE source = ? AND resource = ? AND complete = 1 \
             AND date BETWEEN ? AND ?",
        )?
        .query_map(rusqlite::params![source, resource, start, end], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<_, _>>()?;
    let missing: Vec<String> = available
        .iter()
        .filter(|day| args.force || !done.contains(*day))
        .cloned()
        .collect();
    let sleep = |seconds: f64| match env.sleeper {
        Some(call) => call(seconds),
        None => std::thread::sleep(std::time::Duration::from_secs_f64(seconds)),
    };
    let (mut synced, mut attempted, mut rows_stored) = (0i64, 0i64, 0i64);
    let mut incomplete: Vec<String> = Vec::new();
    for chunk in chunks(&missing, spec.chunk_days) {
        attempted += chunk.len() as i64;
        let mut result = Value::Null;
        for slot in retry_slots(RETRIES + 1) {
            result = match fetcher.as_deref_mut() {
                Some(call) => call(resource, &chunk[0], &chunk[chunk.len() - 1]),
                None => {
                    default_fetch(
                        env.host,
                        env.root,
                        env.transport,
                        source,
                        resource,
                        &chunk[0],
                        &chunk[chunk.len() - 1],
                    )
                    .await
                }
            }
            .unwrap_or_else(|fault| {
                json!({
                    "ok": false,
                    "retry": false,
                    "error": format!("provider adapter raised {}", fault.class),
                })
            });
            if !result.is_object() {
                result = json!({
                    "ok": false,
                    "retry": false,
                    "error": "provider adapter returned an invalid result",
                });
            }
            if pystr::truthy(result.get("ok")) || result.get("retry") == Some(&Value::Bool(false)) {
                break;
            }
            if slot.has_next {
                sleep(linear_pause(slot.index, RETRY_PAUSE));
            }
        }
        if !pystr::truthy(result.get("ok")) {
            mark_failed(
                &connection,
                source,
                resource,
                &chunk,
                failure_state(&result),
                &stamp(args.now),
            )?;
            let mut out = base_result(
                source,
                resource,
                &if pystr::truthy(result.get("error")) {
                    result.get("error").cloned().unwrap_or_default()
                } else {
                    json!("provider request failed")
                },
            );
            out.insert(
                "failed_period".to_owned(),
                json!([chunk[0], chunk[chunk.len() - 1]]),
            );
            out.insert("synced_days".to_owned(), json!(synced));
            out.insert("attempted_days".to_owned(), json!(attempted));
            out.insert("rows_stored".to_owned(), json!(rows_stored));
            out.insert(
                "requested_period".to_owned(),
                json!([start.clone(), end.clone()]),
            );
            out.insert(
                "available_period".to_owned(),
                if available.is_empty() {
                    Value::Null
                } else {
                    json!([start.clone(), available_end.clone().min(end.clone())])
                },
            );
            out.insert("lagged_days".to_owned(), json!(lagged));
            out.insert("db".to_owned(), json!(super::py_path_str(path)));
            return Ok(Value::Object(out));
        }
        let rows_ok = result.get("rows").map(Value::is_array).unwrap_or(false)
            && result
                .get("complete")
                .map(Value::is_boolean)
                .unwrap_or(false);
        if !rows_ok {
            mark_failed(
                &connection,
                source,
                resource,
                &chunk,
                "failed",
                &stamp(args.now),
            )?;
            let mut out = base_result(
                source,
                resource,
                &json!("provider fetcher must return rows list and complete boolean"),
            );
            out.insert(
                "failed_period".to_owned(),
                json!([chunk[0], chunk[chunk.len() - 1]]),
            );
            out.insert("synced_days".to_owned(), json!(synced));
            out.insert("attempted_days".to_owned(), json!(attempted));
            out.insert("rows_stored".to_owned(), json!(rows_stored));
            out.insert("db".to_owned(), json!(super::py_path_str(path)));
            return Ok(Value::Object(out));
        }
        let stored = match store(
            &connection,
            source,
            resource,
            &chunk,
            result["rows"].as_array().map(Vec::as_slice).unwrap_or(&[]),
            result["complete"].as_bool().unwrap_or(false),
            &stamp(args.now),
        ) {
            Ok(count) => count,
            Err(SourcesError::Invalid(message)) => {
                mark_failed(
                    &connection,
                    source,
                    resource,
                    &chunk,
                    "failed",
                    &stamp(args.now),
                )?;
                let mut out = base_result(source, resource, &json!(message));
                out.insert(
                    "failed_period".to_owned(),
                    json!([chunk[0], chunk[chunk.len() - 1]]),
                );
                out.insert("synced_days".to_owned(), json!(synced));
                out.insert("attempted_days".to_owned(), json!(attempted));
                out.insert("rows_stored".to_owned(), json!(rows_stored));
                out.insert("db".to_owned(), json!(super::py_path_str(path)));
                return Ok(Value::Object(out));
            }
            Err(other) => return Err(other),
        };
        rows_stored += stored;
        if result["complete"].as_bool().unwrap_or(false) {
            synced += chunk.len() as i64;
        } else {
            incomplete.extend(chunk.iter().cloned());
        }
    }
    let mut out = Map::new();
    out.insert("ok".to_owned(), json!(true));
    out.insert("source".to_owned(), json!(source));
    out.insert("resource".to_owned(), json!(resource));
    out.insert("period".to_owned(), json!([start.clone(), end.clone()]));
    out.insert(
        "requested_period".to_owned(),
        json!([start.clone(), end.clone()]),
    );
    out.insert(
        "available_period".to_owned(),
        if available.is_empty() {
            Value::Null
        } else {
            json!([start, end.min(available_end)])
        },
    );
    out.insert("lagged_days".to_owned(), json!(lagged));
    out.insert(
        "already_synced_days".to_owned(),
        json!(if args.force { 0 } else { done.len() as i64 }),
    );
    out.insert("synced_days".to_owned(), json!(synced));
    out.insert("attempted_days".to_owned(), json!(attempted));
    out.insert("rows_stored".to_owned(), json!(rows_stored));
    out.insert("incomplete_days".to_owned(), json!(incomplete));
    out.insert("db".to_owned(), json!(super::py_path_str(path)));
    Ok(Value::Object(out))
}

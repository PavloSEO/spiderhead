//! Replay `sources_db_oracle.json` — reference `sources_db` driven by scripted ops
//! (`db_path`/`connect`/`exec_sql`/`write_file`/`available_end`/`sync`/`status`/
//! `query`/`fetch_*`) with injected fetchers and provider seams, a frozen `now`,
//! recorded `time.sleep` pauses and watched-file snapshots. Paths normalize to `$BASE`.
//!

mod common;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::Path;
use std::rc::Rc;

use rusqlite::Connection;
use serde_json::{json, Map, Value};

use spiderhead_sources::sources_db::{self, fetchers, QueryArgs, SyncArgs, SyncEnv};
use spiderhead_sources::{gsc, pystr, pytime};

use common::StubTransport;

/// The frozen `sources_db.datetime` — 2026-02-10T12:00:00+00:00 in epoch microseconds.
fn frozen_micros() -> i64 {
    pytime::days_from_civil(2026, 2, 10) * 86_400_000_000 + 12 * 3_600_000_000
}

/// `$BASE` → the scenario sandbox path inside op arguments.
fn expand_base(value: &Value, base: &Path) -> Value {
    let text = serde_json::to_string(value).unwrap_or_default();
    serde_json::from_str(&text.replace("$BASE", &base.to_string_lossy()))
        .unwrap_or_else(|_| value.clone())
}

/// Replace the sandbox prefix with `$BASE` inside every recorded string.
fn scrub_base(value: &mut Value, base: &str) {
    match value {
        Value::String(text) => *text = text.replace(base, "$BASE"),
        Value::Array(items) => items.iter_mut().for_each(|item| scrub_base(item, base)),
        Value::Object(map) => map.values_mut().for_each(|item| scrub_base(item, base)),
        _ => {}
    }
}

fn scrub_text(text: &str, base: &str) -> String {
    text.replace(base, "$BASE")
}

/// `{result: X}` — with the sandbox prefix scrubbed to `$BASE` throughout.
fn outcome_result<T: serde::Serialize>(value: T, base: &str) -> Value {
    let mut value = serde_json::to_value(value).unwrap_or_default();
    scrub_base(&mut value, base);
    json!({"result": value})
}

fn outcome_error(class: &str, message: &str, base: &str) -> Value {
    json!({"error": {"class": class, "message": scrub_text(message, base)}})
}

/// A scripted provider/fetcher result queue — `{"$raise": …}` entries become faults.
struct Script {
    results: VecDeque<Value>,
}

impl Script {
    fn new(results: &[Value]) -> Self {
        Script {
            results: results.iter().cloned().collect(),
        }
    }

    fn pop(&mut self) -> Result<Value, fetchers::FetcherFault> {
        let entry = self
            .results
            .pop_front()
            .unwrap_or_else(|| json!({"ok": false, "error": "stub: out of results"}));
        if let Some(marker) = entry.get("$raise") {
            return Err(fetchers::FetcherFault {
                class: marker
                    .get("class")
                    .and_then(Value::as_str)
                    .unwrap_or("Exception")
                    .to_owned(),
                message: marker
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            });
        }
        Ok(entry)
    }
}

/// `{"args": …, "kwargs": …, "tag": …}` — one recorded fetcher/provider call.
fn record_call(calls: &Rc<RefCell<Vec<Value>>>, args: Value, kwargs: Value, tag: &str) {
    let mut call = Map::new();
    call.insert("args".to_owned(), args);
    call.insert("kwargs".to_owned(), kwargs);
    call.insert("tag".to_owned(), json!(tag));
    calls.borrow_mut().push(Value::Object(call));
}

/// The `wm.collect` seam — `(operation, host_id, params, paginate)` per reference
/// `_webmaster`; the recorded kwargs carry `paginate` only when it was passed.
fn webmaster_collect<'a>(
    results: &'a [Value],
    calls: Rc<RefCell<Vec<Value>>>,
) -> Box<fetchers::WebmasterCall<'a>> {
    let mut script = Script::new(results);
    Box::new(move |operation, host, params, paginate| {
        record_call(
            &calls,
            json!([operation, host, params.clone()]),
            if paginate {
                json!({"paginate": true})
            } else {
                json!({})
            },
            "webmaster",
        );
        script.pop()
    })
}

/// `MetrikaClient().report` — pops `{"body"|"missing_credential"|"metrika_error"}`.
fn metrika_report(
    script: &mut Script,
    calls: &Rc<RefCell<Vec<Value>>>,
    resource: &str,
    start: &str,
    end: &str,
) -> Result<Value, fetchers::MetrikaFail> {
    let mut call = Map::new();
    call.insert("tag".to_owned(), json!("report"));
    call.insert(
        "params".to_owned(),
        json!({
            "ids": resource,
            "date1": start,
            "date2": end,
            "dimensions": "ym:s:date,ym:s:lastTrafficSource,ym:s:startURL",
            "metrics": "ym:s:visits,ym:s:users,ym:s:bounceRate",
        }),
    );
    call.insert("paginate".to_owned(), json!(true));
    call.insert("limit".to_owned(), json!(1000));
    calls.borrow_mut().push(Value::Object(call));
    let entry = script.pop().unwrap_or_else(|_| json!({}));
    match entry {
        Value::Object(map) if map.contains_key("missing_credential") => {
            Err(fetchers::MetrikaFail::MissingCredential(
                map["missing_credential"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            ))
        }
        Value::Object(map) if map.contains_key("metrika_error") => {
            let error = &map["metrika_error"];
            Err(fetchers::MetrikaFail::Metrika {
                message: Box::new(error.get("message").cloned().unwrap_or_default()),
                status: Box::new(error.get("status").cloned().unwrap_or_default()),
            })
        }
        Value::Object(mut map) => Ok(map.remove("body").unwrap_or_default()),
        other => Ok(other),
    }
}

/// Build the `Fetcher` closure a `sync` op needs; provider kinds wrap the module-level
/// adapters so the sync → fetcher → provider-seam chain is exercised end to end.
fn build_fetcher<'a>(
    kind: &str,
    results: &'a [Value],
    calls: Rc<RefCell<Vec<Value>>>,
) -> Box<sources_db::Fetcher<'a>> {
    match kind {
        "script" => {
            let mut script = Script::new(results);
            Box::new(move |resource, start, end| {
                record_call(&calls, json!([resource, start, end]), json!({}), "fetch");
                script.pop()
            })
        }
        "gsc" => {
            let mut script = Script::new(results);
            Box::new(move |resource, start, end| {
                record_call(
                    &calls,
                    json!([resource]),
                    json!({
                        "start_date": start,
                        "end_date": end,
                        "dimensions": ["date", "query", "page"],
                        "row_limit": 1000,
                        "max_rows": gsc::MAX_ANALYTICS_ROWS,
                    }),
                    "analytics",
                );
                script.pop().and_then(|v| fetchers::fetch_gsc_result(&v))
            })
        }
        "ga4" => {
            let mut script = Script::new(results);
            Box::new(move |resource, start, end| {
                record_call(
                    &calls,
                    json!([
                        resource,
                        start,
                        end,
                        [
                            "date",
                            "landingPagePlusQueryString",
                            "sessionDefaultChannelGroup"
                        ],
                        ["sessions", "engagedSessions", "totalUsers"]
                    ]),
                    json!({}),
                    "report",
                );
                script.pop().and_then(|v| fetchers::fetch_ga4_result(&v))
            })
        }
        "metrika" => {
            calls.borrow_mut().push(json!({"tag": "metrika"}));
            let mut script = Script::new(results);
            Box::new(move |resource, start, end| {
                let report = metrika_report(&mut script, &calls, resource, start, end);
                Ok(fetchers::fetch_metrika_result(report))
            })
        }
        "webmaster" => {
            let mut collect = webmaster_collect(results, calls);
            Box::new(move |resource, start, end| {
                fetchers::fetch_webmaster(&mut collect, resource, start, end)
            })
        }
        "webmaster_history" => {
            let mut collect = webmaster_collect(results, calls);
            Box::new(move |resource, start, end| {
                fetchers::fetch_webmaster_history(&mut collect, resource, start, end)
            })
        }
        other => panic!("unknown fetcher kind {other}"),
    }
}

/// `{exists, mode, size|content, path}` — the watched-file snapshot.
fn file_snapshot(path: &Path, base: &str) -> Value {
    let mut entry = Map::new();
    entry.insert("exists".to_owned(), json!(path.is_file()));
    if let Ok(metadata) = std::fs::metadata(path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            entry.insert(
                "mode".to_owned(),
                json!(format!("0o{:o}", metadata.permissions().mode() & 0o777)),
            );
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default();
        if ["csv", "json", "txt"].contains(&ext) {
            entry.insert(
                "content".to_owned(),
                json!(std::fs::read_to_string(path).unwrap_or_default()),
            );
        } else {
            entry.insert("size".to_owned(), json!(metadata.len()));
        }
    }
    entry.insert(
        "path".to_owned(),
        json!(scrub_text(&path.to_string_lossy(), base)),
    );
    Value::Object(entry)
}

/// `exec_sql` parameters — JSON values as rusqlite bind params.
fn sql_params(params: &[Value]) -> Vec<rusqlite::types::Value> {
    params
        .iter()
        .map(|value| match value {
            Value::Null => rusqlite::types::Value::Null,
            Value::Bool(b) => rusqlite::types::Value::Integer(i64::from(*b)),
            Value::Number(n) => n
                .as_i64()
                .map(rusqlite::types::Value::Integer)
                .unwrap_or_else(|| rusqlite::types::Value::Real(n.as_f64().unwrap_or_default())),
            Value::String(s) => rusqlite::types::Value::Text(s.clone()),
            other => rusqlite::types::Value::Text(pystr::repr_value(other)),
        })
        .collect()
}

fn map_sources_error(err: &sources_db::SourcesError, base: &str) -> Value {
    outcome_error(err.python_class(), &err.to_string(), base)
}

fn map_fetcher_fault(err: &fetchers::FetcherFault, base: &str) -> Value {
    outcome_error(&err.class, &err.message, base)
}

/// Replay one op; returns `{out, calls, sleeps, files}` in the fixture's record shape.
async fn run_op(
    base: &Path,
    db_name: &str,
    op: &Value,
    transport: &StubTransport,
    host: &spiderhead_core::host::FakeHost,
    sleeps: &Rc<RefCell<Vec<f64>>>,
    calls: Rc<RefCell<Vec<Value>>>,
) -> Value {
    let db = base.join(db_name);
    let base_str = base.to_string_lossy().to_string();
    let args = expand_base(op, base);
    let name = args["op"].as_str().unwrap_or_default();
    let out = match name {
        "db_path" => match sources_db::db_path(
            args.get("db").and_then(Value::as_str),
            args.get("project").and_then(Value::as_str),
        ) {
            Ok(path) => outcome_result(path.to_string_lossy().into_owned(), &base_str),
            Err(err) => map_sources_error(&err, &base_str),
        },
        "connect" => match sources_db::connect(&db, args["create"].as_bool().unwrap_or(false)) {
            Ok(connection) => {
                drop(connection);
                outcome_result(json!({"ok": true}), &base_str)
            }
            Err(err) => map_sources_error(&err, &base_str),
        },
        "exec_sql" => {
            let outcome: Result<Value, sources_db::SourcesError> = (|| {
                let connection = Connection::open(&db)?;
                for statement in args["sql"].as_array().cloned().unwrap_or_default() {
                    match statement {
                        Value::Array(pair) => {
                            let sql = pair[0].as_str().unwrap_or_default().to_owned();
                            let params = sql_params(
                                pair.get(1)
                                    .and_then(Value::as_array)
                                    .map(Vec::as_slice)
                                    .unwrap_or(&[]),
                            );
                            connection.execute(&sql, rusqlite::params_from_iter(params.iter()))?;
                        }
                        Value::String(sql) => {
                            connection.execute(&sql, [])?;
                        }
                        _ => {}
                    }
                }
                Ok(json!({"ok": true}))
            })();
            match outcome {
                Ok(value) => outcome_result(value, &base_str),
                Err(err) => map_sources_error(&err, &base_str),
            }
        }
        "write_file" => {
            let outcome: Result<Value, std::io::Error> = (|| {
                let target = base.join(args["path"].as_str().unwrap_or_default());
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&target, args["content"].as_str().unwrap_or_default())?;
                Ok(json!({"ok": true}))
            })();
            match outcome {
                Ok(value) => outcome_result(value, &base_str),
                Err(err) => outcome_error("OSError", &err.to_string(), &base_str),
            }
        }
        "available_end" => {
            let as_of = args
                .get("as_of")
                .and_then(Value::as_str)
                .and_then(pytime::date_fromisoformat);
            match sources_db::available_end(
                args["source"].as_str().unwrap_or_default(),
                as_of,
                Some(frozen_micros()),
            ) {
                Ok(days) => {
                    let (y, m, d) = pytime::civil_from_days(days);
                    outcome_result(format!("{y:04}-{m:02}-{d:02}"), &base_str)
                }
                Err(err) => map_sources_error(&err, &base_str),
            }
        }
        "sync" => {
            let mut fetcher = args.get("fetcher").map(|spec| {
                build_fetcher(
                    spec["kind"].as_str().unwrap_or_default(),
                    spec["results"].as_array().map(Vec::as_slice).unwrap_or(&[]),
                    calls.clone(),
                )
            });
            let sync_args = SyncArgs {
                start_date: args.get("start_date").and_then(Value::as_str),
                end_date: args.get("end_date").and_then(Value::as_str),
                force: args.get("force").and_then(Value::as_bool).unwrap_or(false),
                now: Some(frozen_micros()),
            };
            let sleep_log = sleeps.clone();
            let sleep_fn = move |seconds: f64| sleep_log.borrow_mut().push(seconds);
            let env = SyncEnv {
                host,
                root: base,
                transport,
                sleeper: Some(&sleep_fn),
            };
            match sources_db::sync(
                &db,
                args["source"].as_str().unwrap_or_default(),
                &args["resource"],
                &sync_args,
                fetcher.as_deref_mut(),
                &env,
            )
            .await
            {
                Ok(value) => outcome_result(value, &base_str),
                Err(err) => map_sources_error(&err, &base_str),
            }
        }
        "status" => match sources_db::status(&db) {
            Ok(value) => outcome_result(value, &base_str),
            Err(err) => map_sources_error(&err, &base_str),
        },
        "query" => {
            let out_path = args
                .get("out")
                .and_then(Value::as_str)
                .map(|name| base.join(name).to_string_lossy().into_owned());
            let query_args = QueryArgs {
                resource: args.get("resource").and_then(Value::as_str),
                start_date: args.get("start_date").and_then(Value::as_str),
                end_date: args.get("end_date").and_then(Value::as_str),
                match_text: args.get("match").and_then(Value::as_str),
                limit: args.get("limit").and_then(Value::as_i64).unwrap_or(1000),
                out: out_path.as_deref(),
            };
            match sources_db::query(
                &db,
                args["source"].as_str().unwrap_or_default(),
                &query_args,
            ) {
                Ok(value) => outcome_result(value, &base_str),
                Err(err) => map_sources_error(&err, &base_str),
            }
        }
        "fetch_gsc"
        | "fetch_ga4"
        | "fetch_metrika"
        | "fetch_webmaster"
        | "fetch_webmaster_history" => {
            let provider = args["provider"].as_array().cloned().unwrap_or_default();
            let resource = args["resource"].as_str().unwrap_or_default();
            let start = args["start"].as_str().unwrap_or_default();
            let end = args["end"].as_str().unwrap_or_default();
            let result: Result<Value, fetchers::FetcherFault> = match name {
                "fetch_gsc" => {
                    let mut script = Script::new(&provider);
                    record_call(
                        &calls,
                        json!([resource]),
                        json!({
                            "start_date": start,
                            "end_date": end,
                            "dimensions": ["date", "query", "page"],
                            "row_limit": 1000,
                            "max_rows": gsc::MAX_ANALYTICS_ROWS,
                        }),
                        "analytics",
                    );
                    script.pop().and_then(|v| fetchers::fetch_gsc_result(&v))
                }
                "fetch_ga4" => {
                    let mut script = Script::new(&provider);
                    record_call(
                        &calls,
                        json!([
                            resource,
                            start,
                            end,
                            [
                                "date",
                                "landingPagePlusQueryString",
                                "sessionDefaultChannelGroup"
                            ],
                            ["sessions", "engagedSessions", "totalUsers"]
                        ]),
                        json!({}),
                        "report",
                    );
                    script.pop().and_then(|v| fetchers::fetch_ga4_result(&v))
                }
                "fetch_metrika" => {
                    calls.borrow_mut().push(json!({"tag": "metrika"}));
                    let mut script = Script::new(&provider);
                    let report = metrika_report(&mut script, &calls, resource, start, end);
                    Ok(fetchers::fetch_metrika_result(report))
                }
                "fetch_webmaster" => {
                    let mut collect = webmaster_collect(&provider, calls.clone());
                    fetchers::fetch_webmaster(&mut collect, resource, start, end)
                }
                _ => {
                    let mut collect = webmaster_collect(&provider, calls.clone());
                    fetchers::fetch_webmaster_history(&mut collect, resource, start, end)
                }
            };
            match result {
                Ok(value) => outcome_result(value, &base_str),
                Err(err) => map_fetcher_fault(&err, &base_str),
            }
        }
        other => panic!("unknown op {other}"),
    };
    let mut files = Map::new();
    let mut watched = vec![db.clone()];
    for path in args
        .get("watch")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        if let Some(text) = path.as_str() {
            watched.push(base.join(text));
        }
    }
    for path in watched {
        files.insert(
            scrub_text(&path.to_string_lossy(), &base_str),
            file_snapshot(&path, &base_str),
        );
    }
    json!({
        "out": out,
        "files": files,
        "calls": std::mem::take(&mut *calls.borrow_mut()),
        "sleeps": std::mem::take(&mut *sleeps.borrow_mut()),
    })
}

fn assert_json_eq(actual: &Value, expected: &Value, context: &str) {
    if actual != expected {
        panic!(
            "scenario mismatch at {context}:\n  actual:   {}\n  expected: {}",
            serde_json::to_string_pretty(actual).unwrap_or_default(),
            serde_json::to_string_pretty(expected).unwrap_or_default(),
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn sources_db_oracle() {
    let path = common::fixtures_dir().join("sources_db_oracle.json");
    let fixture: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let dir = common::fixtures_dir().join("tmp_sources_db");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut compared = 0usize;
    for (index, scenario) in fixture["scenarios"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let base = dir.join(format!("s{index}"));
        std::fs::create_dir_all(&base).unwrap();
        let transport = StubTransport::new(&[]);
        let host = spiderhead_core::host::FakeHost::default();
        let sleeps = Rc::new(RefCell::new(Vec::new()));
        let calls = Rc::new(RefCell::new(Vec::new()));
        let db_name = scenario
            .get("db")
            .and_then(Value::as_str)
            .unwrap_or("sources.sqlite");
        for (op_index, op) in scenario["ops"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            let expected = &scenario["ops"][op_index]["record"];
            let actual = run_op(
                &base,
                db_name,
                op,
                &transport,
                &host,
                &sleeps,
                calls.clone(),
            )
            .await;
            let context = format!(
                "{}.ops[{op_index}]",
                scenario["name"].as_str().unwrap_or("?")
            );
            for key in ["out", "calls", "sleeps", "files"] {
                assert_json_eq(&actual[key], &expected[key], &format!("{context}.{key}"));
            }
            compared += 1;
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!("sources_db oracle: {compared} ops compared, 0 diffs");
}

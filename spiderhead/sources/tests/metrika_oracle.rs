//! Oracle replay for `gen/gen_metrika.py`'s fixture — `metrika.rs` client calls
//! driven with a scripted `open_no_redirect` queue plus `metrika_traffic.rs`
//! document builds against a scripted `client.report` seam. Every op replays the
//! recorded calls/sleeps/spends/zone probes and diffs the outcome field-wise.

mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};
use spiderhead_core::host::FakeHost;
use spiderhead_sources::http::{Request, Response, Sleeper, Transport, TransportError};
use spiderhead_sources::metrika::{
    self, MetrikaClient, MetrikaEnv, MetrikaError, MetrikaFail, SpendSink, ZoneinfoProbe, ROW_CAP,
};
use spiderhead_sources::metrika_traffic::{self, GscFetch, ReportCall, TrafficInput};
use spiderhead_sources::spend::Record;

use common::{absent_config_root, fixtures_dir};

const DEFAULT_NOW: i64 = 1_789_000_000;

/// The `open_no_redirect` stub: records `{method, url, auth, timeout}` like the
/// generator's `make_transport`, then resolves the scripted queue entry.
struct QueueTransport {
    queue: Mutex<VecDeque<Value>>,
    calls: Mutex<Vec<Value>>,
}

impl QueueTransport {
    fn new(queue: &Value) -> Self {
        QueueTransport {
            queue: Mutex::new(
                queue
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .collect(),
            ),
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl Transport for QueueTransport {
    async fn send(&self, request: &Request) -> Result<Response, TransportError> {
        let timeout = if request.timeout.fract() == 0.0 {
            json!(request.timeout as i64)
        } else {
            json!(request.timeout)
        };
        let auth = request
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            .map(|(_, value)| json!(value))
            .unwrap_or(Value::Null);
        self.calls.lock().unwrap().push(json!({
            "method": request.method,
            "url": request.url,
            "auth": auth,
            "timeout": timeout,
        }));
        let entry =
            self.queue.lock().unwrap().pop_front().unwrap_or_else(
                || json!({"http": {"code": 500, "text": "stub: out of transport"}}),
            );
        if let Some(body) = entry.get("body") {
            return Ok(Response {
                status: 200,
                headers: Vec::new(),
                body: serde_json::to_vec(body).unwrap_or_default(),
            });
        }
        if let Some(text) = entry.get("text").and_then(Value::as_str) {
            return Ok(Response {
                status: 200,
                headers: Vec::new(),
                body: text.as_bytes().to_vec(),
            });
        }
        if let Some(bytes) = entry.get("bytes").and_then(Value::as_array) {
            return Ok(Response {
                status: 200,
                headers: Vec::new(),
                body: bytes
                    .iter()
                    .filter_map(|b| b.as_u64().map(|v| v as u8))
                    .collect(),
            });
        }
        if let Some(spec) = entry.get("http") {
            let mut headers = Vec::new();
            if let Some(retry) = spec.get("retry_after") {
                headers.push((
                    "retry-after".to_owned(),
                    retry.as_str().unwrap_or_default().to_owned(),
                ));
            }
            let text = spec.get("text").and_then(Value::as_str).unwrap_or_default();
            return Err(TransportError::Http(Response {
                status: spec.get("code").and_then(Value::as_u64).unwrap_or(500) as u16,
                headers,
                body: text.as_bytes().to_vec(),
            }));
        }
        if let Some(msg) = entry.get("url_error").and_then(Value::as_str) {
            // `URLError(msg)` → `str` is `<urlopen error msg>` — `Connect` carries
            // the reason and `pyerr::url_error` adds the urllib wrapper.
            return Err(TransportError::Connect(msg.to_owned()));
        }
        if entry.get("timeout").is_some() {
            // `TimeoutError(msg)` → `str` is `msg`; the fixture only scripts
            // `"timed out"`, which `pyerr::url_error("timeout")` reproduces.
            return Err(TransportError::Connect("timeout".to_owned()));
        }
        panic!("unknown transport entry {entry}");
    }
}

/// `spend.record` kwargs as JSON — Python passes ints for `cost`/`items`.
fn spend_json(rec: &Record) -> Value {
    let mut m = Map::new();
    m.insert("source".to_owned(), json!(rec.source));
    m.insert("operation".to_owned(), json!(rec.operation));
    if let Some(cost) = rec.cost {
        m.insert(
            "cost".to_owned(),
            if cost.fract() == 0.0 {
                json!(cost as i64)
            } else {
                json!(cost)
            },
        );
    }
    m.insert("unit".to_owned(), json!(rec.unit));
    if let Some(task) = &rec.task_id {
        m.insert("task_id".to_owned(), task.clone());
    }
    if let Some(items) = rec.items {
        m.insert("items".to_owned(), json!(items));
    }
    if let Some(extra) = &rec.extra {
        m.insert("extra".to_owned(), Value::Object(extra.clone()));
    }
    if let Some(at) = &rec.at {
        m.insert("at".to_owned(), json!(at));
    }
    Value::Object(m)
}

/// `{result}`/`{error}` exactly like the generator's `outcome()`.
fn outcome(result: Result<Value, MetrikaFail>) -> Value {
    match result {
        Ok(value) => json!({"result": value}),
        Err(fail) => json!({"error": {
            "class": fail.python_class(),
            "message": fail.to_string(),
        }}),
    }
}

/// One `client`-kind op: transport/env seams plus the recorded observation fields.
async fn run_client_op(op: &Value) -> Value {
    let env_spec = op.get("env").cloned().unwrap_or_else(|| json!({}));
    let transport = QueueTransport::new(op.get("queue").unwrap_or(&Value::Null));
    let (sleeper, pauses) = Sleeper::recording();
    let spends = Arc::new(Mutex::new(Vec::<Value>::new()));
    let zone_probes = Arc::new(Mutex::new(Vec::<Value>::new()));
    let zones = env_spec.get("zones").cloned().unwrap_or_else(|| json!({}));

    let spend_impl = {
        let spends = spends.clone();
        move |rec: &Record| {
            spends.lock().unwrap().push(spend_json(rec));
            Ok(json!({"ok": true}))
        }
    };
    let spend_sink: &SpendSink = &spend_impl;
    let probe_impl = {
        let zone_probes = zone_probes.clone();
        move |name: &str| {
            zone_probes.lock().unwrap().push(json!(name));
            zones.get(name).and_then(Value::as_i64)
        }
    };
    let probe: &ZoneinfoProbe = &probe_impl;
    let host = FakeHost::default();
    let root = absent_config_root();
    let env = MetrikaEnv {
        host: &host,
        root: &root,
        transport: &transport,
        sleeper: &sleeper,
        spend: Some(spend_sink),
        now: Some(
            env_spec
                .get("now")
                .and_then(Value::as_i64)
                .unwrap_or(DEFAULT_NOW),
        ),
        zoneinfo: Some(probe),
        row_cap: env_spec
            .get("row_cap")
            .and_then(Value::as_i64)
            .unwrap_or(ROW_CAP),
    };

    let call = op.get("call").cloned().unwrap_or_else(|| json!({}));
    let fn_name = call.get("fn").and_then(Value::as_str).unwrap_or_default();
    let args = call.get("args").cloned().unwrap_or_else(|| json!({}));

    // `rows_to_records` runs without a client in the generator.
    let result = if fn_name == "rows_to_records" {
        metrika::rows_to_records(args.get("report").unwrap_or(&Value::Null)).map(Value::Array)
    } else {
        // `MetrikaClient(token=...)` — falsy tokens fall through to the patched
        // `metrika_token`, which the env scripts as ok/missing/broken.
        let client = match op.get("client_token") {
            Some(Value::String(token)) if !token.is_empty() => {
                MetrikaClient::new(&env, Some(token))
            }
            _ => match env_spec.get("token") {
                Some(spec) if spec.get("missing").is_some() => Err(MetrikaFail::MissingCredential(
                    spec.get("missing")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                )),
                Some(spec) if spec.get("oserror").is_some() => Err(MetrikaFail::Python {
                    class: "OSError".to_owned(),
                    message: spec
                        .get("oserror")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                }),
                spec => MetrikaClient::new(
                    &env,
                    spec.and_then(|s| s.get("token"))
                        .and_then(Value::as_str)
                        .or(Some("synthetic-token")),
                ),
            },
        };
        match client {
            Err(fail) => Err(fail),
            Ok(client) => {
                let arg = |key: &str| args.get(key).cloned().unwrap_or(Value::Null);
                let int_arg = |key: &str, default: i64| {
                    args.get(key).and_then(Value::as_i64).unwrap_or(default)
                };
                match fn_name {
                    "counters" => client.counters().await,
                    _ => {
                        let counter_id = arg("counter_id");
                        let request_id = arg("request_id");
                        let part = arg("part");
                        let params = arg("params");
                        let source = arg("source");
                        let date1 = arg("date1");
                        let date2 = arg("date2");
                        let fields = arg("fields");
                        match fn_name {
                            "counter" => client.counter(&counter_id).await,
                            "goals" => client.goals(&counter_id).await,
                            "filters" => client.filters(&counter_id).await,
                            "operations" => client.operations(&counter_id).await,
                            "report" => {
                                client
                                    .report(
                                        &params,
                                        spiderhead_sources::pystr::truthy(args.get("paginate")),
                                        int_arg("limit", 100),
                                        int_arg("offset", 1),
                                    )
                                    .await
                            }
                            "by_time" => {
                                client
                                    .by_time(&params, int_arg("limit", 100), int_arg("offset", 1))
                                    .await
                            }
                            "create_log_request" => {
                                client
                                    .create_log_request(
                                        &counter_id,
                                        source.as_str().unwrap_or_default(),
                                        date1.as_str().unwrap_or_default(),
                                        date2.as_str().unwrap_or_default(),
                                        &fields,
                                    )
                                    .await
                            }
                            "log_requests" => client.log_requests(&counter_id).await,
                            "log_request" => client.log_request(&counter_id, &request_id).await,
                            "download_log_part" => client
                                .download_log_part(&counter_id, &request_id, &part)
                                .await
                                .map(Value::String),
                            "cancel_log_request" => {
                                client.cancel_log_request(&counter_id, &request_id).await
                            }
                            other => panic!("unknown client fn {other}"),
                        }
                    }
                }
            }
        }
    };

    let mut record = outcome(result);
    let record = record.as_object_mut().unwrap();
    record.insert(
        "calls".to_owned(),
        Value::Array(transport.calls.lock().unwrap().clone()),
    );
    record.insert("sleeps".to_owned(), json!(*pauses.lock().unwrap()));
    record.insert(
        "spends".to_owned(),
        Value::Array(spends.lock().unwrap().clone()),
    );
    record.insert(
        "zone_probes".to_owned(),
        Value::Array(zone_probes.lock().unwrap().clone()),
    );
    record.insert("gsc_calls".to_owned(), json!([]));
    Value::Object(record.clone())
}

/// One `traffic`-kind op: a scripted `client.report` queue and `gsc_fetch`.
async fn run_traffic_op(op: &Value) -> Value {
    let calls = Arc::new(Mutex::new(Vec::<Value>::new()));
    let gsc_calls = Arc::new(Mutex::new(Vec::<Value>::new()));
    let queue = Arc::new(Mutex::new(
        op.get("client_queue")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect::<VecDeque<Value>>(),
    ));

    let report_impl = {
        let calls = calls.clone();
        let queue = queue.clone();
        move |params: Map<String, Value>, limit: i64, offset: i64| {
            calls.lock().unwrap().push(json!({
                "params": params,
                "paginate": false,
                "limit": limit,
                "offset": offset,
            }));
            let entry = queue.lock().unwrap().pop_front().unwrap_or_else(
                || json!({"other": {"class": "IndexError", "message": "pop from empty list"}}),
            );
            let future: metrika_traffic::ReportFuture = Box::pin(async move {
                if let Some(body) = entry.get("body") {
                    return Ok(body.clone());
                }
                if let Some(spec) = entry.get("metrika_error") {
                    return Err(MetrikaFail::Metrika(MetrikaError {
                        status: spec.get("status").and_then(Value::as_i64).unwrap_or(0),
                        message: spec
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                    }));
                }
                if let Some(spec) = entry.get("other") {
                    return Err(MetrikaFail::Python {
                        class: spec
                            .get("class")
                            .and_then(Value::as_str)
                            .unwrap_or("Exception")
                            .to_owned(),
                        message: spec
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                    });
                }
                Err(MetrikaFail::Python {
                    class: "AssertionError".to_owned(),
                    message: format!("unknown client entry {entry}"),
                })
            });
            future
        }
    };
    let report: &ReportCall = &report_impl;

    let gsc_spec = op.get("gsc").and_then(|g| g.get("fetch")).cloned();
    let fetch_holder;
    let gsc_fetch = match &gsc_spec {
        Some(spec) => {
            let spec = spec.clone();
            let gsc_calls = gsc_calls.clone();
            let fetch: Box<GscFetch> = Box::new(move |date1: &str, date2: &str| {
                gsc_calls
                    .lock()
                    .unwrap()
                    .push(json!({"date1": date1, "date2": date2}));
                if let Some(err) = spec.get("error") {
                    return Err(err.as_str().unwrap_or_default().to_owned());
                }
                Ok(spec.get("result").cloned().unwrap_or(Value::Null))
            });
            fetch_holder = fetch;
            Some(&*fetch_holder as &GscFetch)
        }
        None => None,
    };

    let args = op.get("args").cloned().unwrap_or_else(|| json!({}));
    let input = TrafficInput {
        counter_id: args.get("counter_id").cloned().unwrap_or(Value::Null),
        date1: args.get("date1").cloned().unwrap_or(Value::Null),
        date2: args.get("date2").cloned().unwrap_or(Value::Null),
        attribution: args
            .get("attribution")
            .cloned()
            .unwrap_or_else(|| json!("last_significant")),
        traffic: args
            .get("traffic")
            .cloned()
            .unwrap_or_else(|| json!("organic")),
        lang: args.get("lang").cloned().unwrap_or_else(|| json!("en")),
        top: args.get("top").cloned().unwrap_or_else(|| json!(15)),
        filters: args.get("filters").cloned(),
        site_label: args.get("site_label").cloned(),
        gsc_rows: args.get("gsc_rows").filter(|v| !v.is_null()).cloned(),
        gsc_fetch,
        now: args.get("now").and_then(Value::as_i64),
    };
    let result = metrika_traffic::build_traffic_document(&input, &report).await;

    let mut record = outcome(result);
    let record = record.as_object_mut().unwrap();
    record.insert(
        "calls".to_owned(),
        Value::Array(calls.lock().unwrap().clone()),
    );
    record.insert("sleeps".to_owned(), json!([]));
    record.insert("spends".to_owned(), json!([]));
    record.insert("zone_probes".to_owned(), json!([]));
    record.insert(
        "gsc_calls".to_owned(),
        Value::Array(gsc_calls.lock().unwrap().clone()),
    );
    Value::Object(record.clone())
}

#[tokio::test]
async fn metrika_oracle_replay() {
    let path = fixtures_dir().join("metrika_oracle.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("fixture {}: {e}", path.display()));
    let fixture: Value = serde_json::from_str(&text).expect("fixture parses");
    let ops = fixture
        .get("ops")
        .and_then(Value::as_array)
        .expect("fixture.ops");
    for (index, op) in ops.iter().enumerate() {
        let name = op.get("name").and_then(Value::as_str).unwrap_or("?");
        let want = op.get("record").expect("op.record");
        let got = match op.get("kind").and_then(Value::as_str) {
            Some("client") => run_client_op(op).await,
            Some("traffic") => run_traffic_op(op).await,
            other => panic!("op {index} ({name}): unknown kind {other:?}"),
        };
        assert_eq!(
            &got,
            want,
            "op {index} ({name}) differs\nwant: {}\n got: {}",
            serde_json::to_string_pretty(want).unwrap_or_default(),
            serde_json::to_string_pretty(&got).unwrap_or_default(),
        );
    }
}

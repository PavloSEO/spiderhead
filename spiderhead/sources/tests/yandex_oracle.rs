//! Oracle replay for the reference suite's fixture — `yandex_webmaster.rs`
//! `collect`/`url_queries` against a scripted `transport=` callable, `yandex_cloud.rs`
//! Wordstat/WebSearch against a scripted `open_no_redirect` queue, and
//! `yandex_regions.rs` lookups/tree fetches. Every op replays the recorded
//! calls/sleeps/spends/files and diffs the outcome field-wise.

mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};
use spiderhead_core::host::FakeHost;
use spiderhead_sources::credentials::CredentialError;
use spiderhead_sources::http::{Request, Response, Sleeper, Transport, TransportError};
use spiderhead_sources::metrika::SpendSink;
use spiderhead_sources::pyjson;
use spiderhead_sources::spend::Record;
use spiderhead_sources::yandex_cloud::{
    self, Cloud, CloudAnswer, CloudArgs, CloudEnv, CloudError, CloudTransport,
};
use spiderhead_sources::yandex_regions;
use spiderhead_sources::yandex_webmaster::{
    self, CollectArgs, UrlQueriesArgs, WmEnv, WmError, WmTransport,
};

use common::{absent_config_root, fixtures_dir};

/// `spend.record` kwargs as JSON — reference passes ints for `cost`/`items`.
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
    if let Some(items) = rec.items {
        m.insert("items".to_owned(), json!(items));
    }
    if let Some(extra) = &rec.extra {
        m.insert("extra".to_owned(), Value::Object(extra.clone()));
    }
    Value::Object(m)
}

/// Recorded pauses as JSON — reference `exponential_pause`/`retry_after_pause` return ints,
/// so an integral pause serializes as `2`, a jittered one stays a float.
fn pauses_json(pauses: &[f64]) -> Value {
    Value::Array(
        pauses
            .iter()
            .map(|p| {
                if p.fract() == 0.0 {
                    json!(*p as i64)
                } else {
                    json!(*p)
                }
            })
            .collect(),
    )
}

/// `outcome` for a webmaster result.
fn wm_outcome(result: Result<Value, WmError>) -> Value {
    match result {
        Ok(value) => json!({"result": value}),
        Err(err) => json!({"error": {
            "class": err.python_class(),
            "message": err.to_string(),
        }}),
    }
}

/// `outcome` for a cloud result.
fn cloud_outcome<T: serde::Serialize>(result: Result<T, CloudError>) -> Value {
    match result {
        Ok(value) => json!({"result": value}),
        Err(err) => json!({"error": {
            "class": err.python_class(),
            "message": err.to_string(),
        }}),
    }
}

/// A credential reader from an env spec dict (`{token|missing|oserror}`).
fn credential_probe(spec: Value) -> impl Fn() -> Result<String, CredentialError> {
    move || {
        if let Some(token) = spec.get("token").and_then(Value::as_str) {
            return Ok(token.to_owned());
        }
        if let Some(message) = spec.get("missing").and_then(Value::as_str) {
            return Err(CredentialError::Missing(message.to_owned()));
        }
        if let Some(message) = spec.get("oserror").and_then(Value::as_str) {
            return Err(CredentialError::Io(std::io::Error::other(
                message.to_owned(),
            )));
        }
        unreachable!("unknown credential spec {spec}")
    }
}

/// The scripted webmaster `transport` callable: records `{method,url,payload,token}`,
/// resolves the next queue entry into body text or a `WmError`.
fn wm_transport(
    queue: &Value,
    calls: Arc<Mutex<Vec<Value>>>,
) -> impl Fn(&str, &str, Option<&Value>, &Value) -> Result<String, WmError> {
    let queue = Mutex::new(
        queue
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect::<VecDeque<Value>>(),
    );
    move |method, url, payload, token| {
        calls.lock().unwrap().push(json!({
            "method": method,
            "url": url,
            "payload": payload.cloned().unwrap_or(Value::Null),
            "token": token.clone(),
        }));
        // `queue.pop(0)` — the fake records the call, then IndexError on an empty queue.
        let Some(entry) = queue.lock().unwrap().pop_front() else {
            return Err(WmError::Python {
                class: "IndexError".to_owned(),
                message: "pop from empty list".to_owned(),
            });
        };
        if let Some(body) = entry.get("body") {
            return Ok(pyjson::dumps(body));
        }
        if let Some(text) = entry.get("text").and_then(Value::as_str) {
            return Ok(text.to_owned());
        }
        if let Some(spec) = entry.get("http") {
            return Err(WmError::Http {
                code: spec.get("code").and_then(Value::as_i64).unwrap_or(500),
                reason: spec
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("Error")
                    .to_owned(),
                body: spec
                    .get("body")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            });
        }
        if let Some(message) = entry.get("url_error").and_then(Value::as_str) {
            return Err(WmError::Url(message.to_owned()));
        }
        if let Some(message) = entry.get("timeout").and_then(Value::as_str) {
            return Err(WmError::Timeout(message.to_owned()));
        }
        if let Some(spec) = entry.get("other") {
            return Err(WmError::Python {
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
        panic!("unknown webmaster entry {entry}");
    }
}

/// One webmaster `collect`/`url_queries` op.
async fn run_wm_op(op: &Value) -> Value {
    let env_spec = op.get("env").cloned().unwrap_or_else(|| json!({}));
    let host = FakeHost::default();
    let root = absent_config_root();
    let transport = NoTransport;
    let (sleeper, pauses) = Sleeper::recording();
    let today_spec = env_spec.get("today").and_then(Value::as_str);
    let today = move || today_spec.unwrap_or("2025-01-01").to_owned();
    let token_spec = env_spec
        .get("token")
        .cloned()
        .unwrap_or_else(|| json!({"missing": "synthetic missing credentials"}));
    let token = credential_probe(token_spec);
    let env = WmEnv {
        host: &host,
        root: &root,
        transport: &transport,
        sleeper: &sleeper,
        today: Some(&today),
        token: Some(&token),
    };
    let calls = Arc::new(Mutex::new(Vec::new()));
    let send = wm_transport(op.get("queue").unwrap_or(&Value::Null), calls.clone());
    let wm_call: &WmTransport = &send;

    let call = op.get("call").cloned().unwrap_or_else(|| json!({}));
    let args = call.get("args").cloned().unwrap_or_else(|| json!({}));
    let arg = |key: &str| args.get(key);

    let result = match call.get("fn").and_then(Value::as_str) {
        Some("collect") => {
            let collect_args = CollectArgs {
                user_id: arg("user_id"),
                host_id: arg("host_id"),
                query_id: arg("query_id"),
                params: arg("params"),
                paginate: arg("paginate"),
                max_rows: arg("max_rows"),
                token: arg("token"),
                transport: Some(wm_call),
            };
            yandex_webmaster::collect(
                arg("operation").and_then(Value::as_str).unwrap_or_default(),
                &collect_args,
                &env,
            )
            .await
        }
        Some("url_queries") => {
            let query_args = UrlQueriesArgs {
                url: arg("url"),
                url_contains: arg("url_contains"),
                user_id: arg("user_id"),
                max_urls: arg("max_urls"),
                max_queries_per_url: arg("max_queries_per_url"),
                start_date: arg("start_date"),
                end_date: arg("end_date"),
                token: arg("token"),
                transport: Some(wm_call),
            };
            yandex_webmaster::url_queries(arg("host_id").unwrap_or(&Value::Null), &query_args, &env)
                .await
        }
        other => panic!("unknown webmaster fn {other:?}"),
    };

    let mut record = wm_outcome(result);
    let record = record.as_object_mut().unwrap();
    record.insert(
        "calls".to_owned(),
        Value::Array(calls.lock().unwrap().clone()),
    );
    record.insert("sleeps".to_owned(), pauses_json(&pauses.lock().unwrap()));
    record.insert("spends".to_owned(), json!([]));
    Value::Object(record.clone())
}

/// An always-failing `Transport` — the live send must never run during replay.
struct NoTransport;

impl Transport for NoTransport {
    async fn send(&self, request: &Request) -> Result<Response, TransportError> {
        panic!("live transport must not run during replay: {}", request.url);
    }
}

/// The scripted cloud `transport` callable: records
/// `{method,url,auth,content_type,body,timeout}`, resolves the next queue entry.
fn cloud_transport(
    queue: &Value,
    calls: Arc<Mutex<Vec<Value>>>,
) -> impl Fn(&str, &str, Option<&Value>, &str) -> Result<CloudAnswer, CloudError> {
    let queue = Mutex::new(
        queue
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect::<VecDeque<Value>>(),
    );
    move |method, url, body, key| {
        calls.lock().unwrap().push(json!({
            "method": method,
            "url": url,
            "auth": format!("Api-Key {key}"),
            "content_type": body.map(|_| "application/json"),
            "body": body.map(pyjson::dumps),
            "timeout": 45,
        }));
        let Some(entry) = queue.lock().unwrap().pop_front() else {
            return Err(CloudError::Python {
                class: "IndexError".to_owned(),
                message: "pop from empty list".to_owned(),
            });
        };
        let strict = |status: i64, bytes: Vec<u8>| {
            Ok(CloudAnswer {
                status,
                body: bytes,
                strict: true,
            })
        };
        if let Some(body) = entry.get("body") {
            return strict(
                entry.get("status").and_then(Value::as_i64).unwrap_or(200),
                pyjson::dumps(body).into_bytes(),
            );
        }
        if let Some(text) = entry.get("text").and_then(Value::as_str) {
            return strict(
                entry.get("status").and_then(Value::as_i64).unwrap_or(200),
                text.as_bytes().to_vec(),
            );
        }
        if let Some(bytes) = entry.get("bytes").and_then(Value::as_array) {
            return strict(
                entry.get("status").and_then(Value::as_i64).unwrap_or(200),
                bytes
                    .iter()
                    .filter_map(|b| b.as_u64().map(|v| v as u8))
                    .collect(),
            );
        }
        if let Some(spec) = entry.get("http") {
            return Ok(CloudAnswer {
                status: spec.get("code").and_then(Value::as_i64).unwrap_or(500),
                body: spec
                    .get("body")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .as_bytes()
                    .to_vec(),
                strict: false,
            });
        }
        if let Some(message) = entry.get("url_error").and_then(Value::as_str) {
            return Err(CloudError::Url(message.to_owned()));
        }
        if let Some(message) = entry.get("ssl_error").and_then(Value::as_str) {
            return Err(CloudError::Ssl(message.to_owned()));
        }
        if let Some(message) = entry.get("timeout").and_then(Value::as_str) {
            return Err(CloudError::Timeout(message.to_owned()));
        }
        if let Some(spec) = entry.get("other") {
            return Err(CloudError::Python {
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
        panic!("unknown cloud entry {entry}");
    }
}

/// `kw.get(key, default)` — an explicit `null` passes through like reference `None`.
fn arg_or<'a>(args: &'a Value, key: &str, default: &'a Value) -> &'a Value {
    args.get(key).unwrap_or(default)
}

/// One cloud/regions op.
async fn run_cloud_op(op: &Value) -> Value {
    let env_spec = op.get("env").cloned().unwrap_or_else(|| json!({}));
    let mut host = FakeHost::default();
    if let Some(home) = env_spec.get("home").and_then(Value::as_str) {
        host.env.insert("HOME".to_owned(), home.to_owned());
    }
    let root = absent_config_root();
    let transport = NoTransport;
    let (sleeper, pauses) = Sleeper::recording();

    let key_spec = env_spec
        .get("api_key")
        .cloned()
        .unwrap_or_else(|| json!({"token": "synthetic-api-key"}));
    let api_key = credential_probe(key_spec);
    let folder_spec = env_spec
        .get("folder_id")
        .cloned()
        .unwrap_or_else(|| json!({"token": "synthetic-folder-id"}));
    let folder_id = credential_probe(folder_spec);

    let spends = Arc::new(Mutex::new(Vec::<Value>::new()));
    let spend_impl = {
        let spends = spends.clone();
        move |rec: &Record| {
            spends.lock().unwrap().push(spend_json(rec));
            Ok(json!({"ok": true}))
        }
    };
    let spend: &SpendSink = &spend_impl;

    let step = env_spec
        .get("mono_step")
        .and_then(Value::as_f64)
        .unwrap_or(0.001);
    let tick = Arc::new(Mutex::new(0u64));
    let monotonic = {
        let tick = tick.clone();
        move || {
            let mut guard = tick.lock().unwrap();
            *guard += 1;
            1000.0 + *guard as f64 * step
        }
    };

    let env = CloudEnv {
        host: &host,
        root: &root,
        transport: &transport,
        sleeper: &sleeper,
        spend: Some(spend),
        monotonic: Some(&monotonic),
        api_key: Some(&api_key),
        folder_id: Some(&folder_id),
    };

    let calls = Arc::new(Mutex::new(Vec::new()));
    let send = cloud_transport(op.get("queue").unwrap_or(&Value::Null), calls.clone());
    let cloud_call: &CloudTransport = &send;

    let call = op.get("call").cloned().unwrap_or_else(|| json!({}));
    let args = call.get("args").cloned().unwrap_or_else(|| json!({}));
    let init = call.get("init").cloned().unwrap_or_else(|| json!({}));
    let cloud_args = CloudArgs {
        api_key: init.get("api_key"),
        folder_id: init.get("folder_id"),
        rps: init.get("rps"),
        transport: Some(cloud_call),
    };
    let kw = args.get("kw").cloned().unwrap_or_else(|| json!({}));
    let pos = args.get("pos").cloned().unwrap_or_else(|| json!([]));
    let pos_at = |index: usize| pos.get(index).cloned().unwrap_or(Value::Null);

    let fn_name = call.get("fn").and_then(Value::as_str).unwrap_or_default();
    let result = match fn_name {
        "regions.fetch_tree" => {
            yandex_regions::fetch_tree(&env, &cloud_args, kw.get("save_to")).await
        }
        "regions.constructor" => match Cloud::new(&env, &cloud_args) {
            Ok(_) => Ok(json!("constructed")),
            Err(err) => Err(err),
        },
        _ => {
            let client = Cloud::new(&env, &cloud_args);
            match client {
                Err(err) => Err(err),
                Ok(client) => {
                    let default_regions = json!(["225"]);
                    let default_devices = json!(["DEVICE_ALL"]);
                    let regions = arg_or(&kw, "regions", &default_regions);
                    let devices = arg_or(&kw, "devices", &default_devices);
                    match fn_name {
                        "wordstat.top" => {
                            client
                                .wordstat_top(
                                    &pos_at(0),
                                    arg_or(&kw, "limit", &json!(300)),
                                    regions,
                                    devices,
                                )
                                .await
                        }
                        "wordstat.dynamics" => {
                            client
                                .wordstat_dynamics(
                                    &pos_at(0),
                                    &pos_at(1),
                                    &pos_at(2),
                                    arg_or(&kw, "period", &json!("PERIOD_MONTHLY")),
                                    regions,
                                    devices,
                                )
                                .await
                        }
                        "wordstat.expand" => {
                            client
                                .wordstat_expand(
                                    &pos_at(0),
                                    arg_or(&kw, "limit", &json!(300)),
                                    regions,
                                    devices,
                                )
                                .await
                        }
                        "websearch.search" => {
                            client
                                .web_search(
                                    &pos_at(0),
                                    arg_or(&kw, "region", &json!("225")),
                                    arg_or(&kw, "search_type", &json!("SEARCH_TYPE_RU")),
                                    arg_or(&kw, "groups", &json!(10)),
                                    arg_or(&kw, "docs_in_group", &json!(1)),
                                    arg_or(&kw, "family", &json!("FAMILY_MODE_NONE")),
                                    arg_or(&kw, "timeout", &json!(120)),
                                )
                                .await
                        }
                        "websearch.batch" => {
                            client
                                .search_batch(
                                    &pos_at(0),
                                    arg_or(&kw, "region", &json!("225")),
                                    arg_or(&kw, "search_type", &json!("SEARCH_TYPE_RU")),
                                    arg_or(&kw, "groups", &json!(10)),
                                    arg_or(&kw, "docs_in_group", &json!(1)),
                                    arg_or(&kw, "family", &json!("FAMILY_MODE_NONE")),
                                    arg_or(&kw, "timeout", &json!(300)),
                                    arg_or(&kw, "poll", &json!(4)),
                                )
                                .await
                        }
                        other => panic!("unknown cloud fn {other}"),
                    }
                }
            }
        }
    };

    let mut record = cloud_outcome(result);
    let record = record.as_object_mut().unwrap();
    record.insert(
        "calls".to_owned(),
        Value::Array(calls.lock().unwrap().clone()),
    );
    record.insert("sleeps".to_owned(), pauses_json(&pauses.lock().unwrap()));
    record.insert(
        "spends".to_owned(),
        Value::Array(spends.lock().unwrap().clone()),
    );
    Value::Object(record.clone())
}

/// One helper op — pure functions, no env.
fn run_helper_op(op: &Value) -> Value {
    let call = op.get("call").cloned().unwrap_or_else(|| json!({}));
    let args = call.get("args").cloned().unwrap_or_else(|| json!({}));
    let pos = args.get("pos").cloned().unwrap_or_else(|| json!([]));
    let first = pos.get(0).cloned().unwrap_or(Value::Null);
    let result: Result<Value, CloudError> = match call.get("fn").and_then(Value::as_str) {
        Some("normalize") => yandex_cloud::normalize(&first).map(|s| json!(s)),
        Some("parse_serp") => Ok(json!(yandex_cloud::parse_serp(
            first.as_str().unwrap_or_default()
        ))),
        Some("by_name") => Ok(json!(yandex_regions::by_name(
            first.as_str().unwrap_or_default()
        ))),
        other => panic!("unknown helper fn {other:?}"),
    };
    let mut record = cloud_outcome(result);
    let record = record.as_object_mut().unwrap();
    record.insert("calls".to_owned(), json!([]));
    record.insert("sleeps".to_owned(), json!([]));
    record.insert("spends".to_owned(), json!([]));
    Value::Object(record.clone())
}

/// `capture_files` — walk `yandex_tmp/` into a `{path: text}` map, then remove it.
fn capture_files(record: &mut Map<String, Value>) {
    let scratch = std::path::Path::new("yandex_tmp");
    if !scratch.exists() {
        return;
    }
    let mut files = Map::new();
    let mut stack = vec![scratch.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read_dir: {e}")) {
            let path = entry.unwrap_or_else(|e| panic!("dir entry: {e}")).path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let text = std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
                files.insert(path.to_string_lossy().into_owned(), json!(text));
            }
        }
    }
    std::fs::remove_dir_all(scratch).unwrap_or_else(|e| panic!("cleanup: {e}"));
    record.insert("files".to_owned(), Value::Object(files));
}

#[tokio::test(flavor = "current_thread")]
async fn yandex_oracle_replay() {
    // File-writing ops (`save_to`) use relative paths — run the whole replay inside
    // a scratch cwd so `yandex_tmp/` never lands in the repository.
    let original_cwd = std::env::current_dir().unwrap();
    let scratch = std::env::temp_dir().join(format!("yandex_oracle_{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    std::env::set_current_dir(&scratch).unwrap();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let path = fixtures_dir().join("yandex_oracle.json");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("fixture {}: {e}", path.display()));
        serde_json::from_str::<Value>(&text).expect("fixture parses")
    }));
    std::env::set_current_dir(&original_cwd).unwrap();
    let fixture = result.unwrap_or_else(|e| std::panic::resume_unwind(e));

    // The fixture path was resolved relative to the crate — run ops from scratch.
    std::env::set_current_dir(&scratch).unwrap();
    let ops = fixture
        .get("ops")
        .and_then(Value::as_array)
        .expect("fixture.ops")
        .clone();

    let mut failures = Vec::new();
    for (index, op) in ops.iter().enumerate() {
        let name = op.get("name").and_then(Value::as_str).unwrap_or("?");
        eprintln!("op {index} {name}");
        let want = op.get("record").expect("op.record");
        let mut got = match op.get("kind").and_then(Value::as_str) {
            Some("collect") => run_wm_op(op).await,
            Some("cloud") => run_cloud_op(op).await,
            Some("helper") => run_helper_op(op),
            other => panic!("op {index} ({name}): unknown kind {other:?}"),
        };
        capture_files(got.as_object_mut().unwrap());
        if &got != want {
            failures.push(format!(
                "op {index} ({name}) differs\nwant: {}\n got: {}",
                serde_json::to_string_pretty(want).unwrap_or_default(),
                serde_json::to_string_pretty(&got).unwrap_or_default(),
            ));
            if failures.len() >= 5 {
                break;
            }
        }
    }
    std::env::set_current_dir(&original_cwd).unwrap();
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(failures.is_empty(), "{}", failures.join("\n---\n"));
}

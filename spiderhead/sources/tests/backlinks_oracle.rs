//! Replay `backlinks_oracle.json` — reference `backlink_indexes` and `backlink_history`
//! driven by scripted ops (`estimate`/`collect`/`save_snapshot`/`snapshot_history`/
//! `read_snapshot`) with an injected transport recording each request, scenario-keyed
//! credentials, a pinned `uuid.uuid4` and a recording `spend.record`. Sandbox paths
//! normalize to `$BASE`.
//!
//! the reference suite — every reference check maps to one or more ops.

mod common;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use spiderhead_sources::backlink_history as bh;
use spiderhead_sources::backlink_indexes::{
    self as bi, BlEnv, CollectArgs, TransportFail, TransportReply,
};
use spiderhead_sources::credentials::CredentialError;
use spiderhead_sources::http::Request;
use spiderhead_sources::pyjson;
use spiderhead_sources::spend::Record;

use common::{load_fixture, StubTransport};

/// Compare two JSON values after normalizing request bodies (JSON strings may serialize
/// with different whitespace).
fn requests_eq(want: &[Value], got: &[Value]) -> bool {
    if want.len() != got.len() {
        return false;
    }
    want.iter().zip(got.iter()).all(|(w, g)| {
        if w == g {
            return true;
        }
        let mut w = w.clone();
        let mut g = g.clone();
        for req in [&mut w, &mut g] {
            if let Some(body) = req.get("body").and_then(Value::as_str) {
                if let Ok(parsed) = serde_json::from_str::<Value>(body) {
                    req["body"] = parsed;
                }
            }
        }
        w == g
    })
}

/// `{"$float": "nan"|"inf"|"-inf"}` → a string the module's float path parses to the
/// same non-finite value (JSON cannot carry NaN/inf; the parsed float hits the same
/// `math.isfinite` rejection as reference).
fn decode_kw(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            if let Some(marker) = map.get("$float").and_then(Value::as_str) {
                return Value::String(marker.to_owned());
            }
            Value::Object(
                map.iter()
                    .map(|(k, v)| (k.clone(), decode_kw(v)))
                    .collect::<Map<_, _>>(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(decode_kw).collect()),
        other => other.clone(),
    }
}

/// `$BASE` → the scenario sandbox path inside op arguments.
fn expand_base(value: &Value, base: &Path) -> Value {
    let text = serde_json::to_string(value).unwrap_or_default();
    serde_json::from_str(&text.replace("$BASE", &base.to_string_lossy()))
        .unwrap_or_else(|_| value.clone())
}

fn pick<'a>(kw: &'a Value, key: &str) -> Option<&'a Value> {
    kw.get(key)
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

fn bl_outcome(result: Result<Value, bi::BlError>) -> Value {
    match result {
        Ok(value) => json!({"result": value}),
        Err(err) => {
            json!({"error": {"class": err.python_class(), "message": err.to_string()}})
        }
    }
}

fn bh_outcome(result: Result<Value, bh::BlhError>) -> Value {
    match result {
        Ok(value) => json!({"result": value}),
        Err(err) => {
            json!({"error": {"class": err.python_class(), "message": err.to_string()}})
        }
    }
}

/// `(body_text, headers)` from one scripted reply spec.
fn reply_body(spec: &Value, request: &Request) -> String {
    if spec.get("moz_rpc").is_some()
        || spec.get("moz_rpc_bad_id").is_some()
        || spec.get("moz_rpc_error").is_some()
    {
        let sent: Value = serde_json::from_slice(&request.body).unwrap_or_default();
        let id = sent.get("id").cloned().unwrap_or(Value::Null);
        let body = if let Some(result) = spec.get("moz_rpc") {
            json!({"jsonrpc": "2.0", "id": id, "result": result})
        } else if let Some(result) = spec.get("moz_rpc_bad_id") {
            json!({"jsonrpc": "2.0", "id": "deadbeef", "result": result})
        } else {
            json!({"jsonrpc": "2.0", "id": id, "error": spec.get("moz_rpc_error").cloned().unwrap_or_default()})
        };
        return pyjson::dumps(&body);
    }
    match spec.get("body") {
        Some(body @ (Value::Object(_) | Value::Array(_))) => pyjson::dumps(body),
        Some(Value::String(text)) => text.clone(),
        _ => String::new(),
    }
}

/// The recorded request the reference gen writes per transport call. The Rust module
/// renders the `_OMITTED` sentinel as an empty `limit=`/`Count=` where reference leaks a
/// nondeterministic object repr — the gen normalizes it to `$OMITTED`, so the recording
/// normalizes the empty value the same way.
fn record_request(request: &Request) -> Value {
    let omitted = |text: &str| {
        text.replace("limit=&", "limit=$OMITTED&")
            .replace("Count=&", "Count=$OMITTED&")
    };
    let url = omitted(&request.url);
    let url = if url.ends_with("limit=") {
        format!("{url}$OMITTED")
    } else {
        url
    };
    json!({
        "method": request.method,
        "url": url,
        "headers": request
            .headers
            .iter()
            .map(|(k, v)| json!([k.to_lowercase(), v]))
            .collect::<Vec<_>>(),
        "body": if request.body.is_empty() {
            Value::Null
        } else {
            Value::String(omitted(&String::from_utf8_lossy(&request.body)))
        },
    })
}

struct ScriptTransport<'a> {
    replies: &'a RefCell<VecDeque<Value>>,
    requests: &'a RefCell<Vec<Value>>,
}

impl<'a> ScriptTransport<'a> {
    fn call(&self, request: &Request) -> Result<TransportReply, TransportFail> {
        self.requests.borrow_mut().push(record_request(request));
        let spec = self
            .replies
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| panic!("stub: out of replies"));
        if let Some(fault) = spec.get("raise") {
            let class = fault
                .get("class")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let message = fault
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            return match class {
                "URLError" | "TimeoutError" | "OSError" | "ValueError" => {
                    Err(TransportFail::Caught)
                }
                other => Err(TransportFail::Raised(other.to_owned(), message)),
            };
        }
        let headers = spec
            .get("headers")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_owned()))
                    .collect()
            })
            .unwrap_or_default();
        Ok(TransportReply {
            body: reply_body(&spec, request),
            headers,
        })
    }
}

fn spend_json(rec: &Record) -> Value {
    let mut out = Map::new();
    out.insert("source".to_owned(), json!(rec.source));
    out.insert("operation".to_owned(), json!(rec.operation));
    if let Some(cost) = rec.cost {
        out.insert("cost".to_owned(), json!(cost));
    } else {
        out.insert("cost".to_owned(), Value::Null);
    }
    out.insert("unit".to_owned(), json!(rec.unit));
    if let Some(extra) = &rec.extra {
        out.insert("extra".to_owned(), Value::Object(extra.clone()));
    }
    Value::Object(out)
}

fn cred_probe(spec: &Value) -> impl Fn(&str, &str) -> Result<String, CredentialError> + '_ {
    move |path: &str, env_var: &str| {
        let entry = spec
            .get(path)
            .or_else(|| spec.get("*"))
            .cloned()
            .unwrap_or_else(|| json!({"missing": format!("{path} ({env_var})")}));
        if let Some(token) = entry.get("token").and_then(Value::as_str) {
            return Ok(token.to_owned());
        }
        if let Some(message) = entry.get("missing").and_then(Value::as_str) {
            return Err(CredentialError::Missing(message.to_owned()));
        }
        if let Some(message) = entry.get("oserror").and_then(Value::as_str) {
            return Err(CredentialError::Io(std::io::Error::other(
                message.to_owned(),
            )));
        }
        unreachable!("unknown credential spec {entry}")
    }
}

fn sandbox(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("backlinks-oracle-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap_or_default();
    dir
}

async fn run_op(
    op: &Value,
    base: &Path,
    env: &BlEnv<'_, StubTransport>,
    transport: &ScriptTransport<'_>,
) -> Value {
    let fn_name = op.get("fn").and_then(Value::as_str).unwrap_or_default();
    let kw = expand_base(&decode_kw(op.get("kw").unwrap_or(&Value::Null)), base);
    match fn_name {
        "_setup" => {
            if let Some(files) = op.get("files").and_then(Value::as_object) {
                for (name, content) in files {
                    let target = base.join(name);
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent).unwrap_or_default();
                    }
                    std::fs::write(&target, content.as_str().unwrap_or_default())
                        .unwrap_or_default();
                }
            }
            for name in op
                .get("sqlite_foreign")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                let con = rusqlite::Connection::open(base.join(name.as_str().unwrap_or_default()))
                    .unwrap();
                con.execute_batch("CREATE TABLE scan(value TEXT)").unwrap();
            }
            for name in op
                .get("sqlite_bad_marker")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                let con = rusqlite::Connection::open(base.join(name.as_str().unwrap_or_default()))
                    .unwrap();
                con.execute_batch(
                    "CREATE TABLE backlink_meta(format TEXT PRIMARY KEY);
                     INSERT INTO backlink_meta VALUES ('other.v9');
                     CREATE TABLE backlink_snapshots (id INTEGER PRIMARY KEY)",
                )
                .unwrap();
            }
            json!({"result": true})
        }
        "estimate" => bl_outcome(bi::estimate(
            pick(&kw, "provider").unwrap_or(&Value::Null),
            pick(&kw, "limit"),
            pick(&kw, "operation"),
        )),
        "collect" => {
            let args = CollectArgs {
                limit: pick(&kw, "limit"),
                offset: pick(&kw, "offset"),
                operation: pick(&kw, "operation"),
                page_token: pick(&kw, "page_token"),
                confirm_paid: pick(&kw, "confirm_paid"),
                max_units: pick(&kw, "max_units"),
                api_key: pick(&kw, "api_key"),
                access_id: pick(&kw, "access_id"),
                secret_key: pick(&kw, "secret_key"),
                transport: Some(&|request: &Request| transport.call(request)),
            };
            bl_outcome(
                bi::collect(
                    env,
                    pick(&kw, "provider").unwrap_or(&Value::Null),
                    pick(&kw, "target").unwrap_or(&Value::Null),
                    &args,
                )
                .await,
            )
        }
        "save_snapshot" => bh_outcome(bh::save_snapshot(
            pick(&kw, "domain").unwrap_or(&Value::Null),
            pick(&kw, "rows").unwrap_or(&Value::Null),
            pick(&kw, "coverage").unwrap_or(&Value::Null),
            pick(&kw, "domain_edges").unwrap_or(&Value::Null),
            pick(&kw, "db"),
            pick(&kw, "project"),
            pick(&kw, "observed_at").unwrap_or(&Value::Null),
        )),
        "snapshot_history" => bh_outcome(bh::snapshot_history(
            pick(&kw, "db"),
            pick(&kw, "project"),
            pick(&kw, "domain"),
            pick(&kw, "limit"),
            pick(&kw, "offset"),
        )),
        "read_snapshot" => bh_outcome(bh::read_snapshot(
            pick(&kw, "snapshot_id").unwrap_or(&Value::Null),
            pick(&kw, "db"),
            pick(&kw, "project"),
        )),
        other => panic!("unknown fn {other}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn backlinks_oracle_replay() {
    let scenarios = load_fixture("backlinks_oracle.json");
    let mut failures = Vec::new();
    for scenario in &scenarios {
        let name = scenario
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_owned();
        let base = sandbox(&name);
        let replies = RefCell::new(VecDeque::<Value>::new());
        let requests = RefCell::new(Vec::<Value>::new());
        let spends = RefCell::new(Vec::<Value>::new());
        let mut problems = Vec::new();

        for (index, op) in scenario
            .get("ops")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            let want = op.get("outcome").cloned().unwrap_or_default();
            let fn_name = op.get("fn").and_then(Value::as_str).unwrap_or_default();
            let got = if fn_name == "collect" {
                requests.borrow_mut().clear();
                replies.borrow_mut().extend(
                    op.get("replies")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default(),
                );
                let cred_spec = op.get("cred").cloned().unwrap_or_else(|| json!({}));
                let cred = cred_probe(&cred_spec);
                let uuid_text = op
                    .get("uuid")
                    .and_then(Value::as_str)
                    .unwrap_or("11111111-2222-4333-8444-555555555555")
                    .to_owned();
                let uuid_probe = move || uuid_text.clone();
                let host = spiderhead_core::host::FakeHost::default();
                let root = base.join("no-config");
                let live_transport = StubTransport::new(&[]);
                let spend_impl = {
                    let spends = &spends;
                    move |rec: &Record| {
                        spends.borrow_mut().push(spend_json(rec));
                        Ok(json!({"ok": true}))
                    }
                };
                let script = ScriptTransport {
                    replies: &replies,
                    requests: &requests,
                };
                let env = BlEnv {
                    host: &host,
                    root: &root,
                    transport: &live_transport,
                    spend: Some(&spend_impl),
                    cred: Some(&cred),
                    uuid: Some(&uuid_probe),
                };
                let result = run_op(op, &base, &env, &script).await;
                let want_requests = op
                    .get("requests")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if !requests_eq(&want_requests, &requests.borrow()) {
                    problems.push(format!(
                        "op {index} requests differ\nwant: {}\n got: {}",
                        serde_json::to_string_pretty(&want_requests).unwrap_or_default(),
                        serde_json::to_string_pretty(&*requests.borrow()).unwrap_or_default()
                    ));
                }
                result
            } else {
                let live_transport = StubTransport::new(&[]);
                let host = spiderhead_core::host::FakeHost::default();
                let root = base.join("no-config");
                let env = BlEnv {
                    host: &host,
                    root: &root,
                    transport: &live_transport,
                    spend: None,
                    cred: None,
                    uuid: None,
                };
                let script = ScriptTransport {
                    replies: &replies,
                    requests: &requests,
                };
                run_op(op, &base, &env, &script).await
            };
            let mut got = got;
            scrub_base(&mut got, &base.to_string_lossy());
            if got != want {
                problems.push(format!(
                    "op {index} ({fn_name}) outcome differs\nwant: {}\n got: {}",
                    serde_json::to_string_pretty(&want).unwrap_or_default(),
                    serde_json::to_string_pretty(&got).unwrap_or_default()
                ));
            }
        }
        let want_spends = scenario.get("spends").cloned().unwrap_or_default();
        let got_spends = Value::Array(spends.borrow().clone());
        if got_spends != want_spends {
            problems.push(format!(
                "spends differ\nwant: {}\n got: {}",
                serde_json::to_string_pretty(&want_spends).unwrap_or_default(),
                serde_json::to_string_pretty(&got_spends).unwrap_or_default()
            ));
        }
        let _ = std::fs::remove_dir_all(&base);
        if !problems.is_empty() {
            failures.push(format!("scenario {name}: {}", problems.join("; ")));
            if failures.len() >= 5 {
                break;
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n---\n"));
}

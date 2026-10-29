//! Oracle replay for the reference suite's fixture — the Arsenkin client
//! (`_post`, `set_task`, `check`, `wait`, `get`/`refetch`/`delete`/`restart`, `limits`,
//! `status`, `run`, the wordstat helpers and `BatchRunner`) against scripted `responses`
//! queues. Every op replays the recorded requests/pauses/spends and diffs the outcome
//! field-wise.

mod common;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};
use spiderhead_sources::arsenkin::{
    self, ArsEnv, ArsError, BatchRunner, Client, ClientArgs, RateLimiter,
};
use spiderhead_sources::credentials::CredentialError;
use spiderhead_sources::http::Sleeper;
use spiderhead_sources::metrika::SpendSink;
use spiderhead_sources::spend::Record;

use common::{load_fixture, scenario_host, StubTransport};

/// `spend.record` kwargs as JSON — `task_id`/`items`/`extra` appear only when the caller
/// supplied them, like the reference kwargs.
fn spend_json(rec: &Record) -> Value {
    let mut m = Map::new();
    m.insert("source".to_owned(), json!(rec.source));
    m.insert("operation".to_owned(), json!(rec.operation));
    if let Some(cost) = rec.cost {
        m.insert("cost".to_owned(), json!(cost));
    }
    m.insert("unit".to_owned(), json!(rec.unit));
    if let Some(task_id) = &rec.task_id {
        m.insert("task_id".to_owned(), task_id.clone());
    }
    if let Some(items) = rec.items {
        m.insert("items".to_owned(), json!(items));
    }
    if let Some(extra) = &rec.extra {
        m.insert("extra".to_owned(), Value::Object(extra.clone()));
    }
    Value::Object(m)
}

/// `{result}` or `{error: {class, message}}` — the fixture's outcome shape.
fn outcome(result: Result<Value, ArsError>) -> Value {
    match result {
        Ok(value) => json!({"result": value}),
        Err(err) => json!({"error": {
            "class": err.python_class(),
            "message": err.to_string(),
        }}),
    }
}

/// `arsenkin_token` from a spec dict (`{token|missing|oserror}`).
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

fn pick<'a>(args: &'a Value, key: &str) -> &'a Value {
    args.get(key).unwrap_or(&Value::Null)
}

/// One oracle op: dispatch `args.fn` with `args.kw`.
async fn run_op(
    scenario: &Value,
    transport: &StubTransport,
    sleeper: &Sleeper,
    spend: &SpendSink<'_>,
) -> Result<Value, ArsError> {
    let (host, root) = scenario_host(scenario);
    let token = credential_probe(
        scenario
            .get("cred")
            .cloned()
            .unwrap_or_else(|| json!({"token": "test-token-123"})),
    );
    // `time.monotonic` — successive values from the scenario's `clock` list, repeating
    // the last one once exhausted.
    let clock_values: VecDeque<f64> = scenario
        .get("clock")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default();
    let clock_cell = RefCell::new(clock_values);
    let clock = move || {
        let mut q = clock_cell.borrow_mut();
        if q.len() > 1 {
            q.pop_front().unwrap_or_default()
        } else {
            q.front().copied().unwrap_or_default()
        }
    };
    let env = ArsEnv {
        host: &host,
        root: &root,
        transport,
        sleeper,
        spend: Some(spend),
        token: Some(&token),
        clock: Some(&clock),
    };

    let call = scenario.get("args").cloned().unwrap_or_else(|| json!({}));
    let fn_name = call.get("fn").and_then(Value::as_str).unwrap_or_default();
    let kwarg = call.get("kw").cloned().unwrap_or_else(|| json!({}));
    let limiter = scenario.get("limiter").map(|spec| {
        RateLimiter::new(
            spec.get("max_calls")
                .and_then(Value::as_i64)
                .unwrap_or(arsenkin::MAX_RPM),
            spec.get("period").and_then(Value::as_f64).unwrap_or(60.0),
            spec.get("safety").and_then(Value::as_i64).unwrap_or(3),
        )
    });
    let client = Client::new(
        &env,
        ClientArgs {
            token: kwarg.get("token"),
            limiter,
        },
    )?;

    match fn_name {
        "client" => Ok(json!(client.token())),
        "_post" => {
            client
                .post(
                    kwarg
                        .get("endpoint")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    pick(&kwarg, "body"),
                    kwarg.get("retries").and_then(Value::as_u64).unwrap_or(5) as usize,
                    spiderhead_sources::pystr::truthy(kwarg.get("billed")),
                )
                .await
        }
        "limits" => client.limits().await,
        "status" => client.status().await,
        "set_task" => {
            client
                .set_task(pick(&kwarg, "tools_name"), pick(&kwarg, "data"))
                .await
        }
        "check" => client
            .check(pick(&kwarg, "task_id"))
            .await
            .map(|(status, raw)| json!([status, raw])),
        "get" => client.get(pick(&kwarg, "task_id")).await,
        "refetch" => client.refetch(pick(&kwarg, "task_id")).await,
        "delete" => client.delete(pick(&kwarg, "task_id")).await,
        "restart" => client.restart(pick(&kwarg, "task_id")).await,
        "wait" => {
            client
                .wait(
                    pick(&kwarg, "task_id"),
                    kwarg.get("timeout"),
                    kwarg.get("interval"),
                )
                .await
        }
        "run" => client
            .run(
                pick(&kwarg, "tools_name"),
                pick(&kwarg, "data"),
                kwarg.get("timeout"),
                kwarg.get("interval"),
            )
            .await
            .map(|(result, cost)| json!([result, cost])),
        "batch_run" => {
            BatchRunner::new(
                &client,
                kwarg
                    .get("max_concurrent")
                    .and_then(Value::as_u64)
                    .unwrap_or(5) as usize,
            )
            .run(pick(&kwarg, "jobs"))
            .await
        }
        "batch_refetch" => {
            BatchRunner::new(&client, 5)
                .refetch(pick(&kwarg, "task_id"))
                .await
        }
        "_valid_task_id" => Ok(arsenkin::valid_task_id(pick(&kwarg, "value"))
            .map(|v| json!(v))
            .unwrap_or(Value::Null)),
        "_count_items" => arsenkin::count_items(pick(&kwarg, "data")).map(|n| json!(n)),
        "_sanitize_for_journal" => Ok(arsenkin::sanitize_for_journal(pick(&kwarg, "value"))),
        "sanitize_wordstat_query" => Ok(json!(arsenkin::sanitize_wordstat_query(pick(
            &kwarg, "phrase"
        )))),
        "wordstat_payload" => {
            arsenkin::wordstat_payload(pick(&kwarg, "queries"), pick(&kwarg, "region"))
        }
        "parse_wordstat" => {
            arsenkin::parse_wordstat(pick(&kwarg, "result"), pick(&kwarg, "region"))
        }
        other => panic!("unknown fn {other}"),
    }
}

/// Field-wise request comparison, mirroring `common::assert_requests_eq` — `body`
/// compares as parsed JSON (member order is not meaningful to the receiving API).
fn request_diffs(want: &[Value], got: &[Value]) -> Vec<String> {
    let mut problems = Vec::new();
    if want.len() != got.len() {
        problems.push(format!(
            "request count differs: want {} got {}",
            want.len(),
            got.len()
        ));
        return problems;
    }
    for (i, (w, g)) in want.iter().zip(got).enumerate() {
        for f in ["method", "url", "headers", "timeout"] {
            if w.get(f) != g.get(f) {
                problems.push(format!(
                    "request {i} field {f} differs: want {:?} got {:?}",
                    w.get(f),
                    g.get(f)
                ));
            }
        }
        let wb = w.get("body").and_then(Value::as_str).unwrap_or_default();
        let gb = g.get("body").and_then(Value::as_str).unwrap_or_default();
        match (
            serde_json::from_str::<Value>(wb),
            serde_json::from_str::<Value>(gb),
        ) {
            (Ok(wv), Ok(gv)) => {
                if wv != gv {
                    problems.push(format!("request {i} body differs\nwant: {wb}\n got: {gb}"));
                }
            }
            _ => {
                if wb != gb {
                    problems.push(format!("request {i} body differs\nwant: {wb}\n got: {gb}"));
                }
            }
        }
    }
    problems
}

#[tokio::test(flavor = "current_thread")]
async fn arsenkin_oracle_replay() {
    let scenarios = load_fixture("arsenkin_oracle.json");
    let mut failures = Vec::new();
    for (index, scenario) in scenarios.iter().enumerate() {
        let name = scenario
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_owned();
        let responses = scenario
            .get("responses")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let transport = StubTransport::new(&responses);
        let (sleeper, pauses) = Sleeper::recording();
        let spends = Arc::new(Mutex::new(Vec::<Value>::new()));
        let spend_impl = {
            let spends = spends.clone();
            move |rec: &Record| {
                spends.lock().unwrap().push(spend_json(rec));
                Ok(json!({"ok": true}))
            }
        };
        let spend: &SpendSink = &spend_impl;

        let expect = scenario.get("expect").cloned().unwrap_or_default();
        let want_outcome = if let Some(r) = expect.get("result") {
            json!({"result": r})
        } else {
            json!({"error": expect.get("error").cloned().unwrap_or_default()})
        };
        let want_spends = expect.get("spends").cloned().unwrap_or_default();
        let want_requests = expect.get("requests").cloned().unwrap_or_default();
        let want_pauses: Vec<f64> = expect
            .get("pauses")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_f64).collect())
            .unwrap_or_default();

        let got_outcome = outcome(run_op(scenario, &transport, &sleeper, spend).await);
        let mut problems = request_diffs(
            want_requests
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default(),
            &transport.recorded(),
        );
        if *pauses.lock().unwrap() != want_pauses[..] {
            problems.push(format!(
                "pauses differ: want {want_pauses:?} got {:?}",
                *pauses.lock().unwrap()
            ));
        }
        let got_spends = Value::Array(spends.lock().unwrap().clone());
        if got_spends != want_spends {
            problems.push(format!(
                "spends differ\nwant: {}\n got: {}",
                serde_json::to_string_pretty(&want_spends).unwrap_or_default(),
                serde_json::to_string_pretty(&got_spends).unwrap_or_default()
            ));
        }
        if got_outcome != want_outcome {
            problems.push(format!(
                "outcome differs\nwant: {}\n got: {}",
                serde_json::to_string_pretty(&want_outcome).unwrap_or_default(),
                serde_json::to_string_pretty(&got_outcome).unwrap_or_default()
            ));
        }
        if !problems.is_empty() {
            failures.push(format!("op {index} ({name}): {}", problems.join("; ")));
            if failures.len() >= 5 {
                break;
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n---\n"));
}

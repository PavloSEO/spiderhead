//! Replay `cloudflare_traffic_oracle.json` — reference `traffic`/`logpull`/
//! `read_token`/`verify_token` driven by scripted ops: a canned transport queue,
//! seam specs for bindings/history/open_project/import_logs/raw replies, sandbox
//! filesystem setup, and recorded calls/spend to diff. `$BASE`/`$HOME`/`$TMP` are
//! path placeholders produced by the reference scrubber.
//!

mod common;

use spiderhead_core::filesystem::set_permissions_mode;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};

use spiderhead_sources::cloudflare::{self, CfError, CfTransport, RawReply, RawTransport};
use spiderhead_sources::cloudflare_traffic::{self as cft, LogImport, LogpullEnv, TrafficEnv};
use spiderhead_sources::cloudflare_traffic::{BindingsLookup, HistorySink, ProjectOpener};
use spiderhead_sources::http::Sleeper;
use spiderhead_sources::metrika::SpendSink;
use spiderhead_sources::spend::Record;

use common::{load_fixture, scenario_host, StubTransport};

fn expand(value: &Value, base: &str) -> Value {
    if base.is_empty() {
        return value.clone();
    }
    let text = serde_json::to_string(value).unwrap_or_default();
    serde_json::from_str(&text.replace("$BASE", base)).unwrap_or_else(|_| value.clone())
}

fn scrub(value: &Value, base: &str) -> Value {
    let text = serde_json::to_string(value).unwrap_or_default();
    serde_json::from_str(&text.replace(base, "$BASE")).unwrap_or_else(|_| value.clone())
}

fn sandbox(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cf-traffic-oracle-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap_or_default();
    dir
}

fn chmod(path: &Path, mode: u32) {
    set_permissions_mode(path, mode).unwrap_or_default();
}

fn run_fs(steps: Option<&Value>, base: &str) {
    for step in steps.and_then(Value::as_array).cloned().unwrap_or_default() {
        if let Some(dir) = step.get("mkdir") {
            let dir = expand(dir, base);
            std::fs::create_dir_all(dir.as_str().unwrap_or_default()).unwrap_or_default();
        } else if let Some(list) = step.get("write").and_then(Value::as_array) {
            let path = expand(&list[0], base);
            let path = PathBuf::from(path.as_str().unwrap_or_default());
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap_or_default();
            }
            std::fs::write(&path, list[1].as_str().unwrap_or_default()).unwrap_or_default();
            if let Some(mode) = list.get(2).and_then(Value::as_u64) {
                chmod(&path, mode as u32);
            }
        } else if let Some(pair) = step.get("symlink").and_then(Value::as_array) {
            let src = expand(&pair[0], base);
            let dst = expand(&pair[1], base);
            let _ = (&src, &dst);
            #[cfg(unix)]
            std::os::unix::fs::symlink(
                src.as_str().unwrap_or_default(),
                dst.as_str().unwrap_or_default(),
            )
            .unwrap_or_default();
        } else if let Some(pair) = step.get("chmod").and_then(Value::as_array) {
            let path = expand(&pair[0], base);
            chmod(
                Path::new(path.as_str().unwrap_or_default()),
                pair[1].as_u64().unwrap_or(0o600) as u32,
            );
        }
    }
}

/// Scripted exception classes → the matching `CfError` variant the oracle diff sees.
fn raise_err(spec: &Value) -> CfError {
    let class = spec
        .get("class")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let message = spec
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    match class {
        "CloudflareError" | "RuntimeError" => CfError::Cloudflare(message),
        "ValueError" => CfError::Value(message),
        "URLError" | "TimeoutError" | "OSError" => CfError::Os(message),
        "KeyError" => CfError::Python("KeyError", message),
        "TypeError" => CfError::Python("TypeError", message),
        "AttributeError" => CfError::Python("AttributeError", message),
        _ => CfError::Python("RuntimeError", format!("{class}: {message}")),
    }
}

fn outcome(result: Result<Value, CfError>) -> Value {
    match result {
        Ok(value) => json!({"result": value}),
        Err(err) => {
            json!({"error": {"class": err.python_class(), "message": err.to_string()}})
        }
    }
}

/// `spend.record` entries as the JSONL shape the generator scrubs (`at` removed).
fn spend_json(rec: &Record) -> Value {
    let mut m = Map::new();
    m.insert("source".to_owned(), json!(rec.source));
    m.insert("operation".to_owned(), json!(rec.operation));
    m.insert(
        "cost".to_owned(),
        rec.cost.map_or(Value::Null, |c| json!(c)),
    );
    m.insert("unit".to_owned(), json!(rec.unit));
    if let Some(task) = &rec.task_id {
        m.insert("task_id".to_owned(), task.clone());
    }
    m.insert("items".to_owned(), json!(rec.items.unwrap_or(0)));
    if let Some(extra) = &rec.extra {
        m.insert("extra".to_owned(), Value::Object(extra.clone()));
    }
    Value::Object(m)
}

/// Days since the epoch for `YYYY-MM-DD` (the `today` seam).
fn day_of(date: &str) -> i64 {
    let y: i64 = date[0..4].parse().unwrap_or_default();
    let m: i64 = date[5..7].parse().unwrap_or_default();
    let d: i64 = date[8..10].parse().unwrap_or_default();
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Epoch microseconds for `YYYY-MM-DDTHH:MM:SS[.ffffff]Z` (the `now` seam).
fn micros_of(stamp: &str) -> i64 {
    let days = day_of(&stamp[0..10]);
    let h: i64 = stamp[11..13].parse().unwrap_or_default();
    let m: i64 = stamp[14..16].parse().unwrap_or_default();
    let s: i64 = stamp[17..19].parse().unwrap_or_default();
    let frac = if stamp.len() > 20 {
        let end = stamp[20..]
            .find(['Z', '+'])
            .map(|i| i + 20)
            .unwrap_or(stamp.len());
        let digits = stamp[20..end].trim_start_matches('.');
        let mut micros: i64 = digits.parse().unwrap_or_default();
        for _ in digits.len()..6 {
            micros *= 10;
        }
        micros
    } else {
        0
    };
    (days * 86_400 + h * 3600 + m * 60 + s) * 1_000_000 + frac
}

/// Standard base64 decode for the `bytes_b64`/`byte_repeat` raw-reply specs.
fn b64decode(text: &str) -> Vec<u8> {
    fn val(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0u32;
    for byte in text.bytes() {
        let Some(v) = val(byte) else { continue };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

/// Owned boxed seam closures (the crate aliases are borrowed `&dyn` references).
type BoxedLookup<'a> = dyn Fn(&str) -> Result<Value, CfError> + Send + Sync + 'a;
type BoxedHistory<'a> = dyn Fn(&Value, &str) -> Result<Value, CfError> + Send + Sync + 'a;
type BoxedImport<'a> = dyn Fn(&str, &str, i64) -> Result<Value, CfError> + Send + Sync + 'a;

/// A `{"json"|"raise"}` seam spec → the lookup closure; `None` leaves the seam unset.
fn script_lookup<'a>(
    spec: Option<&'a Value>,
    base: &'a str,
    calls: &'a Arc<Mutex<Map<String, Value>>>,
    name: &'a str,
) -> Option<Box<BoxedLookup<'a>>> {
    let spec = expand(spec?, base);
    let calls = calls.clone();
    let name = name.to_owned();
    Some(Box::new(move |arg: &str| {
        let recorded = scrub(&json!(arg), base);
        calls
            .lock()
            .unwrap()
            .entry(name.clone())
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .unwrap()
            .push(json!({"args": [recorded]}));
        if let Some(raise) = spec.get("raise") {
            return Err(raise_err(raise));
        }
        Ok(spec.get("json").cloned().unwrap_or(Value::Null))
    }))
}

/// `cloudflare_history.save_snapshot` substitute: `(result, project)` → stored row.
fn script_history<'a>(
    spec: Option<&'a Value>,
    base: &'a str,
    calls: &'a Arc<Mutex<Map<String, Value>>>,
) -> Option<Box<BoxedHistory<'a>>> {
    let spec = expand(spec?, base);
    let calls = calls.clone();
    Some(Box::new(move |_result: &Value, project: &str| {
        let recorded = scrub(&json!(project), base);
        calls
            .lock()
            .unwrap()
            .entry("history".to_owned())
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .unwrap()
            .push(json!({"project": recorded}));
        if let Some(raise) = spec.get("raise") {
            return Err(raise_err(raise));
        }
        Ok(spec.get("json").cloned().unwrap_or(Value::Null))
    }))
}

/// `logstore.import_logs` substitute: `(temp path, project, max_lines)` → result.
fn script_import<'a>(
    spec: Option<&'a Value>,
    base: &'a str,
    calls: &'a Arc<Mutex<Map<String, Value>>>,
) -> Option<Box<BoxedImport<'a>>> {
    let spec = expand(spec?, base);
    let calls = calls.clone();
    Some(Box::new(
        move |path: &str, project: &str, max_lines: i64| {
            let content = std::fs::read_to_string(path).unwrap_or_default();
            let recorded = scrub(&json!(project), base);
            calls
                .lock()
                .unwrap()
                .entry("import".to_owned())
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .unwrap()
                .push(json!({
                    "path": "$TMP",
                    "project": recorded,
                    "max_lines": max_lines,
                    "content": content,
                }));
            if let Some(raise) = spec.get("raise") {
                return Err(raise_err(raise));
            }
            Ok(spec.get("json").cloned().unwrap_or(Value::Null))
        },
    ))
}

#[allow(clippy::too_many_lines)]
async fn run_op(op: &Value, base: &str) -> (Value, Map<String, Value>, Vec<Value>) {
    run_fs(op.get("fs"), base);

    // `env` vars: null removes, strings set. FakeHost carries the rest as absent.
    let host_spec = {
        let mut vars = Map::new();
        for (key, value) in op
            .get("env")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
        {
            match value {
                Value::Null => {}
                other => {
                    let expanded = expand(&other, base);
                    vars.insert(key, json!(expanded.as_str().unwrap_or_default()));
                }
            }
        }
        let mut cleared = Vec::new();
        for (key, value) in op
            .get("env")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
        {
            if value.is_null() {
                cleared.push(json!(key));
            }
        }
        json!({"env": vars, "clear_env": cleared})
    };
    let (host, _config_root) = scenario_host(&host_spec);

    let send_calls = Arc::new(Mutex::new(Vec::<Value>::new()));
    let raw_calls = Arc::new(Mutex::new(Vec::<Value>::new()));
    let seam_calls = Arc::new(Mutex::new(Map::new()));
    let spends = Arc::new(Mutex::new(Vec::<Value>::new()));

    // Canned transport queue (repeat-expanded), recording every request.
    let queue: VecDeque<Value> = {
        let mut items = VecDeque::new();
        for spec in op
            .get("responses")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            for _ in 0..spec.get("repeat").and_then(Value::as_u64).unwrap_or(1) {
                items.push_back(spec.clone());
            }
        }
        items
    };
    let queue = Arc::new(Mutex::new(queue));
    let send_impl = {
        let queue = queue.clone();
        let send_calls = send_calls.clone();
        move |method: &str, url: &str, body: Option<&Value>, _token: &str| {
            send_calls.lock().unwrap().push(
                json!({"method": method, "url": url, "body": body.cloned().unwrap_or(Value::Null)}),
            );
            let spec = queue
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| panic!("transport queue exhausted at {method} {url}"));
            if let Some(raise) = spec.get("raise") {
                return Err(raise_err(raise));
            }
            Ok(spec.get("json").cloned().unwrap_or(Value::Null))
        }
    };
    let send: &CfTransport = &send_impl;

    let raw_spec = op.get("raw");
    let raw_impl = {
        let raw_calls = raw_calls.clone();
        let raw_spec = raw_spec.cloned();
        move |url: &str, _token: &str| {
            raw_calls.lock().unwrap().push(json!({"url": url}));
            let spec = raw_spec.clone().unwrap_or(Value::Null);
            if let Some(raise) = spec.get("raise") {
                return Err(raise_err(raise));
            }
            if spec.get("other").is_some() {
                return Ok(RawReply::Other);
            }
            if let Some(text) = spec.get("text").and_then(Value::as_str) {
                return Ok(RawReply::Text(text.to_owned()));
            }
            if let Some(b64) = spec.get("bytes_b64").and_then(Value::as_str) {
                return Ok(RawReply::Bytes(b64decode(b64)));
            }
            if let Some(rep) = spec.get("byte_repeat").and_then(Value::as_array) {
                let byte = b64decode(rep[0].as_str().unwrap_or_default());
                let n = rep[1].as_u64().unwrap_or(0) as usize;
                return Ok(RawReply::Bytes(byte.repeat(n)));
            }
            Ok(RawReply::Bytes(Vec::new()))
        }
    };
    let raw: &RawTransport = &raw_impl;

    let bindings_lookup = script_lookup(op.get("bindings"), base, &seam_calls, "bindings");
    let history_lookup = script_history(op.get("history"), base, &seam_calls);
    let open_lookup = script_lookup(op.get("open_project"), base, &seam_calls, "open_project");
    let import_impl = script_import(op.get("import_logs"), base, &seam_calls);

    let spend_impl = {
        let spends = spends.clone();
        move |rec: &Record| {
            spends.lock().unwrap().push(spend_json(rec));
            Ok(json!({"ok": true}))
        }
    };
    let spend_sink: &SpendSink = &spend_impl;

    let args = expand(op.get("args").unwrap_or(&Value::Null), base);
    let opt = |key: &str| args.get(key).filter(|v| !v.is_null());
    let limits = op.get("limits").cloned().unwrap_or(Value::Null);

    let stub = StubTransport::new(&[]);
    let (sleeper, _pauses) = Sleeper::recording();
    let home = |p: &str| PathBuf::from(p.replace('~', "$HOME"));

    let fn_name = op.get("fn").and_then(Value::as_str).unwrap_or_default();
    let got: Value = match fn_name {
        "traffic" => {
            let bindings_ref: Option<&BindingsLookup> = bindings_lookup.as_deref();
            let history_ref: Option<&HistorySink> = history_lookup.as_deref();
            let env = TrafficEnv {
                host: &host,
                home: &home,
                bindings: bindings_ref,
                history: history_ref,
                spend: Some(spend_sink),
                today: opt("today").and_then(Value::as_str).map(day_of),
                page_size: limits.get("page_size").and_then(Value::as_i64),
                max_url_facts: limits
                    .get("max_url_facts")
                    .and_then(Value::as_u64)
                    .map(|v| v as usize),
                max_tracked_paths: limits
                    .get("max_tracked_paths")
                    .and_then(Value::as_u64)
                    .map(|v| v as usize),
            };
            let result = cft::traffic(
                &env,
                &stub,
                &sleeper,
                opt("zone"),
                opt("project"),
                opt("include_configuration"),
                opt("since"),
                opt("until"),
                Some(send),
                opt("token"),
            )
            .await;
            outcome(result)
        }
        "logpull" => {
            let bindings_ref: Option<&BindingsLookup> = bindings_lookup.as_deref();
            let open_ref: Option<&ProjectOpener> = open_lookup.as_deref();
            let import_ref: Option<&LogImport> = import_impl.as_deref();
            let env = LogpullEnv {
                host: &host,
                home: &home,
                bindings: bindings_ref,
                open_project: open_ref,
                import_logs: import_ref,
                now: opt("now").and_then(Value::as_str).map(micros_of),
            };
            let result = cft::logpull(
                &env,
                &stub,
                &sleeper,
                opt("project"),
                opt("zone"),
                args.get("start").unwrap_or(&Value::Null),
                args.get("end").unwrap_or(&Value::Null),
                opt("count"),
                opt("token"),
                Some(send),
                Some(raw),
            )
            .await;
            outcome(result)
        }
        "read_token" => outcome(cloudflare::read_token(&host, &home).map(|token| json!(token))),
        "verify_token" => json!({
            "result": cloudflare::verify_token(&host, &home, &stub, &sleeper, opt("token"), Some(send)).await
        }),
        other => panic!("unknown fn {other}"),
    };

    let mut calls = Map::new();
    let recorded_send = send_calls.lock().unwrap().clone();
    if !recorded_send.is_empty() {
        calls.insert("send".to_owned(), json!(recorded_send));
    }
    let recorded_raw = raw_calls.lock().unwrap().clone();
    if !recorded_raw.is_empty() {
        calls.insert("raw".to_owned(), json!(recorded_raw));
    }
    for (name, value) in seam_calls.lock().unwrap().iter() {
        calls.insert(name.clone(), value.clone());
    }
    let spend_list = spends.lock().unwrap().clone();
    (got, calls, spend_list)
}

async fn run_scenario(scenario: &Value, dir: &Path) -> Vec<String> {
    let mut failures = Vec::new();
    let name = scenario
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    for (i, op) in scenario
        .get("ops")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let base = dir.join(format!("op{i}"));
        std::fs::create_dir_all(&base).unwrap_or_default();
        let base = base.to_string_lossy().to_string();
        let (got, calls, spends) = run_op(op, &base).await;

        let want_outcome = op.get("outcome").cloned().unwrap_or(Value::Null);
        let got = scrub(&got, &base);
        if got != want_outcome {
            failures.push(format!(
                "{name}[{i}] outcome\n  want: {}\n  got:  {}",
                serde_json::to_string(&want_outcome).unwrap_or_default(),
                serde_json::to_string(&got).unwrap_or_default()
            ));
        }
        let want_calls = op.get("calls").cloned().unwrap_or_else(|| json!({}));
        let got_calls = scrub(&Value::Object(calls), &base);
        if got_calls != want_calls {
            failures.push(format!(
                "{name}[{i}] calls\n  want: {}\n  got:  {}",
                serde_json::to_string(&want_calls).unwrap_or_default(),
                serde_json::to_string(&got_calls).unwrap_or_default()
            ));
        }
        let want_spend = op.get("spend").cloned().unwrap_or_else(|| json!([]));
        if json!(spends) != want_spend {
            failures.push(format!(
                "{name}[{i}] spend\n  want: {}\n  got:  {}",
                serde_json::to_string(&want_spend).unwrap_or_default(),
                serde_json::to_string(&json!(spends)).unwrap_or_default()
            ));
        }
    }
    failures
}

#[tokio::test(flavor = "current_thread")]
async fn cloudflare_traffic_oracle() {
    let scenarios = load_fixture("cloudflare_traffic_oracle.json");
    let mut failures = Vec::new();
    let mut ops = 0usize;
    for scenario in &scenarios {
        ops += scenario
            .get("ops")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        let name = scenario
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let dir = sandbox(name);
        failures.extend(run_scenario(scenario, &dir).await);
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert!(
        failures.is_empty(),
        "{} oracle mismatch(es) across {ops} ops:\n{}",
        failures.len(),
        failures.join("\n")
    );
    eprintln!("cloudflare_traffic_oracle: {ops} ops matched");
}

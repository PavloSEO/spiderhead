//! Replay `cloudflare_logs` scenarios recorded by the reference suite.
//! Scripted transport queue, `list_bindings`/`open_project`/`import_logs` seams,
//! scripted Instant Logs socket (recv spec list), cancel flag and `now` ticks;
//! paths stay as `$BASE`/`$HOME`/`$TMP` placeholders like the reference scrubber.

mod common;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};
use spiderhead_sources::cloudflare::{CfError, CfTransport};
use spiderhead_sources::cloudflare_logs::{
    self, InstantSocket, InstantSocketFactory, LogsEnv, RecvError, WsMessage,
};
use spiderhead_sources::cloudflare_traffic::{BindingsLookup, LogImport, ProjectOpener};
use spiderhead_sources::http::Sleeper;

use common::{load_fixture, scenario_host, StubTransport};

fn expand(value: &Value, base: &str) -> Value {
    match value {
        Value::String(text) => json!(text.replace("$BASE", base)),
        Value::Array(items) => json!(items.iter().map(|v| expand(v, base)).collect::<Vec<_>>()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), expand(v, base)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn scrub(value: &Value, base: &str) -> Value {
    match value {
        Value::String(text) => json!(text.replace(base, "$BASE")),
        Value::Array(items) => json!(items.iter().map(|v| scrub(v, base)).collect::<Vec<_>>()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), scrub(v, base)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn sandbox(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "sh-logs-oracle-{}-{}",
        std::process::id(),
        name.replace(|c: char| !c.is_alphanumeric(), "_")
    ))
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
            if let Some(b64) = list
                .get(1)
                .and_then(Value::as_array)
                .filter(|a| a.first().and_then(Value::as_str) == Some("b64"))
            {
                std::fs::write(&path, b64decode(b64[1].as_str().unwrap_or_default()))
                    .unwrap_or_default();
            } else {
                std::fs::write(&path, list[1].as_str().unwrap_or_default()).unwrap_or_default();
            }
            if let Some(mode) = list.get(2).and_then(Value::as_u64) {
                let _ = spiderhead_core::filesystem::set_permissions_mode(&path, mode as u32);
            }
        } else if let Some(pair) = step.get("symlink").and_then(Value::as_array) {
            let src = expand(&pair[0], base);
            let dst = expand(&pair[1], base);
            let _ = (&src, &dst);
            #[cfg(unix)]
            let _ = std::os::unix::fs::symlink(
                src.as_str().unwrap_or_default(),
                dst.as_str().unwrap_or_default(),
            );
        } else if let Some(pair) = step.get("hardlink").and_then(Value::as_array) {
            let src = expand(&pair[0], base);
            let dst = expand(&pair[1], base);
            let _ = std::fs::hard_link(
                src.as_str().unwrap_or_default(),
                dst.as_str().unwrap_or_default(),
            );
        }
    }
}

fn raise_err(spec: &Value) -> CfError {
    let class = spec
        .get("class")
        .and_then(Value::as_str)
        .unwrap_or("OSError");
    let message = spec
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    match class {
        "CloudflareError" => CfError::Cloudflare(message),
        "ValueError" => CfError::Value(message),
        "OSError" | "URLError" | "TimeoutError" => CfError::Os(message),
        "KeyError" => CfError::Python("KeyError", spiderhead_sources::pystr::repr_str(&message)),
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
type BoxedImport<'a> = dyn Fn(&str, &str, i64) -> Result<Value, CfError> + Send + Sync + 'a;

/// A `{"json"|"raise"}` seam spec → the lookup closure; `None` leaves it unset.
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

/// A scripted `InstantSocket` driven by the `{"recv": [...]}` spec list.
struct ScriptedSocket {
    spec: VecDeque<Value>,
    calls: Arc<Mutex<Map<String, Value>>>,
    recv_count: Arc<Mutex<u64>>,
}

impl InstantSocket for ScriptedSocket {
    fn recv(&mut self, timeout: f64) -> Result<WsMessage, RecvError> {
        self.calls
            .lock()
            .unwrap()
            .entry("recv".to_owned())
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .unwrap()
            .push(json!(timeout));
        *self.recv_count.lock().unwrap() += 1;
        let Some(spec) = self.spec.pop_front() else {
            return Err(RecvError::Timeout);
        };
        if spec.get("timeout").is_some() {
            return Err(RecvError::Timeout);
        }
        if spec.get("closed").is_some() {
            return Ok(WsMessage::Closed);
        }
        if spec.get("other").is_some() {
            return Ok(WsMessage::Other);
        }
        if spec.get("raise").is_some() {
            return Err(RecvError::Other);
        }
        if let Some(b64) = spec.get("bytes_b64").and_then(Value::as_str) {
            return Ok(WsMessage::Bytes(b64decode(b64)));
        }
        Ok(WsMessage::Text(
            spec.get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        ))
    }

    fn close(&mut self) {
        self.calls
            .lock()
            .unwrap()
            .insert("closed".to_owned(), json!(true));
    }
}

#[allow(clippy::too_many_lines)]
async fn run_op(op: &Value, base: &str) -> (Value, Map<String, Value>) {
    run_fs(op.get("fs"), base);

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

    let seam_calls = Arc::new(Mutex::new(Map::new()));

    // Canned transport queue — the `resolve_zone_id` spec becomes the first
    // response (`{"success": true, "result": <row>}`) like the generator.
    let queue: VecDeque<Value> = {
        let mut items = VecDeque::new();
        if let Some(rzi) = op.get("resolve_zone_id") {
            if rzi.get("raise").is_some() {
                items.push_back(json!({"raise": rzi.get("raise").cloned().unwrap_or(Value::Null)}));
            } else {
                items.push_back(
                    json!({"json": {"success": true, "result": rzi.get("json").cloned().unwrap_or(Value::Null)}}),
                );
            }
        }
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
        let seam_calls = seam_calls.clone();
        move |method: &str, url: &str, body: Option<&Value>, _token: &str| {
            seam_calls
                .lock()
                .unwrap()
                .entry("send".to_owned())
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .unwrap()
                .push(json!({"method": method, "url": url, "body": body.cloned().unwrap_or(Value::Null)}));
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

    let bindings_lookup = script_lookup(op.get("bindings"), base, &seam_calls, "bindings");
    let open_lookup = script_lookup(op.get("open_project"), base, &seam_calls, "open_project");
    let import_impl = script_import(op.get("import_logs"), base, &seam_calls);

    let bindings_ref: Option<&BindingsLookup> = bindings_lookup.as_deref();
    let open_ref: Option<&ProjectOpener> = open_lookup.as_deref();
    let import_ref: Option<&LogImport> = import_impl.as_deref();
    let home = |p: &str| PathBuf::from(p.replace('~', "$HOME"));
    let env = LogsEnv {
        host: &host,
        home: &home,
        bindings: bindings_ref,
        open_project: open_ref,
        import_logs: import_ref,
    };

    let args = expand(op.get("args").unwrap_or(&Value::Null), base);
    let opt = |key: &str| args.get(key).filter(|v| !v.is_null());

    let stub = StubTransport::new(&[]);
    let (sleeper, _pauses) = Sleeper::recording();

    let fn_name = op.get("fn").and_then(Value::as_str).unwrap_or_default();
    let got: Value = match fn_name {
        "import_logpush_file" => outcome(cloudflare_logs::import_logpush_file(
            &env,
            args.get("input_path").unwrap_or(&Value::Null),
            args.get("project").unwrap_or(&Value::Null),
            opt("max_events"),
        )),
        "instant_logs_capture" => {
            let socket_spec = op.get("socket").cloned().unwrap_or(Value::Null);
            let recv_count = Arc::new(Mutex::new(0u64));
            let factory_impl = {
                let seam_calls = seam_calls.clone();
                let socket_spec = socket_spec.clone();
                let recv_count = recv_count.clone();
                move |url: &str, kwargs: &Map<String, Value>| {
                    seam_calls
                        .lock()
                        .unwrap()
                        .entry("factory".to_owned())
                        .or_insert_with(|| json!([]))
                        .as_array_mut()
                        .unwrap()
                        .push(json!({"url": url, "kwargs": kwargs}));
                    if let Some(raise) = socket_spec.get("factory_raise") {
                        return Err(raise_err(raise));
                    }
                    let spec: VecDeque<Value> = socket_spec
                        .get("recv")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .collect();
                    Ok(Box::new(ScriptedSocket {
                        spec,
                        calls: seam_calls.clone(),
                        recv_count: recv_count.clone(),
                    }) as Box<dyn InstantSocket>)
                }
            };
            // `absent` means "use the real factory" in the venv — scripted,
            // never reached before the unavailable branch anyway.
            let factory_ref: Option<&InstantSocketFactory> = Some(&factory_impl);
            let cancel_spec = op.get("cancel").cloned();
            let cancel_impl = {
                let recv_count = recv_count.clone();
                let cancel_spec = cancel_spec.clone();
                move || {
                    let spec = cancel_spec.clone().unwrap_or(Value::Null);
                    if spec
                        .get("initial")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    {
                        return true;
                    }
                    match spec.get("set_after_recv").and_then(Value::as_u64) {
                        Some(n) => *recv_count.lock().unwrap() >= n,
                        None => false,
                    }
                }
            };
            let ticks = Arc::new(Mutex::new(
                op.get("now")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .collect::<VecDeque<Value>>(),
            ));
            let clock_impl = {
                let seam_calls = seam_calls.clone();
                let ticks = ticks.clone();
                move || {
                    let value = ticks
                        .lock()
                        .unwrap()
                        .pop_front()
                        .and_then(|v| v.as_f64())
                        .unwrap_or(f64::MAX);
                    seam_calls
                        .lock()
                        .unwrap()
                        .entry("clock".to_owned())
                        .or_insert_with(|| json!([]))
                        .as_array_mut()
                        .unwrap()
                        .push(json!(value));
                    value
                }
            };
            let result = cloudflare_logs::instant_logs_capture(
                &env,
                &stub,
                &sleeper,
                args.get("project").unwrap_or(&Value::Null),
                opt("confirm_live_capture"),
                opt("max_seconds"),
                opt("max_events"),
                opt("max_bytes"),
                opt("sample"),
                opt("token"),
                Some(send),
                factory_ref,
                cancel_spec
                    .as_ref()
                    .map(|_| &cancel_impl as &(dyn Fn() -> bool + Send + Sync)),
                if op.get("now").is_some() {
                    Some(&clock_impl as &(dyn Fn() -> f64 + Send + Sync))
                } else {
                    None
                },
            )
            .await;
            outcome(result)
        }
        other => panic!("unknown fn {other}"),
    };

    let calls = seam_calls.lock().unwrap().clone();
    (got, calls)
}

#[tokio::test(flavor = "current_thread")]
async fn cloudflare_logs_oracle() {
    let scenarios = load_fixture("cloudflare_logs_oracle.json");
    let mut failures = Vec::new();
    let mut ops = 0usize;
    for scenario in &scenarios {
        let name = scenario
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let dir = sandbox(name);
        for (i, op) in scenario
            .get("ops")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            ops += 1;
            let base = dir.join(format!("op{i}"));
            std::fs::create_dir_all(&base).unwrap_or_default();
            let base = base.to_string_lossy().to_string();
            let (got, calls) = run_op(op, &base).await;
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
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert!(
        failures.is_empty(),
        "{} oracle mismatch(es) across {ops} ops:\n{}",
        failures.len(),
        failures.join("\n")
    );
    eprintln!("cloudflare_logs_oracle: {ops} ops matched");
}

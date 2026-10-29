//! Shared harness for oracle-fixture tests: a stub transport that replays the recorded
//! responses and captures the requests a client would send, then diffs both sides.
//!
//! Every oracle test compiles this module; individual helpers stay unused in some files.

#![allow(dead_code)]

use serde_json::Value;
use spiderhead_sources::http::{Redirects, Request, Response, Transport, TransportError};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// One canned transport answer from the fixture's `responses` queue.
#[derive(Debug)]
enum Canned {
    /// Serve this HTTP answer.
    Serve(Response),
    /// The transport fails before any answer (timeout/reset/dns).
    Fail(String),
}

/// Replays queued responses; records every [`Request`] a client sends. Redirect-following
/// requests consume further queued answers internally, like `urlopen` does.
#[derive(Debug)]
pub struct StubTransport {
    responses: Mutex<VecDeque<Value>>,
    requests: Mutex<Vec<Value>>,
}

impl StubTransport {
    /// Build from the fixture's `responses` array.
    pub fn new(responses: &[Value]) -> Self {
        StubTransport {
            responses: Mutex::new(responses.iter().cloned().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// The recorded logical requests as JSON values (`method`, `url`, `headers`, `body`,
    /// `timeout`) in the same shape the reference suite writes.
    pub fn recorded(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }

    fn pop(&self) -> Canned {
        let mut queue = self.responses.lock().unwrap();
        let spec = queue.pop_front().unwrap_or_else(
            || serde_json::json!({"status": 500, "body": "stub: out of responses"}),
        );
        drop(queue);
        if let Some(kind) = spec.get("transport_error").and_then(Value::as_str) {
            return Canned::Fail(kind.to_owned());
        }
        let status = spec.get("status").and_then(Value::as_u64).unwrap_or(200) as u16;
        let headers = spec
            .get("headers")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.to_lowercase(), v.as_str().unwrap_or_default().to_owned()))
                    .collect()
            })
            .unwrap_or_default();
        let body = if let Some(n) = spec.get("body_zeros").and_then(Value::as_u64) {
            vec![0u8; n as usize]
        } else {
            match spec.get("body") {
                Some(Value::String(s)) => s.clone().into_bytes(),
                Some(Value::Object(m)) if m.contains_key("$b64") => {
                    b64_decode(m.get("$b64").and_then(Value::as_str).unwrap_or_default())
                }
                Some(other) => spiderhead_sources::pyjson::dumps(other).into_bytes(),
                None => Vec::new(),
            }
        };
        Canned::Serve(Response {
            status,
            headers,
            body,
        })
    }
}

impl Transport for StubTransport {
    async fn send(&self, request: &Request) -> Result<Response, TransportError> {
        // reference fixtures store `timeout` as an int (`timeout=30`); render integral seconds
        // as an integer so the JSON comparison does not hinge on `30` vs `30.0`.
        let timeout = if request.timeout.fract() == 0.0 {
            serde_json::json!(request.timeout as i64)
        } else {
            serde_json::json!(request.timeout)
        };
        self.requests.lock().unwrap().push(serde_json::json!({
            "method": request.method,
            "url": request.url,
            "headers": request
                .headers
                .iter()
                .map(|(k, v)| serde_json::json!([k.to_lowercase(), v]))
                .collect::<Vec<Value>>(),
            "body": String::from_utf8_lossy(&request.body),
            "timeout": timeout,
        }));
        let mut hops = 0usize;
        let answer = loop {
            match self.pop() {
                Canned::Fail(kind) => break Err(TransportError::Connect(kind)),
                Canned::Serve(response) => {
                    let is_redirect = (300..400).contains(&response.status)
                        && response.header("location").is_some();
                    if request.redirects == Redirects::Follow && is_redirect && hops < 10 {
                        hops += 1;
                        continue;
                    }
                    if (200..300).contains(&response.status) {
                        break Ok(response);
                    }
                    break Err(TransportError::Http(response));
                }
            }
        };
        answer
    }
}

/// The `tests/fixtures` directory of this crate.
pub fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Load a fixture file as JSON rows.
pub fn load_fixture(name: &str) -> Vec<Value> {
    let path = fixtures_dir().join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("fixture {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("fixture {}: {e}", path.display()))
}

/// A credential/config root that never exists (scenario `isolate_config`).
pub fn absent_config_root() -> PathBuf {
    PathBuf::from("/nonexistent-oracle-config-root")
}

/// Build the host environment for a scenario: `env` vars set, `clear_env` absent. The
/// config root is always the absent one — file-backed credentials never exist in oracle
/// scenarios (values come from `env`, and missing-credential cases must not see real files).
pub fn scenario_host(scenario: &Value) -> (spiderhead_core::host::FakeHost, PathBuf) {
    let mut env = std::collections::BTreeMap::new();
    if let Some(vars) = scenario.get("env").and_then(Value::as_object) {
        for (key, value) in vars {
            env.insert(key.clone(), value.as_str().unwrap_or_default().to_owned());
        }
    }
    for key in scenario
        .get("clear_env")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        env.remove(key.as_str().unwrap_or_default());
    }
    let host = spiderhead_core::host::FakeHost {
        env,
        ..Default::default()
    };
    (host, absent_config_root())
}

/// Compare the recorded requests + outcome of one scenario with the oracle row; panics with
/// a readable diff on any mismatch. `actual` is `Ok(value)` or `Err((python_class, message))`.
pub fn assert_scenario(
    scenario: &Value,
    transport: &StubTransport,
    pauses: &[f64],
    actual: Result<Value, (String, String)>,
) {
    let expect = scenario.get("expect").expect("scenario.expect");
    let name = scenario.get("name").and_then(Value::as_str).unwrap_or("?");

    let want_requests = expect.get("requests").cloned().unwrap_or_default();
    let got_requests = Value::Array(transport.recorded());
    assert_requests_eq(name, &want_requests, &got_requests);

    if let Some(want_pauses) = expect.get("pauses").and_then(Value::as_array) {
        let want: Vec<f64> = want_pauses.iter().filter_map(Value::as_f64).collect();
        assert_eq!(pauses, &want[..], "{name}: pauses differ");
    }

    match (expect.get("result"), expect.get("error"), actual) {
        (Some(want), _, Ok(got)) => {
            assert_eq!(
                &got, want,
                "{name}: result differs\nwant: {want}\n got: {got}"
            );
        }
        (_, Some(want), Err((class, message))) => {
            let got = serde_json::json!({"class": class, "message": message});
            assert_eq!(
                &got, want,
                "{name}: error differs\nwant: {want}\n got: {got}"
            );
        }
        (Some(want), _, Err((class, message))) => {
            panic!("{name}: expected result {want}, got error {class}: {message}")
        }
        (_, Some(want), Ok(got)) => {
            panic!("{name}: expected error {want}, got result {got}")
        }
        (None, None, _) => panic!("{name}: fixture has neither result nor error"),
    }
}

/// Compare recorded request lists field-wise. `body` compares as parsed JSON because the
/// Rust port stores objects in a `BTreeMap`-ordered `Map` while reference `json.dumps` emits
/// insertion order — the wire bytes differ only in member order, which is not meaningful
/// to the receiving API.
fn b64_decode(text: &str) -> Vec<u8> {
    fn val(b: u8) -> u8 {
        match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => 0,
        }
    }
    let bytes: Vec<u8> = text.bytes().filter(|b| *b != b'=').collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let n = chunk
            .iter()
            .fold(0u32, |acc, b| (acc << 6) | val(*b) as u32)
            << (6 * (4 - chunk.len()));
        let take = chunk.len().saturating_sub(1);
        for i in 0..take {
            out.push((n >> (16 - i * 8)) as u8);
        }
    }
    out
}

/// Compare recorded request lists field-wise. `body` compares as parsed JSON because the
/// Rust port stores objects in a `BTreeMap`-ordered `Map` while reference `json.dumps` emits
/// insertion order — the wire bytes differ only in member order, which is not meaningful
/// to the receiving API.
fn assert_requests_eq(name: &str, want: &Value, got: &Value) {
    let want = want.as_array().cloned().unwrap_or_default();
    let got = got.as_array().cloned().unwrap_or_default();
    assert_eq!(
        want.len(),
        got.len(),
        "{name}: request count differs\nwant: {want:?}\n got: {got:?}"
    );
    for (index, (want_req, got_req)) in want.iter().zip(&got).enumerate() {
        for field in ["method", "url", "headers", "timeout"] {
            assert_eq!(
                want_req.get(field),
                got_req.get(field),
                "{name}: request {index} field {field} differs"
            );
        }
        let want_body = want_req
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let got_body = got_req
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match (
            serde_json::from_str::<Value>(want_body),
            serde_json::from_str::<Value>(got_body),
        ) {
            (Ok(w), Ok(g)) => assert_eq!(
                w, g,
                "{name}: request {index} body differs\nwant: {want_body}\n got: {got_body}"
            ),
            _ => assert_eq!(want_body, got_body, "{name}: request {index} body differs"),
        }
    }
}

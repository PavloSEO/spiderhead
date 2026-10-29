//! Shared helpers for the stdio integration tests.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, BufWriter, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};
use spiderhead_oracle_diff::{diff, Divergence, Options};

/// The binary under test (cargo wires `CARGO_BIN_EXE_*` for integration tests).
const BINARY: &str = env!("CARGO_BIN_EXE_spiderhead-mcp");

/// A line-JSON-RPC client speaking to one spawned server process.
pub struct Client {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Client {
    /// Start `spiderhead-mcp --profile <profile>`.
    pub fn spawn(profile: &str) -> Client {
        let mut child = Command::new(BINARY)
            .arg("--profile")
            .arg(profile)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spiderhead-mcp binary must start");
        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        Client {
            child,
            stdin: BufWriter::new(stdin),
            stdout: BufReader::new(stdout),
            next_id: 1,
        }
    }

    fn send(&mut self, message: &Value) {
        serde_json::to_writer(&mut self.stdin, message).expect("write to the server");
        self.stdin.write_all(b"\n").expect("newline to the server");
        self.stdin.flush().expect("flush to the server");
    }

    fn receive(&mut self) -> Value {
        let mut line = String::new();
        let read = self
            .stdout
            .read_line(&mut line)
            .expect("read from the server");
        assert!(
            read > 0,
            "server closed the stream: {:?}",
            self.child.try_wait()
        );
        serde_json::from_str(&line).expect("server speaks line JSON")
    }

    /// Perform the initialize/initialized handshake, returning the `initialize` result.
    pub fn initialize(&mut self) -> Value {
        self.send(&json!({
            "jsonrpc": "2.0", "id": self.next_id, "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "parity-test", "version": "0"}
            }
        }));
        self.next_id += 1;
        let reply = self.receive();
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        reply["result"].clone()
    }

    /// Send one request and return the full JSON-RPC response message.
    pub fn request(&mut self, method: &str, params: Value) -> Value {
        self.send(&json!({
            "jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params
        }));
        self.next_id += 1;
        self.receive()
    }

    /// `tools/call` returning the raw response message (errors stay in `result`).
    pub fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name": name, "arguments": arguments}))
    }

    /// Send a notification (no reply expected) and do not read anything.
    pub fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// Send a line that is not JSON at all; returns whatever the server emits.
    pub fn send_raw_and_receive(&mut self, line: &str) -> Value {
        self.stdin.write_all(line.as_bytes()).expect("raw write");
        self.stdin.write_all(b"\n").expect("raw newline");
        self.stdin.flush().expect("raw flush");
        self.receive()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Load a fixture saved by the reference suite.
pub fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture file"))
        .expect("fixture JSON")
}

/// Apply the fixture normalisation rule: `{VERSION}` back to the served constant.
pub fn substitute_version(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(text.replace("{VERSION}", "1.30.0")),
        Value::Array(items) => Value::Array(items.iter().map(substitute_version).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, item)| (key.clone(), substitute_version(item)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Structural diff against an oracle fixture; panics with a report if not empty.
pub fn assert_matches_fixture(expected: &Value, actual: &Value, context: &str) {
    let divergences = diff(expected, actual, &Options::default());
    assert!(
        divergences.is_empty(),
        "{context}: {} divergence(s)\n{}",
        divergences.len(),
        report(&divergences)
    );
}

fn report(divergences: &[Divergence]) -> String {
    divergences
        .iter()
        .map(|d| {
            format!(
                "  {} {:?}: expected {:?}, got {:?}\n",
                d.path, d.kind, d.expected, d.actual
            )
        })
        .collect()
}

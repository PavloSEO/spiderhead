//! Integration test: the Rust project workspace commands reproduce the reference
//! fixtures captured by the reference suite — one lifecycle chain
//! (new → open → status → update → bindings), on both surfaces, in fresh work
//! directories per surface.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::{json, Value};
use spiderhead_cli::{run, Output};

/// The oracle's normalisation without a fixture server; both the plain and the
/// canonical (/private/var on macOS) spellings of the work path fold.
struct Norm {
    /// (plain, canonical) spellings of every work dir seen so far; the capture
    /// numbers UUIDs through one shared Norm across both surfaces.
    works: std::cell::RefCell<Vec<(String, String)>>,
    uuids: BTreeMap<String, String>,
    uuid_re: Regex,
    evid_re: Regex,
    ts_re: Regex,
    as_of_re: Regex,
    duration_re: Regex,
}

impl Norm {
    fn new(work: &Path) -> Norm {
        Norm {
            works: std::cell::RefCell::new(vec![(
                work.to_string_lossy().into_owned(),
                work.canonicalize()
                    .unwrap_or_else(|_| work.to_path_buf())
                    .to_string_lossy()
                    .into_owned(),
            )]),
            uuids: BTreeMap::new(),
            uuid_re: Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
                .expect("uuid regex"),
            evid_re: Regex::new(r"(evidence:v1:(<UUID:\d+)>:\w+:)[0-9a-f]{24}")
                .expect("evid regex"),
            ts_re: Regex::new(r"\d{4}-\d\d-\d\d[T ]\d\d:\d\d:\d\d(\.\d+)?(Z|[+-]\d\d:?\d\d)?")
                .expect("ts regex"),
            as_of_re: Regex::new(r#"("as_of": ")\d{4}-\d\d-\d\d"#).expect("as_of regex"),
            duration_re: Regex::new(concat!(
                r#"("(?:crawl_delay_applied_at|delay_seconds|duration|duration_ms|elapsed_ms|"#,
                r#"elapsed_seconds|latency_ms|response_time|time_ms|total_ms|ttfb_ms)": )"#,
                r"-?[0-9][0-9.eE+-]*",
            ))
            .expect("duration regex"),
        }
    }

    /// Register one more work directory (the MCP phase's own).
    fn add_work(&self, work: &Path) {
        self.works.borrow_mut().push((
            work.to_string_lossy().into_owned(),
            work.canonicalize()
                .unwrap_or_else(|_| work.to_path_buf())
                .to_string_lossy()
                .into_owned(),
        ));
    }

    /// Fixture side of the `as_of` fold.
    fn expect(&self, text: &str) -> String {
        self.as_of_re.replace_all(text, "${1}<DATE>").into_owned()
    }

    fn s(&mut self, text: &str) -> String {
        let mut text = text.to_owned();
        for (plain, canonical) in self.works.borrow().iter() {
            text = text.replace(canonical, "<WORK>").replace(plain, "<WORK>");
        }
        let mut uuids: Vec<(String, String)> = Vec::new();
        for capture in self.uuid_re.find_iter(&text) {
            let found = capture.as_str().to_owned();
            let len = self.uuids.len();
            let token = format!("<UUID:{}>", len + 1);
            self.uuids.entry(found.clone()).or_insert(token);
            uuids.push((found.clone(), self.uuids[&found].clone()));
        }
        for (found, token) in uuids {
            text = text.replace(&found, &token);
        }
        let text = self.evid_re.replace_all(&text, "${1}<HASH>").into_owned();
        let text = self.ts_re.replace_all(&text, "<TS>").into_owned();
        // `as_of` is the capture day (today); fold it so the test is date-free.
        let text = self.as_of_re.replace_all(&text, "${1}<DATE>").into_owned();
        self.duration_re
            .replace_all(&text, "${1}<DURATION>")
            .into_owned()
    }
}

fn fixture() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/project/project.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture file"))
        .expect("fixture json")
}

fn unified_diff(field: &str, expected: &str, actual: &str) -> String {
    let mut diff = String::new();
    let mut expected_lines = expected.lines().peekable();
    let mut actual_lines = actual.lines().peekable();
    let mut line_no = 0;
    while expected_lines.peek().is_some() || actual_lines.peek().is_some() {
        line_no += 1;
        let left = expected_lines.next().unwrap_or("<end>");
        let right = actual_lines.next().unwrap_or("<end>");
        if left != right {
            diff += &format!("  {field}:{line_no}\n  - {left}\n  + {right}\n");
            if diff.len() > 3000 {
                diff += "  …\n";
                break;
            }
        }
    }
    diff
}

fn fresh_work(tag: &str) -> PathBuf {
    let work = std::env::temp_dir().join(format!("sh-project-parity-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("work dir");
    work
}

#[test]
fn project_group_matches_the_reference_cli_and_mcp() {
    let fixture = fixture();

    // ── CLI: the chain replays through the real binary in its own work dir ──
    let cli_work = fresh_work("cli");
    let registry = spiderhead_cli::registry();
    let mut norm = Norm::new(&cli_work);
    // The chain order matters: the fixture carries the step explicitly
    // (the JSON file itself is sorted).
    let mut cases: Vec<(String, &Value)> = fixture["cases"]
        .as_object()
        .expect("cases")
        .iter()
        .map(|(name, case)| (name.clone(), case))
        .collect();
    cases.sort_by_key(|(_, case)| case["step"].as_i64().unwrap_or_default());
    for (name, case) in &cases {
        let argv: Vec<String> = case["argv"]
            .as_array()
            .expect("argv")
            .iter()
            .map(|part| {
                part.as_str()
                    .unwrap_or_default()
                    .replace("{WORK}", &cli_work.to_string_lossy())
            })
            .collect();
        let mut out = Output::default();
        let code = run(&argv, &registry, &mut out).expect("handler run");
        let mut problems = String::new();
        if i64::from(code.0) != case["exit"].as_i64().unwrap_or_default() {
            problems += &format!("  exit: expected {}, got {}\n", case["exit"], code.0);
        }
        let actual_stdout = norm.s(&out.stdout);
        let expected_stdout = norm.expect(case["stdout"].as_str().unwrap_or_default());
        if actual_stdout != expected_stdout {
            problems += &unified_diff("stdout", &expected_stdout, &actual_stdout);
        }
        let actual_stderr = norm.s(&out.stderr);
        let expected_stderr = norm.expect(case["stderr"].as_str().unwrap_or_default());
        if actual_stderr != expected_stderr {
            problems += &unified_diff("stderr", &expected_stderr, &actual_stderr);
        }
        assert!(problems.is_empty(), "{name} diverges:\n{problems}");
    }
    let _ = std::fs::remove_dir_all(&cli_work);

    // ── MCP: the same chain through the in-process server, fresh work dir ───
    use spiderhead_mcp::profile::Profile;
    use spiderhead_mcp::server::Server;

    let mcp_work = fresh_work("mcp");
    norm.add_work(&mcp_work);
    let mcp = Server::new(Profile::Full);
    let mut out: Vec<Value> = Vec::new();
    mcp.handle_line(
        r#"{"jsonrpc":"2.0","id":900,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"parity","version":"0"}}}"#,
        &mut out,
    );
    mcp.handle_line(
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        &mut out,
    );
    let mut calls: Vec<(String, &Value)> = fixture["mcp"]
        .as_object()
        .expect("mcp")
        .iter()
        .map(|(name, call)| (name.clone(), call))
        .collect();
    calls.sort_by_key(|(_, call)| call["step"].as_i64().unwrap_or_default());
    for (index, (name, call)) in calls.iter().enumerate() {
        let arguments = resolve(&call["arguments"], &mcp_work);
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1000 + index,
            "method": "tools/call",
            "params": {"name": call["tool"], "arguments": arguments},
        });
        let mut replies: Vec<Value> = Vec::new();
        mcp.handle_line(&request.to_string(), &mut replies);
        let reply = replies
            .iter()
            .find(|reply| reply.get("id") == Some(&json!(1000 + index)))
            .unwrap_or_else(|| panic!("{name}: no reply"));
        let result = &reply["result"];
        let actual_is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let actual_text = result["content"][0]["text"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_default();
        let mut problems = String::new();
        let expected_is_error = call["is_error"].as_bool().unwrap_or(false);
        if actual_is_error != expected_is_error {
            problems +=
                &format!("  is_error: expected {expected_is_error}, got {actual_is_error}\n");
        }
        let normalized = norm.s(&actual_text);
        let expected_text = norm.expect(call["text"].as_str().unwrap_or_default());
        if normalized != expected_text {
            problems += &unified_diff("text", &expected_text, &normalized);
        }
        assert!(problems.is_empty(), "{name} diverges:\n{problems}");
    }
    let _ = std::fs::remove_dir_all(&mcp_work);
}

/// `{WORK}` placeholders in recorded arguments resolve to the surface's work dir.
fn resolve(arguments: &Value, work: &Path) -> Value {
    match arguments {
        Value::String(text) => Value::String(text.replace("{WORK}", &work.to_string_lossy())),
        Value::Array(items) => Value::Array(items.iter().map(|item| resolve(item, work)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), resolve(value, work)))
                .collect(),
        ),
        other => other.clone(),
    }
}

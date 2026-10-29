//! Integration test: the Rust offline reanalysis (derive from a captured scan,
//! re-parse retained documents, audit the derived artifact) reproduces the
//! reference fixtures captured by the reference suite — a fixture-server
//! crawl followed by the derived scan, on both surfaces.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::{json, Value};

use spiderhead_cli::{run, Output};
use spiderhead_oracle_testkit::FixtureServer;

/// The oracle's normalisation for one fixture-server run.
struct Norm {
    base: String,
    works: std::cell::RefCell<Vec<(String, String)>>,
    uuids: BTreeMap<String, String>,
    uuid_re: Regex,
    ts_re: Regex,
    port_re: Regex,
    duration_re: Regex,
}

impl Norm {
    fn new(base: &str, work: &Path) -> Norm {
        Norm {
            base: base.to_owned(),
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
            ts_re: Regex::new(r"\d{4}-\d\d-\d\d[T ]\d\d:\d\d:\d\d(\.\d+)?(Z|[+-]\d\d:?\d\d)?")
                .expect("ts regex"),
            port_re: Regex::new(r"127\.0\.0\.1:\d+").expect("port regex"),
            duration_re: Regex::new(concat!(
                r#"("(?:crawl_delay_applied_at|delay_seconds|duration|duration_ms|elapsed_ms|"#,
                r#"elapsed_seconds|latency_ms|response_time|time_ms|total_ms|ttfb_ms)": )"#,
                r"-?[0-9][0-9.eE+-]*",
            ))
            .expect("duration regex"),
        }
    }

    fn add_work(&self, work: &Path) {
        self.works.borrow_mut().push((
            work.to_string_lossy().into_owned(),
            work.canonicalize()
                .unwrap_or_else(|_| work.to_path_buf())
                .to_string_lossy()
                .into_owned(),
        ));
    }

    fn s(&mut self, text: &str) -> String {
        let mut text = text.replace(&self.base, "{BASE}");
        for (plain, canonical) in self.works.borrow().iter() {
            text = text.replace(canonical, "<WORK>").replace(plain, "<WORK>");
        }
        let text = self
            .port_re
            .replace_all(&text, "127.0.0.1:{PORT}")
            .into_owned();
        let mut text = text;
        let mut uuids: Vec<(String, String)> = Vec::new();
        for capture in self.uuid_re.find_iter(&text) {
            let found = capture.as_str().to_owned();
            if !self.uuids.contains_key(&found) {
                let token = format!("<UUID:{}>", self.uuids.len() + 1);
                self.uuids.insert(found.clone(), token);
            }
            uuids.push((found.clone(), self.uuids[&found].clone()));
        }
        for (found, token) in uuids {
            text = text.replace(&found, &token);
        }
        let text = self.ts_re.replace_all(&text, "<TS>").into_owned();
        self.duration_re
            .replace_all(&text, "${1}<DURATION>")
            .into_owned()
    }
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
    let work =
        std::env::temp_dir().join(format!("sh-reanalyze-parity-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("work dir");
    work
}

#[test]
fn reanalysis_matches_the_reference_cli_and_mcp() {
    std::env::set_var("SEOHEAD_ALLOW_PRIVATE_HOSTS", "127.0.0.1");
    let raw = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reanalysis/reanalysis.json"),
    )
    .expect("fixture file");
    let fixture: Value = serde_json::from_str(&raw).expect("fixture json");

    // ── CLI: crawl the fixture site, derive, read the derived scan ──────────
    let server = FixtureServer::start("basic");
    let work = fresh_work("cli");
    let registry = spiderhead_cli::registry();
    let mut norm = Norm::new(&server.base, &work);
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
                    .replace("{WORK}", &work.to_string_lossy())
                    .replace("{BASE}", &server.base)
            })
            .collect();
        let mut out = Output::default();
        let code = run(&argv, &registry, &mut out).expect("handler run");
        let mut problems = String::new();
        if i64::from(code.0) != case["exit"].as_i64().unwrap_or_default() {
            problems += &format!("  exit: expected {}, got {}\n", case["exit"], code.0);
        }
        let actual_stdout = norm.s(&out.stdout);
        let expected_stdout = case["stdout"].as_str().unwrap_or_default();
        if actual_stdout != expected_stdout {
            problems += &unified_diff("stdout", expected_stdout, &actual_stdout);
        }
        let actual_stderr = norm.s(&out.stderr);
        let expected_stderr = case["stderr"].as_str().unwrap_or_default();
        if actual_stderr != expected_stderr {
            problems += &unified_diff("stderr", expected_stderr, &actual_stderr);
        }
        assert!(problems.is_empty(), "{name} diverges:\n{problems}");
    }
    let _ = std::fs::remove_dir_all(&work);

    // ── MCP: the same chain through the in-process server, fresh work dir ──
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
        let arguments = call["arguments"]
            .to_string()
            .replace("{WORK}", &mcp_work.to_string_lossy())
            .replace("{BASE}", &server.base);
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1000 + index,
            "method": "tools/call",
            "params": {
                "name": call["tool"],
                "arguments": serde_json::from_str::<Value>(&arguments).expect("arguments json")
            },
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
        let expected_text = call["text"].as_str().unwrap_or_default();
        if normalized != expected_text {
            problems += &unified_diff("text", expected_text, &normalized);
        }
        assert!(problems.is_empty(), "{name} diverges:\n{problems}");
    }
    let _ = std::fs::remove_dir_all(&mcp_work);
}

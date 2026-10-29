//! Integration test: the Rust sitemap-crawl and crawl-site --sitemap handlers
//! reproduce the reference fixtures captured by the reference suite —
//! explicit documents, an index, robots discovery, the fallback, and a seeded
//! native crawl, on both surfaces.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::Value;
use spiderhead_cli::{run, Output};
use spiderhead_oracle_testkit::FixtureServer;

/// The oracle's normalisation (the reference suite.s`) as a Rust port.
struct Norm {
    base: String,
    work: String,
    uuids: BTreeMap<String, String>,
    uuid_re: Regex,
    evid_re: Regex,
    ts_re: Regex,
    port_re: Regex,
    duration_re: Regex,
}

impl Norm {
    fn new(base: &str, work: &Path) -> Norm {
        Norm {
            base: base.to_owned(),
            work: work.to_string_lossy().into_owned(),
            uuids: BTreeMap::new(),
            uuid_re: Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
                .expect("uuid regex"),
            evid_re: Regex::new(r"(evidence:v1:(<UUID:\d+)>:\w+:)[0-9a-f]{24}")
                .expect("evid regex"),
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

    fn s(&mut self, text: &str) -> String {
        let text = text
            .replace(&self.base, "{BASE}")
            .replace(&self.work, "<WORK>");
        let text = self
            .port_re
            .replace_all(&text, "127.0.0.1:{PORT}")
            .into_owned();
        let mut uuids: Vec<(String, String)> = Vec::new();
        for capture in self.uuid_re.find_iter(&text) {
            let found = capture.as_str().to_owned();
            let len = self.uuids.len();
            let token = format!("<UUID:{}>", len + 1);
            self.uuids.entry(found.clone()).or_insert(token);
            uuids.push((found.clone(), self.uuids[&found].clone()));
        }
        let mut text = text;
        for (found, token) in uuids {
            text = text.replace(&found, &token);
        }
        let text = self.evid_re.replace_all(&text, "${1}<HASH>").into_owned();
        let text = self.ts_re.replace_all(&text, "<TS>").into_owned();
        self.duration_re
            .replace_all(&text, "${1}<DURATION>")
            .into_owned()
    }
}

fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sitemap")
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
            if diff.len() > 4000 {
                diff += "  …\n";
                break;
            }
        }
    }
    diff
}

/// One site: every CLI fixture replays through the Rust binary.
fn check_cli_site(site: &str) {
    let server = FixtureServer::start(site);
    let work = std::env::temp_dir().join(format!("sh-sitemap-cli-{site}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("work dir");
    let registry = spiderhead_cli::registry();
    let mut norm = Norm::new(&server.base, &work);
    let mut names: Vec<PathBuf> = std::fs::read_dir(fixtures_root().join(site))
        .expect("fixture dir")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension() == Some(std::ffi::OsStr::new("json")))
        .collect();
    names.sort();
    for path in names {
        let raw = std::fs::read_to_string(&path).expect("fixture file");
        let fixture: Value = serde_json::from_str(&raw).expect("fixture json");
        let name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let argv: Vec<String> = fixture["argv"]
            .as_array()
            .expect("argv")
            .iter()
            .map(|part| {
                part.as_str()
                    .unwrap_or_default()
                    .replace("{BASE}", &server.base)
                    .replace("{WORK}", &work.to_string_lossy())
            })
            .collect();
        // Every captured invocation ran in its own reference process.
        spiderhead_store::scan_artifact::forget_validated();
        let mut out = Output::default();
        let code = run(&argv, &registry, &mut out).expect("handler run");
        let mut problems = String::new();
        if i64::from(code.0) != fixture["exit"].as_i64().unwrap_or_default() {
            problems += &format!("  exit: expected {}, got {}\n", fixture["exit"], code.0);
        }
        let actual_stdout = norm.s(&out.stdout);
        let expected_stdout = fixture["stdout"].as_str().unwrap_or_default();
        if actual_stdout != expected_stdout {
            problems += &unified_diff("stdout", expected_stdout, &actual_stdout);
        }
        let actual_stderr = norm.s(&out.stderr);
        let expected_stderr = fixture["stderr"].as_str().unwrap_or_default();
        if actual_stderr != expected_stderr {
            problems += &unified_diff("stderr", expected_stderr, &actual_stderr);
        }
        assert!(
            problems.is_empty(),
            "{site}/{name} diverges from the reference fixture:\n{problems}"
        );
    }
    let _ = std::fs::remove_dir_all(&work);
}

/// One site through the in-process MCP server, fixture order preserved.
fn check_mcp_site(site: &str) {
    use spiderhead_mcp::profile::Profile;
    use spiderhead_mcp::server::Server;

    let server = FixtureServer::start(site);
    let work = std::env::temp_dir().join(format!("sh-sitemap-mcp-{site}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("work dir");
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
    let mut norm = Norm::new(&server.base, &work);
    let dir = fixtures_root().join(site).join("mcp");
    let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("mcp fixture dir")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension() == Some(std::ffi::OsStr::new("json")))
        .collect();
    names.sort();
    for path in names {
        let raw = std::fs::read_to_string(&path).expect("fixture file");
        let fixture: Value = serde_json::from_str(&raw).expect("fixture json");
        let name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let arguments = fixture["arguments"]
            .as_object()
            .expect("arguments")
            .iter()
            .map(|(key, value)| {
                let rendered = match value {
                    Value::String(text) => Value::String(
                        text.replace("{BASE}", &server.base)
                            .replace("{WORK}", &work.to_string_lossy()),
                    ),
                    other => other.clone(),
                };
                (key.clone(), rendered)
            })
            .collect::<serde_json::Map<String, Value>>();
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1000,
            "method": "tools/call",
            "params": {"name": fixture["tool"], "arguments": arguments},
        });
        let mut replies: Vec<Value> = Vec::new();
        mcp.handle_line(&request.to_string(), &mut replies);
        let reply = replies
            .iter()
            .find(|reply| reply.get("id") == Some(&serde_json::json!(1000)))
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
        let expected_is_error = fixture["is_error"].as_bool().unwrap_or(false);
        if actual_is_error != expected_is_error {
            problems +=
                &format!("  is_error: expected {expected_is_error}, got {actual_is_error}\n");
        }
        let normalized = norm.s(&actual_text);
        let expected_text = fixture["text"].as_str().unwrap_or_default();
        if normalized != expected_text {
            problems += &unified_diff("text", expected_text, &normalized);
        }
        assert!(
            problems.is_empty(),
            "{site}/{name} diverges from the reference fixture:\n{problems}"
        );
    }
    let _ = std::fs::remove_dir_all(&work);
}

/// The sitemap wiring's gate: both sites, both surfaces, 0 divergence.
#[test]
fn sitemap_fixtures_match_the_reference_cli_and_mcp() {
    let mut failures = Vec::new();
    if std::panic::catch_unwind(|| check_cli_site("robots-sitemap")).is_err() {
        failures.push("robots-sitemap (cli)");
    }
    if std::panic::catch_unwind(|| check_mcp_site("robots-sitemap")).is_err() {
        failures.push("robots-sitemap (mcp)");
    }
    if std::panic::catch_unwind(|| check_cli_site("basic")).is_err() {
        failures.push("basic (cli)");
    }
    if std::panic::catch_unwind(|| check_mcp_site("basic")).is_err() {
        failures.push("basic (mcp)");
    }
    assert!(
        failures.is_empty(),
        "sitemap parity diverged on: {}",
        failures.join(", ")
    );
}

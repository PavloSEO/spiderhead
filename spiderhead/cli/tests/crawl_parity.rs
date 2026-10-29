//! Integration test: the Rust `crawl-site` handler reproduces the reference CLI's
//! captured fixture outputs (stdout, stderr, exit code) for every synthetic
//! site, on both surfaces — the crawl wiring's main gate.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::Value;
use spiderhead_cli::{run, Output};
use spiderhead_oracle_testkit::FixtureServer;

/// The oracle's normalisation (the reference suite.s`) as a Rust port:
/// volatile base/work/port/UUID/timestamp shapes become stable tokens.
struct Norm {
    base: String,
    work: String,
    work_canonical: String,
    uuids: BTreeMap<String, String>,
    hex32: BTreeMap<String, String>,
    uuid_re: Regex,
    hex32_re: Regex,
    scanfile_re: Regex,
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
            work_canonical: work
                .canonicalize()
                .unwrap_or_else(|_| work.to_path_buf())
                .to_string_lossy()
                .into_owned(),
            uuids: BTreeMap::new(),
            hex32: BTreeMap::new(),
            uuid_re: Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
                .expect("uuid regex"),
            hex32_re: Regex::new(r"\b[0-9a-f]{32}\b").expect("hex32 regex"),
            scanfile_re: Regex::new(r"scans/\d{8}T\d{6}Z_[a-z0-9.-]+_[0-9a-f]{8}\.sqlite")
                .expect("scanfile regex"),
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
        // Project outputs echo the canonical (/private/var on macOS) work
        // root: fold that spelling before the plain one.
        let text = text
            .replace(&self.base, "{BASE}")
            .replace(&self.work_canonical, "<WORK>")
            .replace(&self.work, "<WORK>");
        let text = self
            .port_re
            .replace_all(&text, "127.0.0.1:{PORT}")
            .into_owned();
        let mut uuids: Vec<(String, String)> = Vec::new();
        for capture in self.uuid_re.find_iter(&text) {
            let found = capture.as_str().to_owned();
            if !self.uuids.contains_key(&found) {
                let token = format!("<UUID:{}>", self.uuids.len() + 1);
                self.uuids.insert(found.clone(), token);
            }
            uuids.push((found.clone(), self.uuids[&found].clone()));
        }
        let mut text = text;
        for (found, token) in uuids {
            text = text.replace(&found, &token);
        }
        let text = self.evid_re.replace_all(&text, "${1}<HASH>").into_owned();
        let text = self.ts_re.replace_all(&text, "<TS>").into_owned();
        let text = self
            .duration_re
            .replace_all(&text, "${1}<DURATION>")
            .into_owned();
        // Recommended project scan artifacts: wall-clock stamp + short uuid.
        let text = self
            .scanfile_re
            .replace_all(&text, "<SCANFILE>")
            .into_owned();
        // Dashless run-observation ids: first appearance per surface.
        let mut shorts: Vec<(String, String)> = Vec::new();
        for capture in self.hex32_re.find_iter(&text) {
            let found = capture.as_str().to_owned();
            if !self.hex32.contains_key(&found) {
                let token = format!("<HEX32:{}>", self.hex32.len() + 1);
                self.hex32.insert(found.clone(), token);
            }
            shorts.push((found.clone(), self.hex32[&found].clone()));
        }
        let mut text = text;
        for (found, token) in shorts {
            text = text.replace(&found, &token);
        }
        text
    }
}

fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/crawl")
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

/// One site through the Rust CLI: same invocation as the capture.
fn check_cli_site(name: &str, _index: usize) {
    let server = FixtureServer::start(name);
    let work = std::env::temp_dir().join(format!("sh-crawl-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("work dir");
    let scan = work.join("scan.sqlite");
    let scan_text = scan.to_string_lossy().into_owned();
    let registry = spiderhead_cli::registry();
    let argv = vec![
        "crawl-site".to_string(),
        "--url".to_string(),
        format!("{}/", server.base),
        "--scan-out".to_string(),
        scan_text.clone(),
        "--max-urls".to_string(),
        "200".to_string(),
        "-q".to_string(),
    ];
    let raw = std::fs::read_to_string(fixtures_root().join(name).join("crawl-site.json"))
        .unwrap_or_else(|e| panic!("fixture: {e}"));
    let fixture: Value = serde_json::from_str(&raw).expect("fixture json");
    let mut out = Output::default();
    let code = run(&argv, &registry, &mut out).expect("handler run");
    let mut norm = Norm::new(&server.base, &work);
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
    let _ = std::fs::remove_dir_all(&work);
    assert!(
        problems.is_empty(),
        "{name} (cli) diverges from the reference fixture:\n{problems}"
    );
}

/// One site through the in-process MCP server: same handshake and call shape.
fn check_mcp_site(name: &str, _index: usize) {
    use spiderhead_mcp::profile::Profile;
    use spiderhead_mcp::server::Server;

    let server = FixtureServer::start(name);
    let work = std::env::temp_dir().join(format!("sh-crawl-mcp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("work dir");
    let scan_out = work.join("scan-mcp.sqlite");
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
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1000,
        "method": "tools/call",
        "params": {
            "name": "seo_crawl_site",
            "arguments": {
                "url": format!("{}/", server.base),
                "scan_out": scan_out.to_string_lossy(),
                "max_urls": 200,
            },
        },
    });
    let mut replies: Vec<Value> = Vec::new();
    mcp.handle_line(&request.to_string(), &mut replies);
    let reply = replies
        .iter()
        .find(|reply| reply.get("id") == Some(&serde_json::json!(1000)))
        .unwrap_or_else(|| panic!("seo_crawl_site: no reply"));
    let result = &reply["result"];
    let actual_is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let actual_text = result["content"][0]["text"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_default();
    let path = fixtures_root()
        .join(name)
        .join("mcp")
        .join("seo_crawl_site.json");
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let fixture: Value = serde_json::from_str(&raw).expect("fixture json");
    let expected_is_error = fixture["is_error"].as_bool().unwrap_or(false);
    let expected_text = fixture["text"].as_str().unwrap_or_default();
    let mut norm = Norm::new(&server.base, &work);
    let mut problems = String::new();
    if actual_is_error != expected_is_error {
        problems += &format!("  is_error: expected {expected_is_error}, got {actual_is_error}\n");
    }
    let normalized = norm.s(&actual_text);
    if normalized != expected_text {
        problems += &unified_diff("text", expected_text, &normalized);
    }
    let _ = std::fs::remove_dir_all(&work);
    assert!(
        problems.is_empty(),
        "{name} (mcp) diverges from the reference fixture:\n{problems}"
    );
}

/// Sorted-keys canonical form of a JSON document; non-JSON text passes through.
/// The project phases compare canonically so a key-order difference inside the
/// capture's reference dict cannot read as a divergence.
fn canonical(text: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return text.to_owned();
    };
    fn sort(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let sorted: BTreeMap<String, Value> = map
                    .iter()
                    .map(|(key, item)| (key.clone(), sort(item)))
                    .collect();
                let mut out = serde_json::Map::new();
                for (key, item) in sorted {
                    out.insert(key, item);
                }
                Value::Object(out)
            }
            Value::Array(items) => Value::Array(items.iter().map(sort).collect()),
            other => other.clone(),
        }
    }
    serde_json::to_string_pretty(&sort(&value)).expect("canonical json")
}

/// The crawl inside a project workspace: the capture's CLI project chain —
/// `project-new` on a public-domain identity, then the live loopback crawl via
/// an explicit `--url` (admission, pacing, run observation, scoped evidence).
fn check_cli_project() {
    let server = FixtureServer::start("basic");
    let work = std::env::temp_dir().join(format!("sh-crawl-proj-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("work dir");
    let raw = std::fs::read_to_string(fixtures_root().join("project.json")).expect("fixture");
    let fixture: Value = serde_json::from_str(&raw).expect("fixture json");
    let registry = spiderhead_cli::registry();
    let mut norm = Norm::new(&server.base, &work);
    let mut problems = String::new();
    for step in fixture["cli"].as_array().expect("cli steps") {
        let name = step["name"].as_str().unwrap_or("?").to_owned();
        let argv: Vec<String> = step["argv"]
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
        if i64::from(code.0) != step["exit"].as_i64().unwrap_or_default() {
            problems += &format!("  {name} exit: expected {}, got {}\n", step["exit"], code.0);
        }
        let actual_stdout = canonical(&norm.s(&out.stdout));
        let expected_stdout = step["stdout"].as_str().unwrap_or_default();
        if actual_stdout != canonical(expected_stdout) {
            problems += &unified_diff(&format!("{name} stdout"), expected_stdout, &actual_stdout);
        }
        let actual_stderr = norm.s(&out.stderr);
        let expected_stderr = step["stderr"].as_str().unwrap_or_default();
        if actual_stderr != expected_stderr {
            problems += &unified_diff(&format!("{name} stderr"), expected_stderr, &actual_stderr);
        }
    }
    let _ = std::fs::remove_dir_all(&work);
    assert!(
        problems.is_empty(),
        "project route (cli) diverges from the reference fixture:\n{problems}"
    );
}

/// The same chain through the in-process MCP server, its own fresh work dir.
fn check_mcp_project() {
    use spiderhead_mcp::profile::Profile;
    use spiderhead_mcp::server::Server;

    let server = FixtureServer::start("basic");
    let work = std::env::temp_dir().join(format!("sh-crawl-proj-mcp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("work dir");
    let raw = std::fs::read_to_string(fixtures_root().join("project.json")).expect("fixture");
    let fixture: Value = serde_json::from_str(&raw).expect("fixture json");
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
    let mut problems = String::new();
    for (index, call) in fixture["mcp"]
        .as_array()
        .expect("mcp steps")
        .iter()
        .enumerate()
    {
        let tool = call["tool"].as_str().unwrap_or("?").to_owned();
        let arguments = call["arguments"]
            .to_string()
            .replace("{WORK}", &work.to_string_lossy())
            .replace("{BASE}", &server.base);
        let arguments: Value = serde_json::from_str(&arguments).expect("arguments json");
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1000 + index,
            "method": "tools/call",
            "params": {"name": tool, "arguments": arguments},
        });
        let mut replies: Vec<Value> = Vec::new();
        mcp.handle_line(&request.to_string(), &mut replies);
        let reply = replies
            .iter()
            .find(|reply| reply.get("id") == Some(&serde_json::json!(1000 + index)))
            .unwrap_or_else(|| panic!("{tool}: no reply"));
        let result = &reply["result"];
        let actual_is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let actual_text = result["content"][0]["text"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_default();
        let expected_is_error = call["is_error"].as_bool().unwrap_or(false);
        if actual_is_error != expected_is_error {
            problems += &format!(
                "  {tool} is_error: expected {expected_is_error}, got {actual_is_error}\n"
            );
        }
        let normalized = canonical(&norm.s(&actual_text));
        let expected_text = call["text"].as_str().unwrap_or_default();
        if normalized != canonical(expected_text) {
            problems += &unified_diff(&format!("{tool} text"), expected_text, &normalized);
        }
    }
    let _ = std::fs::remove_dir_all(&work);
    assert!(
        problems.is_empty(),
        "project route (mcp) diverges from the reference fixture:\n{problems}"
    );
}

/// The crawl wiring's gate: 6 sites × 2 surfaces plus the project route,
/// 0 divergence.
///
/// One test thread on purpose: every phase spawns its own fixture servers on
/// locked ports, and the runs are wall-clock bounded crawls — interleaving
/// them multiplies the tail latency of the whole suite.
#[test]
fn crawl_fixtures_match_the_reference_cli_and_mcp() {
    let sites = [
        "basic",
        "errors",
        "redirects",
        "canonical-hreflang",
        "schema",
        "robots-sitemap",
    ];
    let mut failures = Vec::new();
    for (index, site) in sites.iter().enumerate() {
        if std::panic::catch_unwind(|| check_cli_site(site, index)).is_err() {
            failures.push(format!("{site} (cli)"));
        }
    }
    for (index, site) in sites.iter().enumerate() {
        if std::panic::catch_unwind(|| check_mcp_site(site, index)).is_err() {
            failures.push(format!("{site} (mcp)"));
        }
    }
    if std::panic::catch_unwind(check_cli_project).is_err() {
        failures.push("project route (cli)".to_string());
    }
    if std::panic::catch_unwind(check_mcp_project).is_err() {
        failures.push("project route (mcp)".to_string());
    }
    assert!(
        failures.is_empty(),
        "crawl parity diverged on: {}",
        failures.join(", ")
    );
}

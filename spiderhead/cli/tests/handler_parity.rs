//! Integration test: the Rust handlers reproduce the reference CLI's captured
//! fixture outputs (stdout, stderr, exit code, export bytes) for every
//! synthetic site — the slice's main gate.

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
        // Runtime durations of the command itself (the reference suite's VOLATILE_KEYS)
        // are tokenised so two runs stay byte-identical under load.
        self.duration_re
            .replace_all(&text, "${1}<DURATION>")
            .into_owned()
    }
}

fn oracle_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../oracle")
}

fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/handlers")
}

/// One captured command: the argv shape and how the fixture was recorded.
struct Invocation {
    name: &'static str,
    argv: Vec<String>,
    /// The `--out` path of `scan-export`; its bytes are part of the contract.
    out_file: Option<PathBuf>,
}

impl Invocation {
    fn new(name: &'static str, argv: &[&str]) -> Invocation {
        Invocation {
            name,
            argv: argv.iter().map(|part| (*part).to_owned()).collect(),
            out_file: None,
        }
    }
}

/// The twelve captured invocations per site, in capture order (the shared Norm
/// numbers UUIDs in this order too).
fn invocations(base: &str, work: &Path) -> Vec<Invocation> {
    let scan = work.join("scan.sqlite");
    let scan = scan.to_string_lossy().into_owned();
    let about = format!("{base}/about.html");
    let export = work.join("export.json");
    let mut list = vec![
        Invocation::new("scan-status", &["scan-status", "--scan", &scan]),
        Invocation::new(
            "scan-url-query",
            &["scan-url-query", "--scan", &scan, "--limit", "500"],
        ),
        Invocation::new(
            "scan-url-counts",
            &["scan-url-counts", "--scan", &scan, "--filter-sets", "[[]]"],
        ),
        Invocation::new("scan-graph", &["scan-graph", "--scan", &scan]),
        Invocation::new(
            "scan-directory-tree",
            &["scan-directory-tree", "--scan", &scan],
        ),
        Invocation::new("duplicate-check", &["duplicate-check", "--scan", &scan]),
        Invocation::new(
            "scan-findings-page",
            &["scan-findings-page", "--scan", &scan, "--limit", "100"],
        ),
        Invocation::new(
            "scan-inspect",
            &["scan-inspect", "--scan", &scan, "--limit", "20"],
        ),
        Invocation::new(
            "scan-body-read",
            &["scan-body-read", "--scan", &scan, "--url", &about],
        ),
    ];
    let mut export_invocation = Invocation::new(
        "scan-export",
        &["scan-export", "--scan", &scan, "--format", "json"],
    );
    let out = export.to_string_lossy().into_owned();
    export_invocation.argv.push("--out".to_owned());
    export_invocation.argv.push(out);
    export_invocation.out_file = Some(export);
    list.push(export_invocation);
    let home = format!("{base}/");
    list.push(Invocation::new("parse", &["parse", "--url", &home]));
    let absent = work.join("absent.sqlite");
    list.push(Invocation::new(
        "scan-status-missing",
        &["scan-status", "--scan", &absent.to_string_lossy()],
    ));
    list
}

/// What a single fixture records.
struct Expectation {
    exit: i64,
    stdout: String,
    stderr: String,
    out_file: Option<String>,
}

fn expectation(site: &str, name: &str) -> Expectation {
    let path = fixtures_root().join(site).join(format!("{name}.json"));
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let value: Value = serde_json::from_str(&raw).expect("fixture json");
    Expectation {
        exit: value["exit"].as_i64().unwrap_or_default(),
        stdout: value["stdout"].as_str().unwrap_or_default().to_owned(),
        stderr: value["stderr"].as_str().unwrap_or_default().to_owned(),
        out_file: value["out_file"].as_str().map(str::to_owned),
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
            if diff.len() > 4000 {
                diff += "  …\n";
                break;
            }
        }
    }
    diff
}

/// One site: run every invocation through the Rust surface and diff the
/// normalised outputs against the reference fixtures.
fn check_site(name: &str, _index: usize) {
    let server = FixtureServer::start(name);
    let work =
        std::env::temp_dir().join(format!("sh-handler-parity-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("work dir");
    std::fs::copy(
        oracle_root()
            .join("raw-scans")
            .join(name)
            .join("scan.sqlite"),
        work.join("scan.sqlite"),
    )
    .expect("raw scan copy");
    let registry = spiderhead_cli::registry();
    let mut norm = Norm::new(&server.base, &work);
    for invocation in invocations(&server.base, &work) {
        // Each captured command ran in its own reference process: drop the in-process
        // full-validation cache so light/full reporting matches that boundary.
        spiderhead_store::scan_artifact::forget_validated();
        let expected = expectation(name, invocation.name);
        let mut out = Output::default();
        let code = run(&invocation.argv, &registry, &mut out).expect("handler run");
        let mut problems = String::new();
        if i64::from(code.0) != expected.exit {
            problems += &format!("  exit: expected {}, got {}\n", expected.exit, code.0);
        }
        let actual_stdout = norm.s(&out.stdout);
        if actual_stdout != expected.stdout {
            problems += &unified_diff("stdout", &expected.stdout, &actual_stdout);
        }
        let actual_stderr = norm.s(&out.stderr);
        if actual_stderr != expected.stderr {
            problems += &unified_diff("stderr", &expected.stderr, &actual_stderr);
        }
        if let Some(out_path) = &invocation.out_file {
            let actual_bytes = std::fs::read_to_string(out_path).unwrap_or_default();
            let actual_file = norm.s(&actual_bytes);
            let expected_file = expected.out_file.as_deref().unwrap_or_default();
            if actual_file != expected_file {
                problems += &unified_diff("out_file", expected_file, &actual_file);
            }
        }
        if !problems.is_empty() {
            panic!(
                "{}: {} diverges from the reference fixture:\n{}",
                name, invocation.name, problems
            );
        }
    }
    let _ = std::fs::remove_dir_all(&work);
}

/// The slice's main gate: 6 sites × 12 CLI commands, then the same twelve
/// inputs through `tools/call` — 0 divergence on both surfaces.
///
/// One test thread on purpose: the CLI phase resets the in-process
/// full-validation cache per command (fresh reference processes), while the MCP
/// phase keeps it (one long-lived server on both sides) — the cache is global,
/// so the two phases must not interleave.
#[test]
fn fixtures_match_the_reference_cli_and_mcp() {
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
        if std::panic::catch_unwind(|| check_site(site, index)).is_err() {
            failures.push(format!("{site} (cli)"));
        }
    }
    for (index, site) in sites.iter().enumerate() {
        if std::panic::catch_unwind(|| check_mcp_site(site, index)).is_err() {
            failures.push(format!("{site} (mcp)"));
        }
    }
    assert!(
        failures.is_empty(),
        "handler parity diverged on: {}",
        failures.join(", ")
    );
}

/// One `tools/call` argument set, in capture order.
fn mcp_calls(base: &str, work: &Path, scan: &Path, scan_uuid: &str) -> Vec<(&'static str, Value)> {
    let scan_text = scan.to_string_lossy().into_owned();
    let absent = work.join("absent.sqlite");
    let absent = absent.to_string_lossy().into_owned();
    let object: Vec<(&str, Value)> = vec![
        ("input_path", json_string(&scan_text)),
        ("limit", Value::from(500)),
    ];
    let _ = object;
    vec![
        (
            "seo_scan_status",
            serde_json::json!({"input_path": scan_text}),
        ),
        (
            "seo_scan_url_query",
            serde_json::json!({"input_path": scan_text, "limit": 500}),
        ),
        (
            "seo_scan_url_counts",
            serde_json::json!({"input_path": scan_text, "filter_sets": [[]]}),
        ),
        (
            "seo_scan_graph",
            serde_json::json!({"input_path": scan_text}),
        ),
        (
            "seo_scan_directory_tree",
            serde_json::json!({"input_path": scan_text}),
        ),
        (
            "seo_duplicate_check",
            serde_json::json!({"scan": scan_text}),
        ),
        (
            "seo_scan_findings_page",
            serde_json::json!({
                "project": work.to_string_lossy(),
                "scan_uuid": scan_uuid,
                "limit": 100,
            }),
        ),
        (
            "seo_scan_inspect",
            serde_json::json!({"input_path": scan_text, "limit": 20}),
        ),
        (
            "seo_scan_body_read",
            serde_json::json!({"input_path": scan_text, "url": format!("{base}/about.html")}),
        ),
        (
            "seo_scan_export",
            serde_json::json!({
                "input_path": scan_text,
                "out": work.join("export-mcp.json").to_string_lossy(),
                "format": "json",
            }),
        ),
        ("seo_parse", serde_json::json!({"url": format!("{base}/")})),
        (
            "seo_scan_status_absent",
            serde_json::json!({"input_path": absent}),
        ),
    ]
}

fn json_string(value: &str) -> Value {
    Value::String(value.to_owned())
}

/// One site over the in-process server: same handshake, same call order, same
/// normalisation as the capture.
fn check_mcp_site(name: &str, _index: usize) {
    use spiderhead_mcp::profile::Profile;
    use spiderhead_mcp::server::Server;

    let server = FixtureServer::start(name);
    let work = std::env::temp_dir().join(format!("sh-mcp-parity-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("work dir");
    let scan = work.join("scan.sqlite");
    std::fs::copy(
        oracle_root()
            .join("raw-scans")
            .join(name)
            .join("scan.sqlite"),
        &scan,
    )
    .expect("raw scan copy");
    let scan_uuid = scan_uuid(&scan);
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
    for (number, (tool, arguments)) in mcp_calls(&server.base, &work, &scan, &scan_uuid)
        .into_iter()
        .enumerate()
    {
        // One MCP server process serves every call: the in-process full-validation
        // cache behaves exactly like the oracle session's, so it is not reset here.
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1000 + number,
            "method": "tools/call",
            "params": {"name": tool, "arguments": arguments},
        });
        let mut replies: Vec<Value> = Vec::new();
        mcp.handle_line(&request.to_string(), &mut replies);
        let reply = replies
            .iter()
            .find(|reply| reply.get("id") == Some(&serde_json::json!(1000 + number)))
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
        let path = fixtures_root()
            .join(name)
            .join("mcp")
            .join(format!("{tool}.json"));
        let raw =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let fixture: Value = serde_json::from_str(&raw).expect("fixture json");
        let expected_is_error = fixture["is_error"].as_bool().unwrap_or(false);
        let expected_text = fixture["text"].as_str().unwrap_or_default();
        let mut problems = String::new();
        if actual_is_error != expected_is_error {
            problems += &format!(
                "  is_error: expected {expected_is_error}, got {actual_is_error}
"
            );
        }
        let normalized = norm.s(&actual_text);
        if normalized != expected_text {
            problems += &unified_diff("text", expected_text, &normalized);
        }
        if !problems.is_empty() {
            panic!("{name}: {tool} diverges from the reference MCP fixture:\n{problems}");
        }
    }
    let _ = std::fs::remove_dir_all(&work);
}

/// The `scan_uuid` header cell of one scan file.
fn scan_uuid(path: &Path) -> String {
    let connection =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("scan opens");
    connection
        .query_row("SELECT scan_uuid FROM scan WHERE singleton=1", [], |row| {
            row.get::<_, String>(0)
        })
        .expect("scan_uuid")
}

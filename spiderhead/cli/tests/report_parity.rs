//! Integration test: the Rust report-build / facts-export /
//! crawl-describe-settings handlers reproduce the reference fixtures captured by
//! the reference suite, on both surfaces. Everything is offline: the audit
//! inputs materialize from the retained oracle raw scans exactly like the
//! capture's `storage.read_audit`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use regex::Regex;
use serde_json::{json, Value};

/// The oracle's normalisation, without a fixture server: the work path,
/// UUIDs, evidence hashes and timestamps become stable tokens.
struct Norm {
    work: String,
    uuids: BTreeMap<String, String>,
    uuid_re: Regex,
    evid_re: Regex,
    ts_re: Regex,
    port_re: Regex,
    duration_re: Regex,
}

impl Norm {
    fn new(work: &Path) -> Norm {
        Norm {
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
        let text = text.replace(&self.work, "<WORK>");
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
        let text = self
            .port_re
            .replace_all(&text, "127.0.0.1:{PORT}")
            .into_owned();
        self.duration_re
            .replace_all(&text, "${1}<DURATION>")
            .into_owned()
    }
}

fn fixture() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/report/report.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture file"))
        .expect("fixture json")
}

fn oracle_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../oracle")
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

/// The audit documents both surfaces build from: `storage.read_audit` on the
/// oracle side, `AuditV2Reader::materialize_legacy` here (content-equal).
fn materialize(scan: &Path) -> Value {
    let reader =
        spiderhead_store::audit_v2::AuditV2Reader::new(scan, true, false, None).expect("reader");
    reader
        .materialize_legacy(64 * 1024 * 1024)
        .expect("audit document")
}

fn prepare_work(work: &Path) {
    std::fs::create_dir_all(work).expect("work dir");
    for site in ["basic", "errors"] {
        let raw = oracle_root()
            .join("raw-scans")
            .join(site)
            .join("scan.sqlite");
        let scan = work.join(format!("scan-{site}.sqlite"));
        std::fs::copy(&raw, &scan).expect("scan copy");
        std::fs::copy(
            raw.with_file_name("scan.sqlite.audit-v2.sqlite"),
            work.join(format!("scan-{site}.sqlite.audit-v2.sqlite")),
        )
        .expect("companion copy");
        let doc = materialize(&scan);
        std::fs::write(
            work.join(format!("audit-{site}.json")),
            serde_json::to_string(&doc).expect("audit json"),
        )
        .expect("audit write");
    }
    // A discoverable-domain copy of the basic crawl audit (facts refuses an
    // IP-hosted source).
    let text = std::fs::read_to_string(work.join("audit-basic.json")).expect("audit text");
    std::fs::write(
        work.join("audit-example.json"),
        text.replace("http://127.0.0.1:18701", "https://example.org"),
    )
    .expect("audit example");
    let site_audit = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../spiderhead/reports/tests/fixtures/site_audit_full.json");
    std::fs::copy(&site_audit, work.join("site-audit.json")).expect("site audit copy");
    let positive = json!([
        {
            "label": "example.org",
            "crawl_audit": work.join("audit-example.json").to_string_lossy(),
            "site_audit": work.join("site-audit.json").to_string_lossy(),
        }
    ]);
    std::fs::write(
        work.join("sites-positive.json"),
        serde_json::to_string(&positive).expect("sites json"),
    )
    .expect("sites write");
}

#[test]
fn report_group_matches_the_reference_cli_and_mcp() {
    let fixture = fixture();
    let work = std::env::temp_dir().join(format!("sh-report-parity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    prepare_work(&work);
    let mut norm = Norm::new(&work);

    // ── CLI: every fixture replays through the real binary ─────────────────
    for (name, case) in fixture["cases"].as_object().expect("cases") {
        let argv: Vec<String> = case["argv"]
            .as_array()
            .expect("argv")
            .iter()
            .map(|part| {
                part.as_str()
                    .unwrap_or_default()
                    .replace("{WORK}", &work.to_string_lossy())
            })
            .collect();
        spiderhead_store::scan_artifact::forget_validated();
        let output = Command::new(env!("CARGO_BIN_EXE_spiderhead"))
            .args(&argv)
            .current_dir(&work)
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .expect("binary runs");
        let mut problems = String::new();
        let exit = output.status.code().unwrap_or_default();
        let expected_exit = case["exit"].as_i64().unwrap_or_default();
        if i64::from(exit) != expected_exit {
            problems += &format!("  exit: expected {expected_exit}, got {exit}\n");
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let actual_stdout = norm.s(&stdout);
        let expected_stdout = case["stdout"].as_str().unwrap_or_default();
        if actual_stdout != expected_stdout {
            problems += &unified_diff("stdout", expected_stdout, &actual_stdout);
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let actual_stderr = norm.s(&stderr);
        let expected_stderr = case["stderr"].as_str().unwrap_or_default();
        if actual_stderr != expected_stderr {
            problems += &unified_diff("stderr", expected_stderr, &actual_stderr);
        }
        if let Some(expected_file) = case.get("out_file").and_then(Value::as_str) {
            let out_path = argv
                .iter()
                .position(|part| part == "--out")
                .and_then(|index| argv.get(index + 1))
                .expect("--out path");
            let actual_file = std::fs::read_to_string(out_path)
                .map(|text| norm.s(&text))
                .unwrap_or_default();
            if actual_file != expected_file {
                problems += &unified_diff("out_file", expected_file, &actual_file);
            }
        }
        assert!(
            problems.is_empty(),
            "{name} diverges from the reference fixture:\n{problems}"
        );
    }

    // ── MCP: the same inputs through the in-process server ─────────────────
    use spiderhead_mcp::profile::Profile;
    use spiderhead_mcp::server::Server;

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
    for (index, (name, call)) in fixture["mcp"].as_object().expect("mcp").iter().enumerate() {
        let arguments = resolve(&call["arguments"], &work);
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1000 + index,
            "method": "tools/call",
            "params": {"name": call["tool"], "arguments": arguments},
        });
        let mut replies: Vec<Value> = Vec::new();
        mcp.handle_line(&request.to_string(), &mut replies);
        let reply = replies
            .iter()
            .find(|reply| reply.get("id") == Some(&serde_json::json!(1000 + index)))
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
    let _ = std::fs::remove_dir_all(&work);
}

/// `{WORK}` placeholders in recorded arguments resolve to the live work dir.
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

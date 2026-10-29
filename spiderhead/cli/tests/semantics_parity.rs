//! Integration test: the Rust semantics CLI and MCP tools reproduce the
//! oracle's captured stage sequence (the reference suite) step by step.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use regex::Regex;
use serde_json::Value;

/// The oracle's normalisation, without a fixture server: work path, UUIDs,
/// evidence hashes and timestamps become stable tokens.
struct Norm {
    work: String,
    uuids: BTreeMap<String, String>,
    uuid_re: Regex,
    evid_re: Regex,
    ts_re: Regex,
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
        self.ts_re.replace_all(&text, "<TS>").into_owned()
    }
}

fn fixture() -> Value {
    // The fixture belongs to the semantics crate; the CLI binary is what the
    // sequence runs through.
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../semantics/tests/fixtures/cli_oracle.json");
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

/// The stage sequence through the real `spiderhead` binary: the free stages
/// print their progress lines directly, so the process boundary is the oracle
/// capture's boundary too.
#[test]
fn semantics_cli_matches_the_oracle_sequence() {
    let fixture = fixture();
    // Resolved exactly like the capture (`Path.resolve`), so macOS's /private
    // symlink prefix matches on both sides before Norm erases the work path.
    let work = std::env::temp_dir().join(format!("sh-sem-parity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("work dir");
    let work = work.canonicalize().expect("work resolves");
    let csv = work.join("phrases.csv");
    std::fs::write(&csv, fixture["phrases_csv"].as_str().unwrap_or_default()).expect("csv");
    let mut norm = Norm::new(&work);
    for (index, step) in fixture["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .enumerate()
    {
        if index == fixture["inject_after_step"].as_i64().unwrap_or_default() as usize {
            inject(&work.join("p").join("sya.db"), &fixture["inject_sql"]);
        }
        let argv: Vec<String> = step["argv"]
            .as_array()
            .expect("argv")
            .iter()
            .map(|part| {
                part.as_str()
                    .unwrap_or_default()
                    .replace("{PROJECT}", "p")
                    .replace("{CSV}", &csv.to_string_lossy())
            })
            .collect();
        let output = Command::new(env!("CARGO_BIN_EXE_spiderhead"))
            .args(&argv)
            .current_dir(&work)
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .expect("binary runs");
        let mut problems = String::new();
        let exit = output.status.code().unwrap_or_default();
        let expected_exit = step["exit"].as_i64().unwrap_or_default();
        if i64::from(exit) != expected_exit {
            problems += &format!("  exit: expected {expected_exit}, got {exit}\n");
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let expected_stdout = step["stdout"].as_str().unwrap_or_default();
        let actual_stdout = norm.s(&stdout);
        if actual_stdout != expected_stdout {
            problems += &unified_diff("stdout", expected_stdout, &actual_stdout);
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let expected_stderr = step["stderr"].as_str().unwrap_or_default();
        let actual_stderr = norm.s(&stderr);
        if actual_stderr != expected_stderr {
            problems += &unified_diff("stderr", expected_stderr, &actual_stderr);
        }
        assert!(
            problems.is_empty(),
            "step {index} ({:?}) diverges:\n{problems}",
            argv.join(" ")
        );
    }
    // The capture's MCP calls answered the same post-sequence project state.
    check_mcp_calls(&fixture, &work, &mut norm);
    let _ = std::fs::remove_dir_all(&work);
}

/// The same project state answered through the in-process MCP server.
fn check_mcp_calls(fixture: &Value, work: &Path, norm: &mut Norm) {
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
    let project = work.join("p");
    for (index, call) in fixture["mcp"].as_array().expect("mcp").iter().enumerate() {
        let arguments = call["arguments"]
            .as_object()
            .expect("arguments")
            .iter()
            .map(|(key, value)| {
                let rendered = value
                    .to_string()
                    .replace("{PROJECT}", &project.to_string_lossy());
                (
                    key.clone(),
                    serde_json::from_str(&rendered).unwrap_or(value.clone()),
                )
            })
            .collect::<serde_json::Map<String, Value>>();
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
            .unwrap_or_else(|| panic!("call {index}: no reply"));
        let result = &reply["result"];
        let is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let text = result["content"][0]["text"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_default();
        let mut problems = String::new();
        let expected_is_error = call["is_error"].as_bool().unwrap_or(false);
        if is_error != expected_is_error {
            problems += &format!("  is_error: expected {expected_is_error}, got {is_error}\n");
        }
        let actual = norm.s(&text);
        let expected = call["text"].as_str().unwrap_or_default();
        if actual != expected {
            problems += &unified_diff("text", expected, &actual);
        }
        assert!(
            problems.is_empty(),
            "mcp call {index} ({}) diverges:\n{problems}",
            call["tool"].as_str().unwrap_or_default()
        );
    }
}

/// The fixture's SQL injection, replayed between the same two steps.
fn inject(db: &Path, statements: &Value) {
    let connection = rusqlite::Connection::open(db).expect("project db opens");
    for statement in statements.as_array().expect("sql") {
        connection
            .execute(statement.as_str().unwrap_or_default(), [])
            .expect("inject runs");
    }
}

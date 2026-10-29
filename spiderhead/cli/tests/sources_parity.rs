//! Integration test: the Rust offline sources/provider catalog commands
//! (provider-registry, sources-doctor, provider-readiness, sources-status,
//! sources-export) reproduce the reference fixtures captured by
//! the reference suite — isolated `HOME`, credential variables unset, on
//! both surfaces.

use std::path::Path;
use std::process::Command;

use regex::Regex;
use serde_json::{json, Value};

use spiderhead_cli::{run, Output};

/// Every credential variable the registry names — unset on both surfaces.
const CREDENTIAL_VARS: [&str; 19] = [
    "ARSENKIN_TOKEN",
    "YANDEX_CLOUD_API_KEY",
    "YANDEX_CLOUD_FOLDER_ID",
    "YANDEX_METRIKA_TOKEN",
    "YANDEX_WEBMASTER_TOKEN",
    "DATAFORSEO_LOGIN",
    "DATAFORSEO_PASSWORD",
    "GSC_ACCESS_TOKEN",
    "GSC_SERVICE_ACCOUNT_FILE",
    "CRUX_API_KEY",
    "PAGESPEED_API_KEY",
    "GA4_ACCESS_TOKEN",
    "MIRATEXT_API_KEY",
    "BING_WEBMASTER_API_KEY",
    "MOZ_API_TOKEN",
    "CLOUDFLARE_API_TOKEN",
    "CLOUDFLARE_TOKEN_FILE",
    "SEOHEAD_CLOUDFLARE_TOKEN_FILE",
    "INDEXNOW_KEY",
];

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

/// Strip the shell banner lines and isolate the capture environment: a fresh
/// `HOME` (empty `~/.config` credential roots) and no credential variables.
fn prepare(work: &Path) {
    // `env::set_var` is process-global; this test is the only one touching it.
    std::env::set_var("HOME", work);
    for name in CREDENTIAL_VARS {
        std::env::remove_var(name);
    }
    // The MCP server profiles read SPIDERHEAD_CONFIG_DIR; unset it so the
    // isolated HOME is the only configuration source.
    std::env::remove_var("SPIDERHEAD_CONFIG_DIR");
}

fn fresh_work(tag: &str) -> std::path::PathBuf {
    let work = std::env::temp_dir().join(format!("sh-sources-parity-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("work dir");
    let db = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sources/db.sqlite");
    std::fs::copy(db, work.join("sources.sqlite")).expect("sources db fixture");
    work
}

fn normalize(text: &str, work: &Path) -> String {
    // Timestamps the readiness documents may carry.
    let ts = Regex::new(r"\d{4}-\d\d-\d\d[T ]\d\d:\d\d:\d\d(\.\d+)?(Z|[+-]\d\d:?\d\d)?")
        .expect("ts regex");
    let text = ts.replace_all(text, "<TS>").into_owned();
    text.replace(work.to_string_lossy().as_ref(), "<WORK>")
}

#[test]
fn sources_group_matches_the_reference_cli_and_mcp() {
    let raw = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sources/sources.json"),
    )
    .expect("fixture file");
    let fixture: Value = serde_json::from_str(&raw).expect("fixture json");

    // ── CLI: the chain replays through the real binary in its own work dir ──
    let cli_work = fresh_work("cli");
    prepare(&cli_work);
    let registry = spiderhead_cli::registry();
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
                    .replace("{DB}", &cli_work.join("sources.sqlite").to_string_lossy())
            })
            .collect();
        let mut out = Output::default();
        let code = run(&argv, &registry, &mut out).expect("handler run");
        let mut problems = String::new();
        if i64::from(code.0) != case["exit"].as_i64().unwrap_or_default() {
            problems += &format!("  exit: expected {}, got {}\n", case["exit"], code.0);
        }
        let actual_stdout = normalize(&out.stdout, &cli_work);
        let expected_stdout = case["stdout"].as_str().unwrap_or_default();
        if actual_stdout != expected_stdout {
            problems += &unified_diff("stdout", expected_stdout, &actual_stdout);
        }
        let actual_stderr = normalize(&out.stderr, &cli_work);
        let expected_stderr = case["stderr"].as_str().unwrap_or_default();
        if actual_stderr != expected_stderr {
            problems += &unified_diff("stderr", expected_stderr, &actual_stderr);
        }
        assert!(problems.is_empty(), "{name} diverges:\n{problems}");
    }
    let _ = std::fs::remove_dir_all(&cli_work);

    // ── MCP: the same chain through the in-process server, fresh work dir ──
    use spiderhead_mcp::profile::Profile;
    use spiderhead_mcp::server::Server;

    let mcp_work = fresh_work("mcp");
    prepare(&mcp_work);
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
            .replace("{DB}", &mcp_work.join("sources.sqlite").to_string_lossy());
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1000 + index,
            "method": "tools/call",
            "params": {"name": call["tool"], "arguments": serde_json::from_str::<Value>(&arguments).expect("arguments json")},
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
        let normalized = normalize(&actual_text, &mcp_work);
        let expected_text = call["text"].as_str().unwrap_or_default();
        if normalized != expected_text {
            problems += &unified_diff("text", expected_text, &normalized);
        }
        assert!(problems.is_empty(), "{name} diverges:\n{problems}");
    }
    let _ = std::fs::remove_dir_all(&mcp_work);
    // Keep the unused-import warning away when the binary isn't exercised.
    let _ = Command::new("true");
}

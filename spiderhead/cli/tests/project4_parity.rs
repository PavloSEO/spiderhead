//! Integration test: the Rust project group-4 commands (settings, journal,
//! checklist, policy, views, archives, schedule settings) reproduce the reference fixtures captured by
//! the reference suite — one chain per group, on both surfaces, in fresh
//! work directories per surface. Dynamic values (entry ids, occurrence keys,
//! revisions) flow through `{{STEP:/json/pointer}}` markers that each side
//! resolves from its own prior outputs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::{json, Value};

use spiderhead_cli::{run, Output};

/// Chain order mirrors the generator's `CHAINS` mapping: first appearance in
/// this order assigns the shared `<UUID:n>` / `<HEX64:n>` numbers.
const CHAINS: [&str; 11] = [
    "settings",
    "journal",
    "checklist",
    "policy",
    "views",
    "archive",
    "schedule",
    "logs",
    "runs",
    "diag",
    "enrich",
];

/// The oracle's normalisation plus the two group-4 rules: ledger-random 64-hex
/// tokens number by first appearance, and JSON stdout is canonicalised (sorted
/// keys) because the oracle's summary `counts` order is a frozenset iteration
/// order that differs per process.
struct Norm {
    works: std::cell::RefCell<Vec<(String, String)>>,
    uuids: BTreeMap<String, String>,
    hex64: BTreeMap<String, String>,
    uuid_re: Regex,
    hex_re: Regex,
    hex32: BTreeMap<String, String>,
    hex32_re: Regex,
    view_re: Regex,
    evid_re: Regex,
    ts_re: Regex,
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
            hex64: BTreeMap::new(),
            uuid_re: Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
                .expect("uuid regex"),
            hex_re: Regex::new(r"\b[0-9a-f]{64}\b").expect("hex64 regex"),
            hex32: BTreeMap::new(),
            hex32_re: Regex::new(r"\b[0-9a-f]{32}\b").expect("hex32 regex"),
            view_re: Regex::new(r"view-[0-9a-f]{24}").expect("view regex"),
            evid_re: Regex::new(r"(evidence:v1:(<UUID:\d+)>:\w+:)[0-9a-f]{24}")
                .expect("evid regex"),
            ts_re: Regex::new(r"\d{4}-\d\d-\d\d[T ]\d\d:\d\d:\d\d(\.\d+)?(Z|[+-]\d\d:?\d\d)?")
                .expect("ts regex"),
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
        let text = self
            .duration_re
            .replace_all(&text, "${1}<DURATION>")
            .into_owned();
        let mut hexes: Vec<(String, String)> = Vec::new();
        for capture in self.hex_re.find_iter(&text) {
            let found = capture.as_str().to_owned();
            let len = self.hex64.len();
            let token = format!("<HEX64:{}>", len + 1);
            self.hex64.entry(found.clone()).or_insert(token);
            hexes.push((found.clone(), self.hex64[&found].clone()));
        }
        let mut text = text;
        for (found, token) in hexes {
            text = text.replace(&found, &token);
        }
        let text = self.view_re.replace_all(&text, "view-<HASH>").into_owned();
        let mut shorts: Vec<(String, String)> = Vec::new();
        for capture in self.hex32_re.find_iter(&text) {
            let found = capture.as_str().to_owned();
            let len = self.hex32.len();
            let token = format!("<HEX32:{}>", len + 1);
            self.hex32.entry(found.clone()).or_insert(token);
            shorts.push((found.clone(), self.hex32[&found].clone()));
        }
        let mut text = text;
        for (found, token) in shorts {
            text = text.replace(&found, &token);
        }
        text
    }
}

/// Sorted-keys canonical form of a JSON document; non-JSON text passes through.
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

fn fixture() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/project/group4.json");
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

/// `{{STEP:/json/pointer}}` markers resolve against this session's own outputs.
fn resolve_markers(text: &str, history: &[Value]) -> String {
    let re = Regex::new(r"\{\{(\d+):(/[^\}]*)\}\}").expect("marker regex");
    re.replace_all(text, |caps: &regex::Captures| {
        let step: usize = caps[1].parse().unwrap_or(usize::MAX);
        let Some(record) = history.get(step) else {
            return caps[0].to_owned();
        };
        let Ok(document) = serde_json::from_str::<Value>(record["stdout"].as_str().unwrap_or(""))
        else {
            return caps[0].to_owned();
        };
        let mut value = &document;
        for part in caps[2].trim_matches('/').split('/') {
            let next = match value {
                Value::Array(items) => part.parse::<usize>().ok().and_then(|i| items.get(i)),
                Value::Object(map) => map.get(part),
                _ => None,
            };
            match next {
                Some(found) => value = found,
                None => return caps[0].to_owned(),
            }
        }
        match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        }
    })
    .into_owned()
}

fn fresh_work(tag: &str) -> PathBuf {
    let work =
        std::env::temp_dir().join(format!("sh-project4-parity-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("work dir");
    work
}

/// The static work-root inputs of every chain: the retained audit the
/// remediation chain ingests, and the deliberately wrong verification artifact.
fn prepare_work(work: &Path) {
    let baseline = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../projects/tests/fixtures/audited-cap500.sqlite");
    std::fs::copy(baseline, work.join("baseline-cap500.sqlite")).expect("baseline fixture");
    let verification = json!({
        "schema_version": "verification.v1",
        "baseline": {"audit_sha256": "0".repeat(64), "results_policy_fingerprint": "0".repeat(16)},
        "selection": {"occurrence_keys": []},
        "collection": {"state": "not_run"},
        "findings": [{"finding_id": "1", "check": "NO_RESPONSE", "url": "https://example.test/"}],
    });
    let access = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/logs/access.log");
    std::fs::copy(&access, work.join("access.log")).expect("access log fixture");
    std::fs::copy(
        access.with_file_name("access-clean.log"),
        work.join("access-clean.log"),
    )
    .expect("clean access log fixture");
    let complete = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../projects/tests/fixtures/crawl-evidence.sqlite");
    std::fs::copy(complete, work.join("complete-scan.sqlite")).expect("complete scan fixture");
    for name in ["diag", "enrich"] {
        copy_tree(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(name),
            &work.join(name),
        );
    }
    std::fs::create_dir_all(work.join("enrich/out")).expect("enrich out dir");
    std::fs::write(
        work.join("verification-bad.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&verification).expect("verification json")
        ),
    )
    .expect("verification artifact");
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("fixture dir");
    for entry in std::fs::read_dir(from).expect("fixture tree") {
        let entry = entry.expect("fixture entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("fixture file");
        }
    }
}

#[test]
fn project_group4_matches_the_reference_cli_and_mcp() {
    // The fixture was captured outside the 01:00-06:00 schedule window; pin the clock for
    // both the CLI and the MCP phase so the result does not depend on the time of day.
    std::env::set_var("SPIDERHEAD_NOW", "2026-10-29T12:00:00Z");
    let fixture = fixture();
    let mut failures: Vec<String> = Vec::new();

    // ── CLI: the chains replay through the real binary in its own work dir ──
    let cli_work = fresh_work("cli");
    prepare_work(&cli_work);
    let registry = spiderhead_cli::registry();
    let mut norm = Norm::new(&cli_work);
    for chain in CHAINS {
        let cases: Vec<(String, &Value)> = fixture["cases"]
            .as_object()
            .expect("cases")
            .iter()
            .filter(|(_, case)| case["chain"].as_str() == Some(chain))
            .map(|(name, case)| (name.clone(), case))
            .collect();
        let mut history: Vec<Value> = Vec::new();
        let mut ordered = cases;
        ordered.sort_by_key(|(_, case)| case["step"].as_i64().unwrap_or_default());
        for (name, case) in &ordered {
            let argv: Vec<String> = case["argv"]
                .as_array()
                .expect("argv")
                .iter()
                .map(|part| {
                    resolve_markers(part.as_str().unwrap_or_default(), &history)
                        .replace("{WORK}", &cli_work.to_string_lossy())
                })
                .collect();
            let mut out = Output::default();
            let code = run(&argv, &registry, &mut out).expect("handler run");
            history.push(json!({"exit": code.0, "stdout": out.stdout, "stderr": out.stderr}));
            let mut problems = String::new();
            if i64::from(code.0) != case["exit"].as_i64().unwrap_or_default() {
                problems += &format!("  exit: expected {}, got {}\n", case["exit"], code.0);
            }
            let actual_stdout = canonical(&norm.s(&out.stdout));
            let expected_stdout = case["stdout"].as_str().unwrap_or_default();
            if actual_stdout != canonical(expected_stdout) {
                problems += &unified_diff("stdout", expected_stdout, &actual_stdout);
            }
            let actual_stderr = norm.s(&out.stderr);
            let expected_stderr = case["stderr"].as_str().unwrap_or_default();
            if actual_stderr != expected_stderr {
                problems += &unified_diff("stderr", expected_stderr, &actual_stderr);
            }
            if !problems.is_empty() {
                failures.push(format!("{name} diverges:\n{problems}"));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&cli_work);

    // ── MCP: the same chains through the in-process server, fresh work dir ──
    use spiderhead_mcp::profile::Profile;
    use spiderhead_mcp::server::Server;

    let mcp_work = fresh_work("mcp");
    prepare_work(&mcp_work);
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
    for chain in CHAINS {
        let calls: Vec<(String, &Value)> = fixture["mcp"]
            .as_object()
            .expect("mcp")
            .iter()
            .filter(|(_, call)| call["chain"].as_str() == Some(chain))
            .map(|(name, call)| (name.clone(), call))
            .collect();
        let mut history: Vec<Value> = Vec::new();
        let mut ordered = calls;
        ordered.sort_by_key(|(_, call)| call["step"].as_i64().unwrap_or_default());
        for (index, (name, call)) in ordered.iter().enumerate() {
            let arguments = resolve(
                &resolve_markers(call["arguments"].to_string().as_str(), &history),
                &mcp_work,
            );
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
            history.push(json!({"stdout": actual_text}));
            let mut problems = String::new();
            let expected_is_error = call["is_error"].as_bool().unwrap_or(false);
            if actual_is_error != expected_is_error {
                problems +=
                    &format!("  is_error: expected {expected_is_error}, got {actual_is_error}\n");
            }
            let normalized = canonical(&norm.s(&actual_text));
            let expected_text = call["text"].as_str().unwrap_or_default();
            if normalized != canonical(expected_text) {
                problems += &unified_diff("text", expected_text, &normalized);
            }
            if !problems.is_empty() {
                failures.push(format!("{name} diverges:\n{problems}"));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&mcp_work);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// `{WORK}` placeholders in recorded arguments resolve to the surface's work dir.
fn resolve(arguments: &str, work: &Path) -> Value {
    let resolved = arguments.replace("{WORK}", &work.to_string_lossy());
    serde_json::from_str(&resolved).expect("arguments json")
}

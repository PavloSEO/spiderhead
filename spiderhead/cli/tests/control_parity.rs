//! Integration test: the Rust catalog, workflow-run and BI delivery commands
//! reproduce the reference fixtures captured by the reference suite — one
//! chain per group, on both surfaces, in fresh work directories per surface.
//! Dynamic values (entry ids, run ids, revisions) flow through
//! `{{STEP:/json/pointer[#task]}}` markers that each side resolves from its own
//! prior outputs; the `cli` pseudo-tool runs an owner-only setup step through
//! the command line inside an MCP chain.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::{json, Value};

use spiderhead_cli::{run, Output};

/// Chain order is recorded by the generator: first appearance in this order
/// assigns the shared `<UUID:n>` / `<HEX64:n>` numbers.
fn chains(fixture: &Value) -> Vec<String> {
    fixture["order"]
        .as_array()
        .expect("order")
        .iter()
        .filter_map(|name| name.as_str().map(str::to_owned))
        .collect()
}

/// The oracle's normalisation plus the two group-3 rules: ledger-random 64-hex
/// tokens number by first appearance, and JSON stdout is canonicalised (sorted
/// keys) because the oracle's summary `counts` order is a frozenset iteration
/// order that differs per process.
struct Norm {
    works: std::cell::RefCell<Vec<(String, String)>>,
    uuids: BTreeMap<String, String>,
    hex64: BTreeMap<String, String>,
    uuid_re: Regex,
    hex_re: Regex,
    evid_re: Regex,
    ts_re: Regex,
    port_re: Regex,
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
            .port_re
            .replace_all(&text, "127.0.0.1:{PORT}")
            .into_owned();
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
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/control/control.json");
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
    let re = Regex::new(r"\{\{(\d+):(/[^\}#]*)(#task)?\}\}").expect("marker regex");
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
            Value::String(text) if caps.get(3).is_some() => {
                format!(
                    "custom:goal-{}",
                    text.strip_prefix("inbox:").unwrap_or(text)
                )
            }
            Value::String(text) => text.clone(),
            other => other.to_string(),
        }
    })
    .into_owned()
}

fn fresh_work(tag: &str) -> PathBuf {
    let work = std::env::temp_dir().join(format!("sh-control-parity-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("work dir");
    work
}

/// The static work-root inputs of every chain: the retained oracle scan the BI
/// package is built from, the offline cohort inputs and the host allowlist the
/// destination previews read (the env var names it for this surface's run).
fn prepare_work(work: &Path) {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let raw = crate_dir.join("../../oracle/raw-scans/basic");
    std::fs::copy(raw.join("scan.sqlite"), work.join("scan-basic.sqlite")).expect("scan copy");
    std::fs::copy(
        raw.join("scan.sqlite.audit-v2.sqlite"),
        work.join("scan-basic.sqlite.audit-v2.sqlite"),
    )
    .expect("companion copy");
    for name in ["publication_cohort_input.json", "gsc_progress_input.json"] {
        std::fs::copy(
            crate_dir.join("tests/fixtures/control/inputs").join(name),
            work.join(name),
        )
        .expect("input copy");
    }
    let datasets = [
        "pages",
        "findings",
        "metrics",
        "link_occurrences",
        "cohorts",
        "coverage",
    ];
    let worksheets: Value = datasets
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let mut title = name.to_string();
            title[..1].make_ascii_uppercase();
            (
                name.to_string(),
                json!({"worksheet_id": index, "worksheet_title": title}),
            )
        })
        .collect::<serde_json::Map<_, _>>()
        .into();
    let tables: Value = datasets
        .iter()
        .map(|name| (name.to_string(), json!({"table_id": format!("t_{name}")})))
        .collect::<serde_json::Map<_, _>>()
        .into();
    let config = json!({
        "sheets": {"targets": {
            "main-sheet": {
                "enabled": true, "kind": "google_sheets_service_account",
                "spreadsheet_id": "sheet-main-1", "worksheets": worksheets,
            },
            "elsewhere": {"enabled": false},
        }},
        "bigquery": {"targets": {
            "bq-main": {
                "enabled": true, "kind": "google_bigquery_service_account",
                "project_id": "proj-1", "dataset_id": "seo", "location": "EU",
                "cost_authorized": true, "tables": tables,
            },
        }},
    });
    std::fs::write(
        work.join("bi-destinations.json"),
        serde_json::to_string(&config).expect("config json"),
    )
    .expect("config write");
    std::env::set_var(
        "SEOHEAD_BI_DESTINATIONS_FILE",
        work.join("bi-destinations.json"),
    );
    let base = "https://github.com/PavloSEO/spiderhead/releases";
    let state = json!({
        "format": "spiderhead.update-check.v1",
        "checked_at": "2026-10-01T00:00:00Z",
        "result": {
            "state": "available",
            "current_version": "3.2.0",
            "update_available": true,
            "latest": null,
            "releases": [{
                "tag": "v9.9.9", "version": "9.9.9", "url": format!("{base}/tag/v9.9.9"),
                "published_at": null, "changelog": "",
                "assets": [
                    {"name": "spiderhead-9.9.9-py3-none-any.whl",
                     "url": format!("{base}/download/v9.9.9/spiderhead-9.9.9-py3-none-any.whl"),
                     "size": 10, "sha256": "ab".repeat(32)},
                    {"name": "notes.txt", "url": format!("{base}/download/v9.9.9/notes.txt"),
                     "size": 3, "sha256": "cd".repeat(32)},
                ],
            }],
        },
    });
    std::fs::write(
        work.join("update-state.json"),
        serde_json::to_string(&state).expect("state json"),
    )
    .expect("state write");
    std::env::set_var("SEOHEAD_UPDATE_STATE_PATH", work.join("update-state.json"));
    // Keep the doctor from finding a crawler installed on the host.
    std::env::set_var("SPIDERHEAD_DOCTOR_APP_DIRS", "");
}

/// The doctor keys that name the interpreter, optional modules or the live
/// port are environment-bound and absent from both sides' comparison.
fn scrub_doctor(text: &str) -> String {
    let Ok(mut document) = serde_json::from_str::<Value>(text) else {
        return text.to_owned();
    };
    if document["format"] != "spiderhead.doctor.v1" {
        return text.to_owned();
    }
    if let Some(object) = document.as_object_mut() {
        for key in ["python", "playwright", "mcp", "mcp_http_bind"] {
            object.remove(key);
        }
    }
    if let Some(sqlite) = document["sqlite"].as_object_mut() {
        sqlite.remove("version");
    }
    document.to_string()
}

/// One CLI step through the in-process registry.
fn cli_step(argv: &[String], registry: &spiderhead_cli::Registry) -> (i32, Output) {
    let mut out = Output::default();
    let code = run(argv, registry, &mut out).expect("handler run");
    (code.0.into(), out)
}

#[test]
fn control_group_matches_the_reference_cli_and_mcp() {
    let fixture = fixture();

    // ── CLI: the chains replay through the real binary in its own work dir ──
    let cli_work = fresh_work("cli");
    prepare_work(&cli_work);
    let registry = spiderhead_cli::registry();
    let mut norm = Norm::new(&cli_work);
    for chain in chains(&fixture) {
        let cases: Vec<(String, &Value)> = fixture["cases"]
            .as_object()
            .expect("cases")
            .iter()
            .filter(|(_, case)| case["chain"].as_str() == Some(chain.as_str()))
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
            let actual_stdout = canonical(&scrub_doctor(&norm.s(&out.stdout)));
            let expected_stdout = case["stdout"].as_str().unwrap_or_default();
            if actual_stdout != canonical(expected_stdout) {
                problems += &unified_diff("stdout", expected_stdout, &actual_stdout);
            }
            let actual_stderr = norm.s(&out.stderr);
            let expected_stderr = case["stderr"].as_str().unwrap_or_default();
            if actual_stderr != expected_stderr {
                problems += &unified_diff("stderr", expected_stderr, &actual_stderr);
            }
            assert!(problems.is_empty(), "{name} diverges:\n{problems}");
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
    for chain in chains(&fixture) {
        let calls: Vec<(String, &Value)> = fixture["mcp"]
            .as_object()
            .expect("mcp")
            .iter()
            .filter(|(_, call)| call["chain"].as_str() == Some(chain.as_str()))
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
            if call["tool"].as_str() == Some("cli") {
                let argv: Vec<String> = arguments["argv"]
                    .as_array()
                    .expect("cli argv")
                    .iter()
                    .map(|part| part.as_str().unwrap_or_default().to_owned())
                    .collect();
                let (code, out) = cli_step(&argv, &spiderhead_cli::registry());
                history.push(json!({"stdout": out.stdout}));
                assert_eq!(code, 0, "{name}: setup step failed: {}", out.stderr);
                let normalized = canonical(&norm.s(&out.stdout));
                let expected_text = call["text"].as_str().unwrap_or_default();
                assert!(
                    normalized == canonical(expected_text),
                    "{name} diverges:\n{}",
                    unified_diff("text", expected_text, &normalized)
                );
                continue;
            }
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
            let normalized = canonical(&scrub_doctor(&norm.s(&actual_text)));
            let expected_text = call["text"].as_str().unwrap_or_default();
            if normalized != canonical(expected_text) {
                problems += &unified_diff("text", expected_text, &normalized);
            }
            assert!(problems.is_empty(), "{name} diverges:\n{problems}");
        }
    }
    let _ = std::fs::remove_dir_all(&mcp_work);
}

/// `{WORK}` placeholders in recorded arguments resolve to the surface's work dir.
fn resolve(arguments: &str, work: &Path) -> Value {
    let resolved = arguments.replace("{WORK}", &work.to_string_lossy());
    serde_json::from_str(&resolved).expect("arguments json")
}

//! Parity of the scan-store command group: the Rust CLI and MCP surfaces
//! reproduce the reference baseline fixtures (stdout, stderr, exit code, MCP text and
//! the files a command writes) for every case in `cases.json`.

use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::{json, Value};
use spiderhead_cli::{run, Output};
use spiderhead_mcp::profile::Profile;
use spiderhead_mcp::server::Server;

fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/scan_store")
}

fn oracle_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../oracle")
}

/// The oracle's text normalisation: volatile shapes become stable tokens.
struct Norm {
    work: String,
    uuids: std::collections::BTreeMap<String, String>,
    patterns: [Regex; 4],
}

impl Norm {
    fn new(work: &Path) -> Norm {
        let re = |text: &str| Regex::new(text).expect("regex");
        Norm {
            work: work.to_string_lossy().into_owned(),
            uuids: Default::default(),
            patterns: [
                re(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}"),
                re(r"(evidence:v1:(<UUID:\d+)>:\w+:)[0-9a-f]{24}"),
                re(r"\d{4}-\d\d-\d\d[T ]\d\d:\d\d:\d\d(\.\d+)?(Z|[+-]\d\d:?\d\d)?"),
                re(concat!(
                    r#"("(?:crawl_delay_applied_at|delay_seconds|duration|duration_ms|elapsed_ms|"#,
                    r#"elapsed_seconds|latency_ms|response_time|time_ms|total_ms|ttfb_ms|"#,
                    r#"ttfb_first_ms|ttfb_second_ms|device|inode|mtime_ns)": )-?[0-9][0-9.eE+-]*"#
                )),
            ],
        }
    }

    fn s(&mut self, text: &str) -> String {
        let text = text.replace(&self.work, "<WORK>");
        let text = Regex::new(r"127\.0\.0\.1:\d+")
            .expect("port")
            .replace_all(&text, "127.0.0.1:{PORT}")
            .into_owned();
        let mut text = text;
        let found: Vec<String> = self.patterns[0]
            .find_iter(&text)
            .map(|m| m.as_str().to_owned())
            .collect();
        for uuid in found {
            let next = self.uuids.len() + 1;
            let token = self
                .uuids
                .entry(uuid.clone())
                .or_insert_with(|| format!("<UUID:{next}>"))
                .clone();
            text = text.replace(&uuid, &token);
        }
        let text = Regex::new(r#"("digest": ")[0-9a-f]{64}"#)
            .expect("digest")
            .replace_all(&text, "${1}<DIGEST>")
            .into_owned();
        let text = Regex::new(r"\d{8}T\d{6}Z")
            .expect("compact timestamp")
            .replace_all(&text, "<TS>")
            .into_owned();
        let text = self.patterns[1].replace_all(&text, "${1}<HASH>");
        let text = self.patterns[2].replace_all(&text, "<TS>");
        self.patterns[3]
            .replace_all(&text, "${1}<DURATION>")
            .into_owned()
    }
}

/// Replace `{TOKEN}` placeholders in every string of a JSON value.
fn fill(value: &Value, tokens: &[(&str, String)]) -> Value {
    match value {
        Value::String(text) => {
            let mut text = text.clone();
            for (key, replacement) in tokens {
                text = text.replace(&format!("{{{key}}}"), replacement);
            }
            Value::String(text)
        }
        Value::Array(items) => Value::Array(items.iter().map(|v| fill(v, tokens)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), fill(v, tokens)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// A fresh work directory with the site's scan (and its second copy) and the placeholder values.
fn make_work(site: &str, tag: &str) -> (PathBuf, Vec<(&'static str, String)>) {
    let work = std::env::temp_dir().join(format!(
        "sh-scanstore-{site}-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("work dir");
    let raw = oracle_root().join("raw-scans").join(site);
    for name in ["scan.sqlite", "scan.sqlite.audit-v2.sqlite"] {
        if raw.join(name).exists() {
            std::fs::copy(raw.join(name), work.join(name)).expect("copy scan");
        }
    }
    std::fs::copy(work.join("scan.sqlite"), work.join("scan2.sqlite")).expect("copy scan2");
    let con = rusqlite::Connection::open_with_flags(
        work.join("scan.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("scan opens");
    let start: String = con
        .query_row("SELECT start_url FROM scan WHERE singleton=1", [], |r| {
            r.get(0)
        })
        .expect("start_url");
    let tokens = vec![
        ("WORK", work.to_string_lossy().into_owned()),
        (
            "SCAN",
            work.join("scan.sqlite").to_string_lossy().into_owned(),
        ),
        (
            "SCAN2",
            work.join("scan2.sqlite").to_string_lossy().into_owned(),
        ),
        ("BASE", start.trim_end_matches('/').to_owned()),
        ("START", start),
    ];
    (work, tokens)
}

fn seed_files(case: &Value, work: &Path, tokens: &[(&str, String)]) {
    for (name, text) in case["files_in"].as_object().into_iter().flatten() {
        let text = fill(text, tokens);
        std::fs::write(work.join(name), text.as_str().unwrap_or_default()).expect("seed file");
    }
}

fn collect_files(work: &Path, names: &[Value], norm: &mut Norm) -> Value {
    let mut files = serde_json::Map::new();
    for name in names.iter().filter_map(Value::as_str) {
        let target = work.join(name);
        let mut paths = Vec::new();
        if target.is_dir() {
            let mut stack = vec![target];
            while let Some(dir) = stack.pop() {
                for entry in std::fs::read_dir(dir).expect("read dir").flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        stack.push(path);
                    } else {
                        paths.push(path);
                    }
                }
            }
        } else if target.exists() {
            paths.push(target);
        }
        for path in paths {
            let bytes = std::fs::read(&path).unwrap_or_default();
            let key = path.strip_prefix(work).unwrap_or(&path).to_string_lossy();
            let text = match String::from_utf8(bytes.clone()) {
                Ok(text) => norm.s(&text),
                Err(_) => {
                    use sha2::{Digest, Sha256};
                    let hex: String = Sha256::digest(&bytes)
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect();
                    format!("sha256:{hex}")
                }
            };
            files.insert(key.into_owned(), Value::String(text));
        }
    }
    Value::Object(files)
}

fn run_cli(case: &Value, site: &str) -> Value {
    let (work, tokens) = make_work(site, "cli");
    seed_files(case, &work, &tokens);
    let mut norm = Norm::new(&work);
    let registry = spiderhead_cli::registry();
    let mut steps = vec![case];
    steps.extend(case["then"].as_array().into_iter().flatten());
    let mut outputs = Vec::new();
    for step in steps {
        spiderhead_store::scan_artifact::forget_validated();
        let command = step["command"].as_str().unwrap_or_default().to_owned();
        let argv = vec![
            command,
            "--input".to_owned(),
            fill(&step["args"], &tokens).to_string(),
        ];
        let mut out = Output::default();
        let code = run(&argv, &registry, &mut out).expect("cli run");
        outputs.push(json!({
            "exit": code.0,
            "stdout": norm.s(&out.stdout),
            "stderr": norm.s(&out.stderr),
        }));
    }
    let empty = Vec::new();
    let names = case["collect"].as_array().unwrap_or(&empty);
    let files = collect_files(&work, names, &mut norm);
    let _ = std::fs::remove_dir_all(&work);
    json!({"steps": outputs, "files": files})
}

fn run_mcp(case: &Value, site: &str, server: &Server) -> Value {
    spiderhead_projects::action_log::set_interface("mcp");
    let (work, tokens) = make_work(site, "mcp");
    seed_files(case, &work, &tokens);
    let mut norm = Norm::new(&work);
    let mut steps = vec![case];
    steps.extend(case["then"].as_array().into_iter().flatten());
    let mut outputs = Vec::new();
    for (number, step) in steps.into_iter().enumerate() {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1000 + number,
            "method": "tools/call",
            "params": {"name": step["tool"], "arguments": fill(&step["args"], &tokens)},
        });
        let mut replies: Vec<Value> = Vec::new();
        server.handle_line(&request.to_string(), &mut replies);
        let result = replies
            .iter()
            .find(|reply| reply.get("id") == Some(&json!(1000 + number)))
            .map_or(Value::Null, |reply| reply["result"].clone());
        outputs.push(json!({
            "is_error": result.get("isError").and_then(Value::as_bool).unwrap_or(false),
            "text": norm.s(result["content"][0]["text"].as_str().unwrap_or_default()),
        }));
    }
    let empty = Vec::new();
    let names = case["collect"].as_array().unwrap_or(&empty);
    let files = collect_files(&work, names, &mut norm);
    let _ = std::fs::remove_dir_all(&work);
    json!({"steps": outputs, "files": files})
}

/// The first path at which two JSON values differ, with both sides.
fn first_difference(expected: &Value, actual: &Value) -> String {
    fn walk(path: &str, left: &Value, right: &Value) -> Option<String> {
        match (left, right) {
            (Value::Object(a), Value::Object(b)) => {
                for key in a.keys().chain(b.keys()) {
                    let missing = Value::String("<absent>".into());
                    let (x, y) = (
                        a.get(key).unwrap_or(&missing),
                        b.get(key).unwrap_or(&missing),
                    );
                    if let Some(found) = walk(&format!("{path}/{key}"), x, y) {
                        return Some(found);
                    }
                }
                None
            }
            (Value::Array(a), Value::Array(b)) if a.len() == b.len() => a
                .iter()
                .zip(b)
                .enumerate()
                .find_map(|(i, (x, y))| walk(&format!("{path}/{i}"), x, y)),
            (Value::String(a), Value::String(b)) if a != b => {
                let line = a.lines().zip(b.lines()).find(|(x, y)| x != y).map_or_else(
                    || {
                        format!(
                            "length {} vs {} lines",
                            a.lines().count(),
                            b.lines().count()
                        )
                    },
                    |(x, y)| format!("python: {x}\n  rust:   {y}"),
                );
                Some(format!("{path}\n  {line}"))
            }
            _ if left == right => None,
            _ => Some(format!("{path}\n  python: {left}\n  rust:   {right}")),
        }
    }
    let text = walk("", expected, actual).unwrap_or_default();
    text.chars().take(1500).collect()
}

/// Every case, every site, both surfaces: 0 divergences.
#[test]
fn scan_store_commands_match_the_reference_oracle() {
    let raw = std::fs::read_to_string(fixtures_root().join("cases.json")).expect("cases");
    let cases: Vec<Value> = serde_json::from_str(&raw).expect("cases json");
    let server = Server::new(Profile::Full);
    let mut sink: Vec<Value> = Vec::new();
    server.handle_line(
        r#"{"jsonrpc":"2.0","id":900,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"parity","version":"0"}}}"#,
        &mut sink,
    );
    server.handle_line(
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        &mut sink,
    );
    let mut failures = Vec::new();
    for case in &cases {
        let id = case["id"].as_str().unwrap_or_default();
        let sites: Vec<&str> = case["sites"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_else(|| vec!["basic"]);
        for site in sites {
            let path = fixtures_root().join(site).join(format!("{id}.json"));
            let fixture: Value =
                serde_json::from_str(&std::fs::read_to_string(&path).expect("fixture"))
                    .expect("fixture json");
            for (surface, actual) in [
                ("cli", run_cli(case, site)),
                ("mcp", run_mcp(case, site, &server)),
            ] {
                if actual != fixture[surface] {
                    failures.push(format!(
                        "{site}/{id} ({surface}): {}",
                        first_difference(&fixture[surface], &actual)
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

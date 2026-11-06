//! Parity of the rest-b command group: `log-scan`, `gsc-archive`,
//! `metrika-traffic-pdf` and the Cloudflare log imports reproduce the Python
//! oracle fixtures (stdout, stderr, exit code, MCP text, written files and the
//! wire requests a scripted loopback provider saw) for every case in
//! `tests/fixtures/rest/cases.json`.
//!
//! A case may name `setup` steps run before the command itself, `then` steps
//! run after it, `files_in` seeds, `collect` output files, `env` credentials
//! and a `routes` script served by a 127.0.0.1 provider stand-in through
//! `SPIDERHEAD_PROVIDER_LOOPBACK`. No real provider is ever called.

#[path = "common/fake_net.rs"]
mod fake_net;

use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::{json, Map, Value};
use spiderhead_cli::{run, Output};
use spiderhead_mcp::profile::Profile;
use spiderhead_mcp::server::Server;

/// Every credential variable the providers read — unset before each case.
const CREDENTIAL_VARS: [&str; 23] = [
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
    "TOPVISOR_TOKEN",
    "TOPVISOR_USER_ID",
    "SPIDERHEAD_CONFIG_DIR",
    "SEOHEAD_CHROME",
];

fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rest")
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
                    r#"ttfb_first_ms|ttfb_second_ms|device|inode|mtime_ns|retry_at)": )-?[0-9][0-9.eE+-]*"#
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
        let text = Regex::new(r#"("(?:digest|sha256)": ")[0-9a-f]{64}"#)
            .expect("digest")
            .replace_all(&text, "${1}<DIGEST>")
            .into_owned();
        let text = Regex::new(r#"("session_id": ")[0-9a-f]{32}"#)
            .expect("session")
            .replace_all(&text, "${1}<HEX>")
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

fn seed_files(case: &Value, work: &Path, tokens: &[(&str, String)]) {
    for site in case["scan_seeds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../oracle/raw-scans")
            .join(site);
        for entry in std::fs::read_dir(&source).expect("scan seed dir").flatten() {
            let path = entry.path();
            if path.is_file() {
                std::fs::copy(&path, work.join(entry.file_name())).expect("seed scan");
            }
        }
    }
    for (name, text) in case["files_in"].as_object().into_iter().flatten() {
        let path = work.join(name);
        std::fs::create_dir_all(path.parent().expect("seed parent")).expect("seed dir");
        let text = fill(text, tokens);
        std::fs::write(&path, text.as_str().unwrap_or_default()).expect("seed file");
    }
}

fn collect_files(work: &Path, names: &[Value], norm: &mut Norm) -> Value {
    let mut files = Map::new();
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
            paths.sort();
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

/// The ordered step list of one case: setup, the command itself, follow-ups.
fn steps(case: &Value) -> Vec<&Value> {
    let mut steps: Vec<&Value> = case["setup"].as_array().into_iter().flatten().collect();
    steps.push(case);
    steps.extend(case["then"].as_array().into_iter().flatten());
    steps
}

/// Per-case process environment: isolated HOME, cleared credentials, loopback.
fn prepare(work: &Path, origin: Option<&str>, case: &Value) {
    // `env::set_var` is process-global; this test is the only one touching it.
    std::env::set_var("HOME", work);
    for name in CREDENTIAL_VARS {
        std::env::remove_var(name);
    }
    std::env::set_var("NO_PROXY", "127.0.0.1");
    std::env::set_var("no_proxy", "127.0.0.1");
    match origin {
        Some(origin) => std::env::set_var("SPIDERHEAD_PROVIDER_LOOPBACK", origin),
        None => std::env::remove_var("SPIDERHEAD_PROVIDER_LOOPBACK"),
    }
    if let Some(extra) = case["env"].as_object() {
        for (name, value) in extra {
            std::env::set_var(name, value.as_str().unwrap_or_default());
        }
    }
}

fn clear_extra_env(case: &Value) {
    if let Some(extra) = case["env"].as_object() {
        for name in extra.keys() {
            std::env::remove_var(name);
        }
    }
}

fn fresh_work(tag: &str) -> PathBuf {
    let work = std::env::temp_dir()
        .join(format!(
            "sh-rest-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ))
        .canonicalize()
        .unwrap_or_else(|_| std::env::temp_dir().join(format!("sh-rest-{tag}-x")));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("work dir");
    work.canonicalize().expect("canonical work dir")
}

fn run_cli(case: &Value, origin: Option<&str>) -> Value {
    let work = fresh_work("cli");
    let tokens = vec![
        ("WORK", work.to_string_lossy().into_owned()),
        ("ORIGIN", origin.unwrap_or_default().to_owned()),
    ];
    seed_files(case, &work, &tokens);
    prepare(&work, origin, case);
    let mut norm = Norm::new(&work);
    let registry = spiderhead_cli::registry();
    let mut outputs = Vec::new();
    for step in steps(case) {
        spiderhead_store::scan_artifact::forget_validated();
        let argv = if let Some(flags) = step["flags"].as_array() {
            let mut argv = vec![step["command"].as_str().unwrap_or_default().to_owned()];
            argv.extend(
                fill(&Value::Array(flags.clone()), &tokens)
                    .as_array()
                    .expect("flags")
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned)),
            );
            argv
        } else {
            vec![
                step["command"].as_str().unwrap_or_default().to_owned(),
                "--input".to_owned(),
                fill(&step["args"], &tokens).to_string(),
            ]
        };
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
    clear_extra_env(case);
    let _ = std::fs::remove_dir_all(&work);
    json!({"steps": outputs, "files": files})
}

fn run_mcp(case: &Value, origin: Option<&str>) -> Value {
    spiderhead_projects::action_log::set_interface("mcp");
    let work = fresh_work("mcp");
    let tokens = vec![
        ("WORK", work.to_string_lossy().into_owned()),
        ("ORIGIN", origin.unwrap_or_default().to_owned()),
    ];
    seed_files(case, &work, &tokens);
    prepare(&work, origin, case);
    let mut norm = Norm::new(&work);
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
    let mut outputs = Vec::new();
    for (number, step) in steps(case).into_iter().enumerate() {
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
    clear_extra_env(case);
    let _ = std::fs::remove_dir_all(&work);
    json!({"steps": outputs, "files": files})
}

/// Recorded wire requests, normalised the same way as stdout.
fn requests_value(server: &fake_net::FakeProviders, norm: &mut Norm) -> Value {
    let raw = Value::Array(server.requests());
    let text = norm.s(&raw.to_string());
    serde_json::from_str(&text).expect("requests json")
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

/// Every case, both surfaces, zero divergences. With `SPIDERHEAD_BLESS=1` each
/// case's fixture is rewritten from the freshly computed outcome instead.
#[test]
fn rest_commands_match_the_python_oracle() {
    let bless = spiderhead_oracle_diff::bless_enabled();
    let raw = std::fs::read_to_string(fixtures_root().join("cases.json")).expect("cases");
    let cases: Vec<Value> = serde_json::from_str(&raw).expect("cases json");
    let mut failures = Vec::new();
    for case in &cases {
        let id = case["id"].as_str().unwrap_or_default();
        let path = fixtures_root().join(format!("{id}.json"));
        let fixture: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("fixture"))
                .expect("fixture json");
        let mut blessed = Map::new();
        for surface in ["cli", "mcp"] {
            let has_routes = case["routes"].as_array().is_some();
            // Routes may embed the origin, so the port is learned before they are
            // filled; the probe's just-released port is what the server rebinds.
            let (server, origin) = if has_routes {
                let probe = fake_net::FakeProviders::start(&json!([]));
                let origin = probe.origin.clone();
                drop(probe);
                let work = fresh_work("route-fill");
                let tokens = vec![
                    ("WORK", work.to_string_lossy().into_owned()),
                    ("ORIGIN", origin.clone()),
                ];
                let server = fake_net::FakeProviders::start(&fill(&case["routes"], &tokens));
                let _ = std::fs::remove_dir_all(&work);
                let origin = server.origin.clone();
                (Some(server), Some(origin))
            } else {
                (None, None)
            };
            let _ = origin;
            let actual = match surface {
                "cli" => run_cli(case, origin.as_deref()),
                _ => run_mcp(case, origin.as_deref()),
            };
            if bless {
                let mut fresh = actual.clone();
                fresh["requests"] = json!([]);
                if let Some(server) = &server {
                    let mut norm = Norm::new(Path::new("/nonexistent"));
                    fresh["requests"] = requests_value(server, &mut norm);
                }
                blessed.insert(surface.to_owned(), fresh);
                continue;
            }
            let mut expected = fixture[surface].clone();
            if let Some(server) = &server {
                let mut norm = Norm::new(Path::new("/nonexistent"));
                let actual_requests = requests_value(server, &mut norm);
                let _ = &mut expected;
                if expected["requests"] != actual_requests {
                    failures.push(format!(
                        "{id} ({surface}) requests: {}",
                        first_difference(&expected["requests"], &actual_requests)
                    ));
                }
            }
            let mut actual_body = actual.clone();
            if let Some(map) = actual_body.as_object_mut() {
                map.remove("requests");
            }
            if let Some(map) = expected.as_object_mut() {
                map.remove("requests");
            }
            if actual_body != expected {
                failures.push(format!(
                    "{id} ({surface}): {}",
                    first_difference(&expected, &actual_body)
                ));
            }
        }
        if bless {
            spiderhead_oracle_diff::bless_json(&path, &Value::Object(blessed));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

//! Integration test: the networked source/provider commands reproduce the
//! reference fixtures captured by the reference suite on both surfaces.
//!
//! Each case replays against a scripted loopback server (the provider hosts are
//! re-aimed through `SPIDERHEAD_PROVIDER_LOOPBACK`), under an isolated `HOME`
//! with only the seeded test credentials, and must match stdout, stderr, exit
//! code and the exact wire requests the oracle made. No real provider is called.

#[path = "common/fake_net.rs"]
mod fake_net;

use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::{json, Value};

use spiderhead_cli::{run, Output};
use spiderhead_mcp::profile::Profile;
use spiderhead_mcp::server::Server;

/// Every credential variable the registry names — unset before each case.
const CREDENTIAL_VARS: [&str; 22] = [
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
];

fn normalize(text: &str, work: &Path, dates: bool) -> String {
    let text = text.replace(work.to_string_lossy().as_ref(), "<WORK>");
    let port = Regex::new(r"127\.0\.0\.1:\d+").expect("port regex");
    let uuid =
        Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}").expect("uuid");
    let ts = Regex::new(r"\d{4}-\d\d-\d\d[T ]\d\d:\d\d:\d\d(\.\d+)?(Z|[+-]\d\d:?\d\d)?")
        .expect("ts regex");
    let date = Regex::new(r"\d{4}-\d\d-\d\d").expect("date regex");
    let millis = Regex::new(r#"("response_time_ms": )[0-9.]+"#).expect("millis regex");
    let text = port.replace_all(&text, "127.0.0.1:<PORT>");
    let text = millis.replace_all(&text, "${1}<MS>");
    let text = uuid.replace_all(&text, "<UUID>");
    let text = ts.replace_all(&text, "<TS>");
    if dates {
        date.replace_all(&text, "<DATE>").into_owned()
    } else {
        text.into_owned()
    }
}

/// ISO date `days` before today (UTC), for the `{DAY-n}` placeholders.
fn day_before(days: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let z = now.div_euclid(86_400) - days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// `{WORK}` / `{ORIGIN}` placeholders resolved anywhere inside a JSON value.
fn fill(value: &Value, work: &Path, origin: &str) -> Value {
    let work_json = serde_json::to_string(work.to_string_lossy().as_ref()).expect("work json");
    let mut text = value
        .to_string()
        .replace("{WORK}", &work_json[1..work_json.len() - 1])
        .replace("{ORIGIN}", origin);
    let day = Regex::new(r"\{DAY-(\d+)\}").expect("day regex");
    let resolved = day
        .replace_all(&text, |caps: &regex::Captures| {
            day_before(caps[1].parse().unwrap_or(0))
        })
        .into_owned();
    text = resolved;
    serde_json::from_str(&text).expect("filled json")
}

fn unified_diff(field: &str, expected: &str, actual: &str) -> String {
    let mut diff = String::new();
    let mut left_lines = expected.lines();
    let mut right_lines = actual.lines();
    let mut line_no = 0;
    loop {
        line_no += 1;
        let (left, right) = (left_lines.next(), right_lines.next());
        if left.is_none() && right.is_none() {
            break;
        }
        let (left, right) = (left.unwrap_or("<end>"), right.unwrap_or("<end>"));
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

fn seed(work: &Path, files: &Value) {
    use spiderhead_core::filesystem::set_permissions_mode;
    for entry in files.as_array().cloned().unwrap_or_default() {
        let path = work.join(entry["path"].as_str().expect("seed path"));
        std::fs::create_dir_all(path.parent().expect("seed parent")).expect("seed dir");
        std::fs::write(&path, entry["text"].as_str().unwrap_or_default()).expect("seed file");
        let mode = entry["mode"].as_u64().unwrap_or(0o600) as u32;
        set_permissions_mode(&path, mode).expect("seed mode");
    }
}

fn prepare(work: &Path, origin: &str, case: &Value) {
    // `env::set_var` is process-global; this test is the only one touching it.
    std::env::set_var("HOME", work);
    for name in CREDENTIAL_VARS {
        std::env::remove_var(name);
    }
    std::env::set_var("NO_PROXY", "127.0.0.1");
    std::env::set_var("no_proxy", "127.0.0.1");
    std::env::set_var("SPIDERHEAD_PROVIDER_LOOPBACK", origin);
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
        .join(format!("sh-net-parity-{tag}-{}", std::process::id()))
        .canonicalize()
        .unwrap_or_else(|_| std::env::temp_dir().join(format!("sh-net-parity-{tag}")));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("work dir");
    work.canonicalize().expect("canonical work dir")
}

fn compare_requests(
    expected: &Value,
    server: &fake_net::FakeProviders,
    work: &Path,
    dates: bool,
    unordered: bool,
) -> String {
    let actual: Value = serde_json::from_str(&normalize(
        &Value::Array(server.requests()).to_string(),
        work,
        dates,
    ))
    .expect("requests json");
    // Concurrent donor fetches reach the server in no fixed order.
    let sorted = |value: &Value| {
        let mut items = value.as_array().cloned().unwrap_or_default();
        items.sort_by_key(Value::to_string);
        Value::Array(items)
    };
    if &actual == expected || (unordered && sorted(&actual) == sorted(expected)) {
        return String::new();
    }
    let pretty = |value: &Value| serde_json::to_string_pretty(value).expect("pretty");
    unified_diff("requests", &pretty(expected), &pretty(&actual))
}

fn run_cli_case(name: &str, case: &Value) -> String {
    let work = fresh_work(&format!("cli-{}", std::process::id()));
    // Routes may embed the origin, so the port is learned before they are filled.
    let probe = fake_net::FakeProviders::start(&json!([]));
    let origin = probe.origin.clone();
    drop(probe);
    let server = fake_net::FakeProviders::start(&fill(&case["routes"], &work, &origin));
    let origin = server.origin.clone();
    seed(&work, &fill(&case["seed"], &work, &origin));
    prepare(&work, &origin, case);
    let argv: Vec<String> = fill(&case["cli"]["argv"], &work, &origin)
        .as_array()
        .expect("argv")
        .iter()
        .map(|part| part.as_str().unwrap_or_default().to_owned())
        .collect();
    let registry = spiderhead_cli::registry();
    let mut out = Output::default();
    let code = run(&argv, &registry, &mut out).expect("handler run");
    let dates = case["relative_dates"].as_bool().unwrap_or(false);
    let expect = &case["expect"]["cli"];
    let mut problems = String::new();
    if i64::from(code.0) != expect["exit"].as_i64().unwrap_or_default() {
        problems += &format!("  exit: expected {}, got {}\n", expect["exit"], code.0);
    }
    for (field, actual) in [("stdout", &out.stdout), ("stderr", &out.stderr)] {
        let actual = normalize(actual, &work, dates);
        let wanted = expect[field].as_str().unwrap_or_default();
        if actual != wanted {
            problems += &unified_diff(field, wanted, &actual);
        }
    }
    problems += &compare_requests(
        &expect["requests"],
        &server,
        &work,
        dates,
        case["unordered_requests"].as_bool().unwrap_or(false),
    );
    clear_extra_env(case);
    let _ = std::fs::remove_dir_all(&work);
    if problems.is_empty() {
        String::new()
    } else {
        format!("{name} (cli) diverges:\n{problems}")
    }
}

fn run_mcp_case(name: &str, case: &Value) -> String {
    let work = fresh_work("mcp");
    let probe = fake_net::FakeProviders::start(&json!([]));
    let origin = probe.origin.clone();
    drop(probe);
    let server = fake_net::FakeProviders::start(&fill(&case["routes"], &work, &origin));
    let origin = server.origin.clone();
    seed(&work, &fill(&case["seed"], &work, &origin));
    prepare(&work, &origin, case);
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
    let call = fill(&case["mcp"], &work, &origin);
    let request = json!({
        "jsonrpc": "2.0", "id": 1000, "method": "tools/call",
        "params": {"name": call["tool"], "arguments": call["arguments"]},
    });
    let mut replies: Vec<Value> = Vec::new();
    mcp.handle_line(&request.to_string(), &mut replies);
    let reply = replies
        .iter()
        .find(|reply| reply.get("id") == Some(&json!(1000)))
        .unwrap_or_else(|| panic!("{name}: no reply"));
    let result = &reply["result"];
    let dates = case["relative_dates"].as_bool().unwrap_or(false);
    let expect = &case["expect"]["mcp"];
    let mut problems = String::new();
    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if is_error != expect["is_error"].as_bool().unwrap_or(false) {
        problems += &format!(
            "  is_error: expected {}, got {is_error}\n",
            expect["is_error"]
        );
    }
    let text = result["content"][0]["text"].as_str().unwrap_or_default();
    let actual = normalize(text, &work, dates);
    let wanted = expect["text"].as_str().unwrap_or_default();
    if actual != wanted {
        problems += &unified_diff("text", wanted, &actual);
    }
    problems += &compare_requests(
        &expect["requests"],
        &server,
        &work,
        dates,
        case["unordered_requests"].as_bool().unwrap_or(false),
    );
    clear_extra_env(case);
    let _ = std::fs::remove_dir_all(&work);
    if problems.is_empty() {
        String::new()
    } else {
        format!("{name} (mcp) diverges:\n{problems}")
    }
}

#[test]
fn networked_sources_match_the_reference_oracle() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sources_net");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("fixture dir")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    let mut failures = String::new();
    let mut count = 0;
    for file in files {
        let raw = std::fs::read_to_string(&file).expect("fixture file");
        let fixture: Value = serde_json::from_str(&raw).expect("fixture json");
        for case in fixture["cases"].as_array().expect("cases") {
            let name = format!(
                "{}/{}",
                fixture["group"].as_str().unwrap_or_default(),
                case["name"].as_str().unwrap_or_default()
            );
            if case.get("cli").is_some() {
                failures += &run_cli_case(&name, case);
                count += 1;
            }
            if case.get("mcp").is_some() {
                failures += &run_mcp_case(&name, case);
                count += 1;
            }
        }
    }
    assert!(count > 0, "no cases ran");
    assert!(failures.is_empty(), "{failures}");
}

//! Integration test: the Rust recon checks (cdn, tech, security, regions,
//! ai-bots, robots, links, hreflang, redirects, llms.txt) reproduce the reference
//! fixtures captured by the reference suite — every command against the
//! same 127.0.0.1 fixture server, on the CLI surface through the real binary.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::Value;

use spiderhead_cli::{run, Output};
use spiderhead_oracle_testkit::FixtureServer;

/// The oracle's normalisation, plus the wall-clock latency keys the cdn
/// transport probes measure per run.
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
                r#"("(?:crawl_delay_applied_at|delay_seconds|dom_content_loaded_ms|duration|"#,
                r#"duration_ms|elapsed_ms|elapsed_seconds|first_contentful_paint_ms|"#,
                r#"largest_contentful_paint_ms|latency_ms|load_ms|response_time|time_ms|"#,
                r#"total_ms|ttfb_ms|ttfb_first_ms|ttfb_second_ms)": )"#,
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
        self.duration_re
            .replace_all(&text, "${1}<DURATION>")
            .into_owned()
    }
}

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/checks")
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

/// site → the command fixtures captured against it (mirrors the generator).
const SITES: [(&str, &[&str]); 7] = [
    (
        "basic",
        &[
            "ai-bots-check",
            "cdn-check",
            "headers-check",
            "links-check-internal",
            "llms-txt-check",
            "regions-check",
            "security-check",
            "security-probe",
            "site-audit",
            "site-audit-report",
            "tech-detect",
        ],
    ),
    (
        "robots-sitemap",
        &[
            "ai-bots-check",
            "robots-check",
            "robots-check-badbot",
            "robots-check-paths",
            "site-audit-limit2",
        ],
    ),
    (
        "canonical-hreflang",
        &["hreflang-check", "hreflang-check-none"],
    ),
    (
        "redirects",
        &[
            "redirects-check-chain",
            "redirects-check-limit",
            "redirects-check-loop",
            "redirects-check-single",
            "redirects-check-to-404",
        ],
    ),
    ("errors", &["links-check"]),
    (
        "llms-txt",
        &[
            "llms-txt-check",
            "llms-txt-check-brand",
            "llms-txt-check-invalid",
            "robots-check",
        ],
    ),
    ("check-errors", &["llms-txt-check", "robots-check"]),
];

#[test]
fn recon_checks_match_the_reference_cli() {
    // The capture pins the same allow-list (the generator's ENV_BASE).
    std::env::set_var("SEOHEAD_ALLOW_PRIVATE_HOSTS", "127.0.0.1");
    let registry = spiderhead_cli::registry();
    let mut problems = String::new();
    for (site, commands) in SITES.iter() {
        let server = FixtureServer::start(site);
        let work =
            std::env::temp_dir().join(format!("sh-checks-cli-{site}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).expect("work dir");
        // Relative report paths (`audit-<host>.md`) land in the scratch dir, not the crate.
        std::env::set_current_dir(&work).expect("enter work dir");
        let mut norm = Norm::new(&server.base, &work);
        for name in *commands {
            let path = fixtures_root().join(site).join(format!("{name}.json"));
            let raw = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let fixture: Value = serde_json::from_str(&raw).expect("fixture json");
            let argv: Vec<String> = fixture["argv"]
                .as_array()
                .expect("argv")
                .iter()
                .map(|part| {
                    part.as_str()
                        .unwrap_or_default()
                        .replace("{BASE}", &server.base)
                })
                .collect();
            let mut out = Output::default();
            let code = run(&argv, &registry, &mut out).expect("handler run");
            if i64::from(code.0) != fixture["exit"].as_i64().unwrap_or_default() {
                problems += &format!(
                    "  {site}/{name} exit: expected {}, got {}\n",
                    fixture["exit"], code.0
                );
            }
            let actual_stdout = norm.s(&out.stdout);
            let expected_stdout = fixture["stdout"].as_str().unwrap_or_default();
            if actual_stdout != expected_stdout {
                problems += &format!("  {site}/{name} stdout:\n");
                problems += &unified_diff("stdout", expected_stdout, &actual_stdout);
            }
            let actual_stderr = norm.s(&out.stderr);
            let expected_stderr = fixture["stderr"].as_str().unwrap_or_default();
            if actual_stderr != expected_stderr {
                problems += &format!("  {site}/{name} stderr:\n");
                problems += &unified_diff("stderr", expected_stderr, &actual_stderr);
            }
        }
        let _ = std::fs::remove_dir_all(&work);
    }
    assert!(problems.is_empty(), "recon checks diverge:\n{problems}");
}

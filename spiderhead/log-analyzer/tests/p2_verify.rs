//! Multi-step bot verification: ranges, then reverse+forward DNS, then ASN; method kept;
//! failures isolated per vendor. Network is a fake resolver and 127.0.0.1 only.

mod common;

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use spiderhead_log_analyzer::dns::{DnsFailure, SystemClock};
use spiderhead_log_analyzer::import::{import_logs, ImportContext, ImportOptions};
use spiderhead_log_analyzer::presets::PresetSet;
use spiderhead_log_analyzer::ranges::{RangeBook, VerificationStatus};
use spiderhead_log_analyzer::verify::{
    bot_verification_page, AsnResolver, Engine, Live, Mode, PageQuery, Whois,
};

use common::{make_project, verifier, DirLoader, FakeResolver, QuickClock};

const GOOGLE: &str = "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)";
const BING: &str = "Mozilla/5.0 (compatible; bingbot/2.0; +http://www.bing.com/bingbot.htm)";
const GPT: &str = "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko); compatible; GPTBot/1.1";
const CLAUDE: &str = "Mozilla/5.0 (compatible; ClaudeBot/1.0; +claudebot@anthropic.com)";
const FB: &str = "facebookexternalhit/1.1 (+http://www.facebook.com/externalhit_uatext.php)";

struct Asn {
    calls: AtomicUsize,
    answer: Result<Vec<String>, String>,
}

impl AsnResolver for Asn {
    fn prefixes(&self, _asn: &str) -> Result<Vec<String>, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.answer.clone()
    }
}

fn artifact(hits: &[(&str, &str)]) -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("proj");
    make_project(&root, "https://example.test");
    let log: String = hits
        .iter()
        .enumerate()
        .map(|(i, (ip, ua))| {
            format!("{ip} - - [10/Oct/2026:10:00:{:02} +0000] \"GET /p{i} HTTP/1.1\" 200 1 \"-\" \"{ua}\"\n", i % 60)
        })
        .collect();
    let file = tmp.path().join("a.log");
    std::fs::write(&file, log).unwrap();
    let free = |_: &Path| Ok(u64::MAX);
    let now = || 1_790_000_000.0;
    let ctx = ImportContext {
        loader: &DirLoader,
        free_space: &free,
        now: &now,
    };
    let r = import_logs(
        &ctx,
        &file.to_string_lossy(),
        &root.to_string_lossy(),
        ImportOptions::default(),
    );
    let source = r["source"].as_str().unwrap().to_string();
    (tmp, source)
}

fn status_of(page: &Value, ip: &str) -> (String, Option<String>) {
    let row = page["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["ip"] == ip)
        .unwrap_or_else(|| panic!("no row for {ip}: {page}"));
    (
        row["status"].as_str().unwrap().to_string(),
        row["method"].as_str().map(str::to_string),
    )
}

fn resolver() -> FakeResolver {
    let mut r = FakeResolver::standard();
    r.ptr
        .insert("66.249.66.9".into(), "bot.evil.example".into());
    r
}

#[test]
fn dns_chain_reports_each_outcome_and_its_method() {
    let hits = [
        ("66.249.66.1", GOOGLE),
        ("66.249.66.2", GOOGLE),
        ("66.249.66.3", GOOGLE),
        ("203.0.113.5", GOOGLE),
        ("157.55.39.9", BING),
    ];
    let (_tmp, src) = artifact(&hits);
    let fake = Arc::new(resolver());
    let v = verifier(fake.clone());
    let book = RangeBook::new();
    let engine = Engine {
        presets: PresetSet::builtin(),
        book: &book,
        verifier: &v,
        asn: None,
        clock: &QuickClock,
    };
    let cached = bot_verification_page(&src, &engine, &PageQuery::default(), 1.0e9).unwrap();
    assert_eq!(cached["live"], false);
    assert!(fake.lookups().is_empty(), "cached mode never resolves");
    assert_eq!(status_of(&cached, "66.249.66.1").0, "unverified");

    let q = PageQuery {
        mode: Mode::Dns,
        ..PageQuery::default()
    };
    let now = SystemClock.wall_for_tests();
    let live = bot_verification_page(&src, &engine, &q, now).unwrap();
    assert_eq!(
        status_of(&live, "66.249.66.1"),
        ("verified".into(), Some("dns_reverse_forward".into()))
    );
    assert_eq!(
        status_of(&live, "66.249.66.2").0,
        "spoofed",
        "forward does not return the IP"
    );
    assert_eq!(
        status_of(&live, "66.249.66.3").0,
        "spoofed",
        "PTR outside the vendor domain"
    );
    assert_eq!(status_of(&live, "203.0.113.5").0, "ptr_failed");
    assert_eq!(status_of(&live, "157.55.39.9").0, "verified");

    let before = fake.lookups().len();
    let again = bot_verification_page(&src, &engine, &PageQuery::default(), now + 5.0).unwrap();
    assert_eq!(fake.lookups().len(), before, "cached evidence is reused");
    assert_eq!(
        status_of(&again, "66.249.66.1"),
        ("verified".into(), Some("dns_reverse_forward".into()))
    );
    assert_eq!(status_of(&again, "203.0.113.5").0, "ptr_failed");
}

trait WallForTests {
    fn wall_for_tests(&self) -> f64;
}
impl WallForTests for SystemClock {
    fn wall_for_tests(&self) -> f64 {
        use spiderhead_log_analyzer::dns::Clock;
        self.wall()
    }
}

#[test]
fn ranges_come_first_and_a_stale_list_does_not_condemn_a_real_crawler() {
    let hits = [
        ("20.171.206.9", GPT),
        ("8.8.4.4", GPT),
        ("66.249.66.1", GOOGLE),
        ("160.79.104.5", CLAUDE),
        ("9.9.9.9", CLAUDE),
    ];
    let (_tmp, src) = artifact(&hits);
    let fake = Arc::new(resolver());
    let v = verifier(fake.clone());
    let mut book = RangeBook::new();
    book.load_json("gptbot", r#"["20.171.206.0/24"]"#).unwrap();
    book.load_json("claudebot", r#"["160.79.104.0/23"]"#)
        .unwrap();
    book.load_json("google-common", r#"["192.0.2.0/24"]"#)
        .unwrap();
    let engine = Engine {
        presets: PresetSet::builtin(),
        book: &book,
        verifier: &v,
        asn: None,
        clock: &QuickClock,
    };
    let page = bot_verification_page(&src, &engine, &PageQuery::default(), 1.0e9).unwrap();
    assert_eq!(
        status_of(&page, "20.171.206.9"),
        ("verified".into(), Some("ip_list".into()))
    );
    assert_eq!(
        status_of(&page, "8.8.4.4").0,
        "spoofed",
        "list-only vendor, outside the list"
    );
    assert_eq!(
        status_of(&page, "160.79.104.5"),
        ("verified".into(), Some("ip_list".into()))
    );
    assert_eq!(
        status_of(&page, "9.9.9.9").0,
        "unverified",
        "Anthropic publishes no PTR domain, so nothing can confirm or refute"
    );
    assert_eq!(
        status_of(&page, "66.249.66.1").0,
        "unverified",
        "outside the Google list but DNS not run yet"
    );
    assert!(fake.lookups().is_empty());

    let q = PageQuery {
        mode: Mode::Dns,
        group: "googlebot".into(),
        ..PageQuery::default()
    };
    let live = bot_verification_page(&src, &engine, &q, SystemClock.wall_for_tests()).unwrap();
    assert_eq!(
        status_of(&live, "66.249.66.1"),
        ("verified".into(), Some("dns_reverse_forward".into())),
        "DNS gives a second chance after a stale list"
    );
    assert_eq!(live["total"], 1, "group filter");
}

#[test]
fn a_dead_vendor_is_given_up_alone() {
    let mut r = resolver();
    let google: Vec<String> = (10..16).map(|n| format!("66.249.66.{n}")).collect();
    for ip in &google {
        r.failures.insert(ip.clone(), DnsFailure::TryAgain);
    }
    let fake = Arc::new(r);
    let v = verifier(fake.clone());
    let book = RangeBook::new();
    let engine = Engine {
        presets: PresetSet::builtin(),
        book: &book,
        verifier: &v,
        asn: None,
        clock: &QuickClock,
    };
    let mut rows: Vec<(String, String, Option<_>)> = google
        .iter()
        .map(|ip| ("Googlebot".to_string(), ip.clone(), None))
        .collect();
    rows.push(("Bingbot".into(), "157.55.39.9".into(), None));
    let mut live = Live::new(Mode::Dns, 100);
    live.workers = 1;
    live.retries = 1;
    live.trip = 3;
    let out = engine.resolve_live(&rows, &live);
    assert!(out[..6]
        .iter()
        .all(|x| x.status == VerificationStatus::NetworkError));
    assert_eq!(
        out[6].status,
        VerificationStatus::Verified,
        "Bing is unaffected"
    );
    let google_calls = fake
        .lookups()
        .iter()
        .filter(|ip| ip.starts_with("66.249.66."))
        .count();
    assert_eq!(
        google_calls,
        3 * 2,
        "three failing rows x (1 try + 1 retry), then no more lookups"
    );
}

#[test]
fn asn_step_is_cached_and_failures_are_network_errors() {
    let book = RangeBook::new();
    let fake = Arc::new(resolver());
    let v = verifier(fake);
    let ok = Asn {
        calls: AtomicUsize::new(0),
        answer: Ok(vec!["31.13.64.0/18".into()]),
    };
    let engine = Engine {
        presets: PresetSet::builtin(),
        book: &book,
        verifier: &v,
        asn: Some(&ok),
        clock: &QuickClock,
    };
    let rows = vec![
        ("Facebook".to_string(), "31.13.65.1".to_string(), None),
        ("Facebook".to_string(), "8.8.8.8".to_string(), None),
    ];
    let mut live = Live::new(Mode::Full, 10);
    live.workers = 1;
    let out = engine.resolve_live(&rows, &live);
    assert_eq!(
        (out[0].status, out[0].method),
        (VerificationStatus::Verified, Some("asn"))
    );
    assert_eq!(out[1].status, VerificationStatus::Spoofed);
    assert_eq!(ok.calls.load(Ordering::SeqCst), 1, "one whois query per AS");

    let down = Asn {
        calls: AtomicUsize::new(0),
        answer: Err("timeout".into()),
    };
    let engine = Engine {
        asn: Some(&down),
        ..engine
    };
    let live = Live::new(Mode::Full, 10);
    let out = engine.resolve_live(&rows, &live);
    assert_eq!(out[0].status, VerificationStatus::NetworkError);
    assert!(
        down.calls.load(Ordering::SeqCst) <= 3,
        "retries are bounded"
    );
    let cached_only = engine.resolve("Facebook", "31.13.65.1", None, None);
    assert_eq!(
        cached_only.status,
        VerificationStatus::Unverified,
        "no ASN lookup without full mode"
    );
}

#[test]
fn whois_client_reads_routes_from_a_local_server() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut query = [0u8; 64];
        let n = sock.read(&mut query).unwrap();
        assert!(String::from_utf8_lossy(&query[..n]).starts_with("-i origin AS32934"));
        sock.write_all(b"route:  31.13.64.0/18\norigin: AS32934\nroute6: 2a03:2880::/32\n")
            .unwrap();
    });
    let whois = Whois {
        server: format!("127.0.0.1:{port}"),
        timeout: Duration::from_secs(5),
    };
    assert_eq!(
        whois.prefixes("AS32934").unwrap(),
        vec!["31.13.64.0/18", "2a03:2880::/32"]
    );
    server.join().unwrap();
    assert!(whois.prefixes("32934").is_err(), "AS prefix is required");
}

#[test]
fn status_and_virtual_group_filters_apply_to_bots_only() {
    let hits = [
        ("1.1.1.1", "Mozilla/5.0 Chrome/120.0 Safari/537.36"),
        ("20.171.206.9", GPT),
        ("66.249.66.1", GOOGLE),
        ("31.13.65.1", FB),
    ];
    let (_tmp, src) = artifact(&hits);
    let fake = Arc::new(resolver());
    let v = verifier(fake);
    let mut book = RangeBook::new();
    book.load_json("gptbot", r#"["20.171.206.0/24"]"#).unwrap();
    let engine = Engine {
        presets: PresetSet::builtin(),
        book: &book,
        verifier: &v,
        asn: None,
        clock: &QuickClock,
    };
    let all = bot_verification_page(&src, &engine, &PageQuery::default(), 1.0e9).unwrap();
    assert_eq!(all["total"], 3, "humans are never rows");
    let ai = bot_verification_page(
        &src,
        &engine,
        &PageQuery {
            group: "all_ai_bots".into(),
            ..PageQuery::default()
        },
        1.0e9,
    )
    .unwrap();
    assert_eq!(ai["total"], 1);
    assert_eq!(ai["rows"][0]["group"], "ai_bot");
    let verified = bot_verification_page(
        &src,
        &engine,
        &PageQuery {
            status: "verified".into(),
            ..PageQuery::default()
        },
        1.0e9,
    )
    .unwrap();
    assert_eq!(verified["total"], 1);
    assert_eq!(all["status_counts"]["verified"], 1);
    assert!(bot_verification_page(
        &src,
        &engine,
        &PageQuery {
            status: "bogus".into(),
            ..PageQuery::default()
        },
        1.0e9
    )
    .is_err());
}

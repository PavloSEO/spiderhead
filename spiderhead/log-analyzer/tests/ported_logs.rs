//! Behaviour tests for parsing, bot detection, analysis and reverse DNS.

mod common;

use std::io::Cursor;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use spiderhead_log_analyzer::analyze::{analyze_lines, analyze_log, findings, AnalyzeOptions};
use spiderhead_log_analyzer::bots::{detect_bot, GOOGLE_CRAWLERS, GOOGLE_FETCHERS, GOOGLE_SPECIAL};
use spiderhead_log_analyzer::clock::iso_utc;
use spiderhead_log_analyzer::dns::{DnsFailure, Gate, Resolver, Verifier};
use spiderhead_log_analyzer::format::{
    detect_format, parse_apache, parse_apache_timestamp, section,
};
use spiderhead_log_analyzer::reader::Lines;

use common::{verifier, FakeResolver, QuickClock};

const COMBINED: &str = concat!(
    "66.249.66.1 - - [18/Mar/2024:00:02:09 +0000] \"GET /catalog/pumps HTTP/1.1\" 200 5120 \"-\" \"Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)\"\n",
    "66.249.66.1 - - [18/Mar/2024:00:03:10 +0000] \"GET /old-page HTTP/1.1\" 404 0 \"-\" \"Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)\"\n",
    "20.15.1.2 - - [18/Mar/2024:01:00:00 +0000] \"GET /blog/post HTTP/1.1\" 200 8000 \"-\" \"Mozilla/5.0 AppleWebKit (KHTML, like Gecko) Chrome/120 Safari/537.36\"\n",
    "52.70.1.1 - - [19/Mar/2024:02:00:00 +0000] \"GET /pricing HTTP/1.1\" 200 3000 \"-\" \"Mozilla/5.0 (compatible; GPTBot/1.0; +https://openai.com/gptbot)\"\n",
    "1.2.3.4 - - [19/Mar/2024:03:00:00 +0000] \"GET /catalog/x HTTP/1.1\" 503 0 \"-\" \"Mozilla/5.0 (compatible; AhrefsBot/7.0; +http://ahrefs.com/robot/)\"\n",
);
const COMMON: &str =
    "24.5.66.10 - - [18/Mar/2024:00:02:09 +0000] \"GET /old-pricing HTTP/1.1\" 301 0\n";
const IIS: &str = concat!(
    "#Software: Microsoft Internet Information Services 10.0\n",
    "#Fields: date time cs-uri-stem cs-uri-query cs-method c-ip cs(User-Agent) sc-status sc-bytes\n",
    "2024-03-18 00:02:09 /catalog/pumps - GET 66.249.66.1 Mozilla/5.0+(compatible;+Googlebot/2.1) 200 5120\n",
);
const NGINX_MAIN: &str = "203.0.113.7 - - [18/Mar/2024:00:02:09 +0000] \"GET /catalog/pumps HTTP/1.1\" 200 5120 \"-\" \"Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)\" \"-\"";
const NGINX_NO_USER: &str = "203.0.113.7 - [18/Mar/2024:00:02:09 +0000] \"GET /catalog/pumps HTTP/1.1\" 200 5120 \"https://referrer.example.test/\" \"Mozilla/5.0 AppleWebKit (KHTML, like Gecko)\"";

fn write(dir: &tempfile::TempDir, text: &str) -> String {
    let path = dir.path().join("access.log");
    std::fs::write(&path, text).unwrap();
    path.to_string_lossy().into_owned()
}

fn analyze(text: &str, o: AnalyzeOptions) -> Value {
    let dir = tempfile::tempdir().unwrap();
    let v = verifier(Arc::new(FakeResolver::standard()));
    analyze_log(&write(&dir, text), &o, &v).unwrap()
}

fn lines(n: &[(&str, u32)]) -> String {
    n.iter()
        .map(|(ip, s)| format!("{ip} - - [18/Mar/2024:00:00:{s:02} +0000] \"GET / HTTP/1.1\" 200 1 \"-\" \"Googlebot/2.1\"\n"))
        .collect()
}

#[test]
fn specific_signature_wins_over_generic() {
    assert_eq!(
        detect_bot(Some("Googlebot-Image/1.0")).unwrap().name,
        "Googlebot Image"
    );
    assert_eq!(
        detect_bot(Some("Mozilla/5.0 (compatible; Googlebot/2.1)"))
            .unwrap()
            .name,
        "Googlebot"
    );
}

#[test]
fn human_is_not_a_bot() {
    assert!(detect_bot(Some("Mozilla/5.0 (Macintosh) Chrome/120 Safari/537.36")).is_none());
    assert!(detect_bot(None).is_none());
    assert!(detect_bot(Some("-")).is_none());
}

#[test]
fn ai_crawlers_are_not_verifiable() {
    for (ua, name) in [
        ("GPTBot/1.0", "GPTBot (OpenAI)"),
        ("ClaudeBot/1.0", "ClaudeBot (Anthropic)"),
        ("PerplexityBot/1.0", "PerplexityBot"),
    ] {
        let bot = detect_bot(Some(ua)).unwrap();
        assert_eq!(
            (bot.name, bot.family, bot.verifiable()),
            (name, "ai", false)
        );
    }
}

#[test]
fn search_engines_are_verifiable() {
    for ua in ["Googlebot/2.1", "bingbot/2.0", "YandexBot/3.0"] {
        assert!(detect_bot(Some(ua)).unwrap().verifiable());
    }
}

#[test]
fn timezone_is_applied_in_the_right_direction() {
    assert!(
        iso_utc(parse_apache_timestamp("10/Oct/2000:13:55:36 -0700").unwrap())
            .contains("T20:55:36")
    );
    assert!(
        iso_utc(parse_apache_timestamp("10/Oct/2000:13:55:36 +0300").unwrap())
            .contains("T10:55:36")
    );
}

#[test]
fn broken_timestamp_is_none() {
    assert!(parse_apache_timestamp("not a date").is_none());
    assert!(parse_apache_timestamp("32/Xxx/2024:00:00:00 +0000").is_none());
}

#[test]
fn format_detection() {
    let det = |t: &str| detect_format(t.lines()).map(|f| f.name());
    assert_eq!(det(COMBINED), Some("combined"));
    assert_eq!(det(COMMON), Some("common"));
    assert_eq!(det(IIS), Some("iis"));
    assert_eq!(det("garbage\nmore garbage"), None);
}

#[test]
fn nginx_main_with_forwarded_for() {
    assert_eq!(
        detect_format([NGINX_MAIN]).map(|f| f.name()),
        Some("combined")
    );
    assert_eq!(
        detect_format([NGINX_NO_USER]).map(|f| f.name()),
        Some("combined")
    );
    let row = parse_apache(NGINX_MAIN).unwrap().unwrap();
    assert_eq!(
        (row.ip.as_str(), row.path.as_str(), row.status, row.bytes),
        ("203.0.113.7", "/catalog/pumps", 200, 5120)
    );
    assert_eq!(
        detect_bot(row.user_agent.as_deref()).unwrap().name,
        "Googlebot"
    );
    let row = parse_apache(NGINX_NO_USER).unwrap().unwrap();
    assert_eq!(
        row.referer.as_deref(),
        Some("https://referrer.example.test/")
    );
    assert_eq!(
        row.user_agent.as_deref(),
        Some("Mozilla/5.0 AppleWebKit (KHTML, like Gecko)")
    );
}

#[test]
fn request_time_after_forwarded_for() {
    assert_eq!(
        parse_apache(&format!("{NGINX_MAIN} 0.123"))
            .unwrap()
            .unwrap()
            .time_taken,
        Some(0.123)
    );
}

#[test]
fn sections_are_first_path_segment() {
    assert_eq!(section("/catalog/pumps/cdm"), "/catalog");
    assert_eq!(section("/"), "/");
    assert_eq!(section("/pricing?utm=1"), "/pricing");
}

#[test]
fn combined_log_is_parsed_and_grouped() {
    let r = analyze(COMBINED, AnalyzeOptions::default());
    assert_eq!(
        (r["ok"].clone(), r["format"].clone()),
        (json!(true), json!("combined"))
    );
    assert_eq!(
        (r["lines"]["parsed"].clone(), r["lines"]["skipped"].clone()),
        (json!(5), json!(0))
    );
    for family in ["googlebot", "ai", "human", "seo-tool"] {
        assert!(r["by_family"].get(family).is_some(), "{family}");
    }
    assert!(r["sections_by_family"]["googlebot"]
        .get("/catalog")
        .is_some());
    assert_eq!(r["status_by_family"]["googlebot"]["404"], json!(1));
}

#[test]
fn iis_reads_field_positions_from_directive() {
    let r = analyze(IIS, AnalyzeOptions::default());
    assert_eq!(
        (r["format"].clone(), r["lines"]["parsed"].clone()),
        (json!("iis"), json!(1))
    );
    assert!(r["by_family"].get("googlebot").is_some());
}

#[test]
fn verification_is_off_by_default() {
    assert_eq!(
        analyze(COMBINED, AnalyzeOptions::default())["verification"]["checked"],
        json!(false)
    );
}

#[test]
fn missing_file_is_data() {
    let v = verifier(Arc::new(FakeResolver::default()));
    let r = analyze_log("/nope/does-not-exist.log", &AnalyzeOptions::default(), &v).unwrap();
    assert_eq!(r["ok"], json!(false));
    assert!(r["error"]
        .as_str()
        .unwrap()
        .starts_with("[Errno 2] No such file or directory"));
}

#[test]
fn unparsable_file_says_so() {
    let r = analyze(
        "definitely not a log\nsecond line\n",
        AnalyzeOptions::default(),
    );
    assert_eq!(r["ok"], json!(false));
    let e = r["error"].as_str().unwrap();
    assert!(e.contains("Apache Common/Combined") && e.contains("IIS W3C"));
}

#[test]
fn findings_flag_error_rate_for_bots() {
    let r = analyze(COMBINED, AnalyzeOptions::default());
    assert!(r["findings"].as_array().unwrap().iter().any(|f| f
        .as_str()
        .unwrap()
        .starts_with("seo-tool:")
        && f.as_str().unwrap().contains("(100%)")));
}

#[test]
fn findings_mention_ai_crawlers() {
    let r = analyze(COMBINED, AnalyzeOptions::default());
    assert!(r["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f.as_str().unwrap().contains("GPTBot (OpenAI)")));
}

#[test]
fn top_paths_keep_incrementing_after_the_cap_fills() {
    let row = |p: &str, s: u32| {
        format!("192.0.2.1 - - [18/Mar/2024:00:00:{s:02} +0000] \"GET {p} HTTP/1.1\" 200 1 \"-\" \"Googlebot/2.1\"\n")
    };
    let text = [
        row("/first", 1),
        row("/second", 2),
        row("/third", 3),
        row("/first", 4),
    ]
    .concat();
    let r = analyze(
        &text,
        AnalyzeOptions {
            max_tracked_paths: 2,
            ..AnalyzeOptions::default()
        },
    );
    assert_eq!(r["by_family"]["googlebot"]["Googlebot"], json!(4));
    assert_eq!(r["top_paths_by_family"]["googlebot"]["/first"], json!(2));
    assert_eq!(r["paths_truncated"], json!(["googlebot"]));
}

#[test]
fn unique_ips_are_exact_beyond_the_old_cap() {
    let text: String = (0..2100)
        .map(|i| format!("192.0.{}.{} - - [18/Mar/2024:00:00:{:02} +0000] \"GET / HTTP/1.1\" 200 1 \"-\" \"Googlebot/2.1\"\n", i / 256, i % 256, i % 60))
        .collect();
    let r = analyze(
        &text,
        AnalyzeOptions {
            offset: 2098,
            limit: 2,
            ..AnalyzeOptions::default()
        },
    );
    assert_eq!(r["bots"][0]["unique_ips"], json!(2100));
    assert_eq!(r["unique_bot_ips"], json!(2100));
    assert_eq!(r["bot_ips"]["total"], json!(2100));
    assert_eq!(r["bot_ips"]["rows"].as_array().unwrap().len(), 2);
    assert_eq!(
        (r["ips_truncated"].clone(), r["bot_ips"]["has_more"].clone()),
        (json!([]), json!(false))
    );
}

#[test]
fn ip_page_counts_repeats_last_visit_and_subnet() {
    let text = lines(&[("192.0.2.1", 1), ("192.0.2.2", 2), ("192.0.2.1", 3)]);
    let r = analyze(
        &text,
        AnalyzeOptions {
            ip: Some("192.0.2.1/32".into()),
            ..AnalyzeOptions::default()
        },
    );
    assert_eq!(r["bot_ips"]["total"], json!(1));
    let row = &r["bot_ips"]["rows"][0];
    assert_eq!(row["visits"], json!(2));
    assert!(row["last_visit"]
        .as_str()
        .unwrap()
        .ends_with("00:00:03+00:00"));
    assert_eq!(row["verdict"], json!("unchecked"));
}

#[test]
fn per_ip_verification_does_not_extrapolate_family() {
    let text = lines(&[("192.0.2.1", 1), ("192.0.2.2", 2)]);
    let mut fake = FakeResolver::standard();
    fake.forward
        .insert("crawl.googlebot.com".into(), vec!["192.0.2.1".into()]);
    let resolver = Arc::new(fake);
    let v = verifier(Arc::clone(&resolver));
    let dir = tempfile::tempdir().unwrap();
    let path = write(&dir, &text);
    let verdicts = |r: &Value| {
        r["bot_ips"]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["verdict"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    let o = |max_dns| AnalyzeOptions {
        verify_bots: true,
        max_dns,
        ..AnalyzeOptions::default()
    };
    let r = analyze_log(&path, &o(1), &v).unwrap();
    assert_eq!(resolver.lookups(), ["8.8.8.8", "192.0.2.1"]);
    assert_eq!(verdicts(&r), ["verified", "unchecked"]);
    let r = analyze_log(&path, &o(2), &v).unwrap();
    assert_eq!(
        resolver.lookups(),
        ["8.8.8.8", "192.0.2.1", "8.8.8.8", "192.0.2.2"]
    );
    assert_eq!(verdicts(&r), ["verified", "fake"]);
    assert_eq!(r["bot_ips"]["rows"][0]["cached"], json!(true));
}

#[test]
fn log_analysis_validates_bounds() {
    let v = verifier(Arc::new(FakeResolver::default()));
    let base = AnalyzeOptions::default;
    for o in [
        AnalyzeOptions { limit: 0, ..base() },
        AnalyzeOptions {
            max_lines: 0,
            ..base()
        },
        AnalyzeOptions {
            offset: -1,
            ..base()
        },
        AnalyzeOptions {
            max_dns: 101,
            ..base()
        },
        AnalyzeOptions {
            ip: Some("not-an-ip".into()),
            ..base()
        },
    ] {
        assert!(analyze_log("synthetic.log", &o, &v).is_err());
    }
}

struct Blocking {
    release: Arc<(Mutex<bool>, Condvar)>,
}

impl Resolver for Blocking {
    fn reverse(&self, _ip: std::net::IpAddr) -> Result<String, DnsFailure> {
        let (lock, cv) = &*self.release;
        let mut open = lock.lock().unwrap();
        while !*open {
            let (guard, timeout) = cv.wait_timeout(open, Duration::from_secs(1)).unwrap();
            open = guard;
            if timeout.timed_out() {
                break;
            }
        }
        Ok("outside.example.test".into())
    }
    fn forward(&self, _h: &str, _v6: bool) -> Result<Vec<String>, DnsFailure> {
        Ok(vec![])
    }
}

fn release(flag: &Arc<(Mutex<bool>, Condvar)>) {
    *flag.0.lock().unwrap() = true;
    flag.1.notify_all();
}

#[test]
fn dns_deadline_reports_unavailable() {
    let flag = Arc::new((Mutex::new(false), Condvar::new()));
    let v = Verifier::new(
        Arc::new(Blocking {
            release: Arc::clone(&flag),
        }),
        Arc::new(QuickClock),
    );
    let r = v.verify_with_timeout("192.0.2.99", GOOGLE_CRAWLERS, Duration::from_millis(10));
    assert_eq!(r["verified"], Value::Null);
    assert!(r["reason"].as_str().unwrap().contains("deadline"));
    release(&flag);
}

#[test]
fn stalled_dns_workers_are_bounded_across_calls() {
    let flag = Arc::new((Mutex::new(false), Condvar::new()));
    let v = Verifier::with_limits(
        Arc::new(Blocking {
            release: Arc::clone(&flag),
        }),
        Arc::new(QuickClock),
        Gate::new(1),
        Duration::from_millis(10),
    );
    let first = v.verify_bot_rdns("192.0.2.1", GOOGLE_CRAWLERS);
    assert!(first["reason"].as_str().unwrap().contains("deadline"));
    let second = v.verify_bot_rdns("192.0.2.2", GOOGLE_CRAWLERS);
    assert_eq!(second["verified"], Value::Null);
    assert!(second["reason"].as_str().unwrap().contains("capacity"));
    release(&flag);
}

#[test]
fn ipv6_forward_confirmation_and_suffix_boundary() {
    let mut r = FakeResolver::default();
    r.ptr
        .insert("2001:db8::1".into(), "crawl.googlebot.com.".into());
    r.forward
        .insert("crawl.googlebot.com".into(), vec!["2001:db8::1".into()]);
    assert_eq!(
        verifier(Arc::new(r)).verify_bot_rdns("2001:db8::1", GOOGLE_CRAWLERS)["forward_match"],
        json!(true)
    );
    let mut r = FakeResolver::default();
    r.ptr
        .insert("2001:db8::1".into(), "evilgooglebot.com".into());
    assert_eq!(
        verifier(Arc::new(r)).verify_bot_rdns("2001:db8::1", GOOGLE_CRAWLERS)["verified"],
        json!(false)
    );
}

#[test]
fn max_lines_does_not_consume_the_rest_of_the_file() {
    let line = "192.0.2.1 - - [18/Mar/2024:00:00:01 +0000] \"GET / HTTP/1.1\" 200 1 \"-\" \"Googlebot/2.1\"\n";
    let pulled = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&pulled);
    let source = (0..25).map(move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(line.to_string())
    });
    let o = AnalyzeOptions {
        max_lines: 1,
        sample_size: 1,
        ..AnalyzeOptions::default()
    };
    let v = verifier(Arc::new(FakeResolver::default()));
    let r = analyze_lines(source, "synthetic", &o, None, &v).unwrap();
    // One sample line plus the single line pulled to learn that the cap was hit.
    assert_eq!(pulled.load(Ordering::SeqCst), 2);
    assert_eq!(
        r["lines"],
        json!({"total": 1, "parsed": 1, "skipped": 0, "truncated": true})
    );
}

#[test]
fn forward_lookup_unavailable_is_not_a_mismatch() {
    let mut r = FakeResolver::default();
    r.ptr.insert(
        "66.249.66.1".into(),
        "crawl-66-249-66-1.googlebot.com".into(),
    );
    r.break_forward = true;
    let result = verifier(Arc::new(r)).verify_bot_rdns("66.249.66.1", GOOGLE_CRAWLERS);
    assert_eq!(result["verified"], Value::Null);
    let reason = result["reason"].as_str().unwrap().to_lowercase();
    assert!(reason.contains("forward") && reason.contains("unavailable"));
    let report = json!({
        "lines": {"parsed": 1, "skipped": 0, "total": 1},
        "by_family": {"googlebot": {"66.249.66.1": 1}},
        "status_by_family": {},
        "bot_detection": {"unknown_user_agent": 0},
        "verification": {"checked": true, "dns_available": true, "checks": [{"bot": "googlebot", "ip": "66.249.66.1", "verified": null}]},
    });
    assert!(!findings(&report)
        .iter()
        .any(|f| f.contains("impersonating")));
}

#[test]
fn real_mismatch_still_flags() {
    let mut r = FakeResolver::default();
    r.ptr
        .insert("1.2.3.4".into(), "crawl-1-2-3-4.googlebot.com".into());
    r.forward
        .insert("crawl-1-2-3-4.googlebot.com".into(), vec!["9.9.9.9".into()]);
    let result = verifier(Arc::new(r)).verify_bot_rdns("1.2.3.4", GOOGLE_CRAWLERS);
    assert_eq!(result["verified"], json!(false));
    assert!(result["reason"]
        .as_str()
        .unwrap()
        .contains("does not resolve back to"));
}

#[test]
fn google_bots_verify_against_their_own_family_suffix() {
    for (ua, suffixes) in [
        ("Googlebot/2.1", GOOGLE_CRAWLERS),
        ("Googlebot-Image/1.0", GOOGLE_CRAWLERS),
        ("GoogleOther", GOOGLE_CRAWLERS),
        ("Google-Extended", GOOGLE_CRAWLERS),
        ("AdsBot-Google", GOOGLE_SPECIAL),
        ("Mediapartners-Google", GOOGLE_SPECIAL),
        ("APIs-Google", GOOGLE_SPECIAL),
        ("Google-InspectionTool", GOOGLE_SPECIAL),
        ("FeedFetcher-Google", GOOGLE_FETCHERS),
    ] {
        assert_eq!(
            detect_bot(Some(ua)).unwrap().rdns_suffixes,
            suffixes,
            "{ua}"
        );
    }
}

#[test]
fn each_google_family_suffix_verifies_only_within_its_family() {
    for (table, host) in [
        (GOOGLE_CRAWLERS, "crawl-66-249-66-1.googlebot.com"),
        (GOOGLE_SPECIAL, "rate-limited-proxy-66-249-66-1.google.com"),
        (GOOGLE_FETCHERS, "66-249-66-1.gae.googleusercontent.com"),
    ] {
        let mut r = FakeResolver::default();
        r.ptr.insert("66.249.66.1".into(), host.into());
        r.forward.insert(host.into(), vec!["66.249.66.1".into()]);
        assert_eq!(
            verifier(Arc::new(r)).verify_bot_rdns("66.249.66.1", table)["verified"],
            json!(true),
            "{host}"
        );
    }
    let mut r = FakeResolver::default();
    r.ptr.insert(
        "66.249.66.1".into(),
        "rate-limited-proxy-66-249-66-1.google.com".into(),
    );
    assert_eq!(
        verifier(Arc::new(r)).verify_bot_rdns("66.249.66.1", GOOGLE_CRAWLERS)["verified"],
        json!(false)
    );
}

#[test]
fn gcp_vm_ptr_is_fake() {
    let mut r = FakeResolver::default();
    r.ptr.insert(
        "34.178.163.194".into(),
        "194.163.178.34.bc.googleusercontent.com".into(),
    );
    let result = verifier(Arc::new(r)).verify_bot_rdns("34.178.163.194", GOOGLE_CRAWLERS);
    assert_eq!(result["verified"], json!(false));
    assert_eq!(
        result["hostname"],
        json!("194.163.178.34.bc.googleusercontent.com")
    );
    assert!(result["reason"]
        .as_str()
        .unwrap()
        .contains("Google Cloud VM"));
}

#[test]
fn gzip_log_is_read_by_content_not_extension() {
    let v = verifier(Arc::new(FakeResolver::default()));
    for name in ["combined.log.1.gz", "renamed.archive"] {
        let r = analyze_log(&common::log_path(name), &AnalyzeOptions::default(), &v).unwrap();
        assert_eq!(r["ok"], json!(true), "{name}");
    }
}

#[test]
fn truncated_gzip_is_an_error() {
    let v = verifier(Arc::new(FakeResolver::default()));
    let r = analyze_log(
        &common::log_path("truncated.gz"),
        &AnalyzeOptions::default(),
        &v,
    )
    .unwrap();
    assert_eq!(r["ok"], json!(false));
    assert!(r["error"].as_str().unwrap().contains("Cannot read log"));
}

#[test]
fn common_log_makes_no_absent_bot_claims() {
    let r = analyze(COMMON, AnalyzeOptions::default());
    assert_eq!(r["bot_detection"]["available"], json!(false));
    let notes: Vec<&str> = r["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(notes.iter().any(|m| m.contains("unavailable")));
    assert!(!notes
        .iter()
        .any(|m| m.contains("Googlebot does not appear")));
}

#[test]
fn read_failures_in_the_stream_are_result_data() {
    let broken = vec![
        Ok(COMBINED.lines().next().unwrap().to_string()),
        Err(spiderhead_log_analyzer::LogsError::Io(
            "synthetic disk unavailable".into(),
        )),
    ];
    let v = verifier(Arc::new(FakeResolver::default()));
    let r = analyze_lines(
        broken.into_iter(),
        "x",
        &AnalyzeOptions::default(),
        None,
        &v,
    )
    .unwrap();
    assert_eq!(r["ok"], json!(false));
    assert!(r["error"]
        .as_str()
        .unwrap()
        .contains("synthetic disk unavailable"));
}

#[test]
fn time_between_size_and_referer_keeps_agent_and_referer() {
    let line = "66.249.66.1 - - [18/Mar/2024:00:02:09 +0000] \"GET /catalog/pumps HTTP/1.1\" 200 5120 0.123 \"https://example.test/pricing\" \"Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)\"";
    let row = parse_apache(line).unwrap().unwrap();
    assert!(row
        .user_agent
        .unwrap()
        .starts_with("Mozilla/5.0 (compatible; Googlebot"));
    assert_eq!(
        (row.referer.as_deref(), row.bytes, row.time_taken),
        (Some("https://example.test/pricing"), 5120, Some(0.123))
    );
}

#[test]
fn time_after_user_agent_and_no_time_layouts_both_parse() {
    let trailing = parse_apache("66.249.66.1 - - [18/Mar/2024:00:02:09 +0000] \"GET /a HTTP/1.1\" 200 5120 \"-\" \"Googlebot/2.1\" 0.5").unwrap().unwrap();
    assert_eq!(
        (trailing.user_agent.as_deref(), trailing.time_taken),
        (Some("Googlebot/2.1"), Some(0.5))
    );
    let classic = parse_apache("66.249.66.1 - - [18/Mar/2024:00:02:09 +0000] \"GET /a HTTP/1.1\" 200 5120 \"-\" \"Googlebot/2.1\"").unwrap().unwrap();
    assert_eq!(classic.time_taken, None);
    let common =
        parse_apache("24.5.66.10 - - [18/Mar/2024:00:02:09 +0000] \"GET /b HTTP/1.1\" 301 0 0.9")
            .unwrap()
            .unwrap();
    assert_eq!((common.status, common.time_taken), (301, Some(0.9)));
}

#[test]
fn single_ident_token_is_accepted() {
    let line = "66.249.66.1 - [18/Mar/2024:00:02:09 +0000] \"GET /catalog/pumps HTTP/1.1\" 200 5120 \"-\" \"Googlebot/2.1\"";
    assert_eq!(
        parse_apache(line).unwrap().unwrap().user_agent.as_deref(),
        Some("Googlebot/2.1")
    );
    let common = "24.5.66.10 - [18/Mar/2024:00:02:09 +0000] \"GET /old HTTP/1.1\" 301 0";
    assert_eq!(parse_apache(common).unwrap().unwrap().status, 301);
    assert_eq!(detect_format([line]).map(|f| f.name()), Some("combined"));
    assert_eq!(detect_format([common]).map(|f| f.name()), Some("common"));
}

#[test]
fn universal_newlines_reach_the_analysis() {
    let text = COMBINED.replace('\n', "\r\n");
    let r = analyze(&text, AnalyzeOptions::default());
    assert_eq!(r["lines"]["parsed"], json!(5));
    let lines: Vec<_> = Lines::new(Box::new(Cursor::new(b"a\r\nb".to_vec()))).collect();
    assert_eq!(lines.len(), 2);
}

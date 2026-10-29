//! Whole-store bot verification: one call resolves every bot–IP pair, verdicts persist
//! in the sidecar cache, an interrupted run resumes, a dead resolver is reported as
//! `resolver_unavailable`, and every published Google range list authenticates IPs.
//! Network is a fake resolver and 127.0.0.1 only.

mod common;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use spiderhead_log_analyzer::dns::Clock;
use spiderhead_log_analyzer::import::{import_logs, ImportContext, ImportOptions};
use spiderhead_log_analyzer::presets::PresetSet;
use spiderhead_log_analyzer::rangecache::RangeCache;
use spiderhead_log_analyzer::ranges::{RangeBook, RangeFetcher};
use spiderhead_log_analyzer::verify::{bot_verification_all, AllQuery, Engine, Mode};

use common::{make_project, verifier, DirLoader, FakeResolver};

const GOOGLE: &str = "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)";
const ADSBOT: &str = "Mozilla/5.0 (compatible; AdsBot-Google/1.0)";
const FEED: &str = "FeedFetcher-Google; (+http://www.google.com/feedfetcher.html)";
const GPT: &str = "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko); compatible; GPTBot/1.1";
const YANDEX: &str = "Mozilla/5.0 (compatible; YandexBot/3.0)";

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

fn engine<'a>(book: &'a RangeBook, v: &'a spiderhead_log_analyzer::dns::Verifier) -> Engine<'a> {
    Engine {
        presets: PresetSet::builtin(),
        book,
        verifier: v,
        asn: None,
        clock: &common::QuickClock,
    }
}

fn google_book() -> RangeBook {
    let mut book = RangeBook::new();
    let dir = common::fixtures().join("google_lists");
    book.load_dir(&dir).unwrap();
    book
}

/// Progress recorder.
#[derive(Default)]
struct Progress(Mutex<Vec<(usize, usize)>>);
impl Progress {
    fn callback(&self) -> impl Fn(usize, usize) + '_ {
        |done, total| self.0.lock().unwrap().push((done, total))
    }
    fn calls(&self) -> Vec<(usize, usize)> {
        self.0.lock().unwrap().clone()
    }
}

#[test]
fn one_call_verifies_every_pair_and_the_second_run_touches_no_network() {
    let hits = [
        ("66.249.66.1", GOOGLE), // real Google rDNS
        ("66.249.66.3", GOOGLE), // PTR outside the vendor domain
        ("203.0.113.5", GOOGLE), // no PTR
        ("20.171.206.9", GPT),   // inside the GPTBot list
        (
            "157.55.39.9",
            "Mozilla/5.0 (compatible; bingbot/2.0; +http://www.bing.com/bingbot.htm)",
        ),
        ("5.255.253.1", YANDEX),
    ];
    let (_tmp, src) = artifact(&hits);
    let fake = Arc::new(FakeResolver::standard());
    let v = verifier(fake.clone());
    let mut book = RangeBook::new();
    book.load_json("gptbot", r#"["20.171.206.0/24"]"#).unwrap();
    let e = engine(&book, &v);
    let progress = Progress::default();
    let q = AllQuery {
        mode: Mode::Dns,
        ..AllQuery::default()
    };
    let out = bot_verification_all(&src, &e, &q, 1.0e9, &progress.callback()).unwrap();
    assert_eq!(out["ok"], true);
    assert_eq!(out["total"], 6);
    assert_eq!(out["done"], 6);
    // google .1 verified, .3 spoofed, 203 ptr_failed, gpt verified,
    // bing verified, yandex verified -> verified 4, spoofed 1, ptr_failed 1
    let counts = &out["status_counts"];
    assert_eq!(counts["verified"], 4);
    assert_eq!(counts["spoofed"], 1);
    assert_eq!(counts["ptr_failed"], 1);
    assert_eq!(out["method_counts"]["ip_list"], 1);
    assert_eq!(out["method_counts"]["dns_reverse_forward"], 5);
    assert_eq!(out["verdict_cache"]["stored"], 6);
    assert!(progress.calls().last().unwrap().0 == 6);

    // A second run: every verdict comes back from the sidecar cache.
    let before = fake.lookups().len();
    let again = bot_verification_all(&src, &e, &q, 1.0e9 + 60.0, &|_, _| {}).unwrap();
    assert_eq!(fake.lookups().len(), before, "no lookup on a warm cache");
    assert_eq!(again["verdict_cache"]["hits"], 6);
    assert_eq!(again["status_counts"], *counts);
    assert_eq!(again["live"]["dns_lookups"], 0);
}

#[test]
fn a_dead_resolver_is_reported_not_faked() {
    let hits = [
        ("5.255.253.1", YANDEX), // DNS-only bot
        ("66.249.66.1", GOOGLE), // list + DNS bot, no list loaded here
        ("20.171.206.9", GPT),   // list-only bot with its list loaded
    ];
    let (_tmp, src) = artifact(&hits);
    let mut dead = FakeResolver::standard();
    dead.blocked = true;
    let fake = Arc::new(dead);
    let v = verifier(fake.clone());
    let mut book = RangeBook::new();
    book.load_json("gptbot", r#"["20.171.206.0/24"]"#).unwrap();
    let e = engine(&book, &v);
    let q = AllQuery {
        mode: Mode::Full,
        ..AllQuery::default()
    };
    let out = bot_verification_all(&src, &e, &q, 1.0e9, &|_, _| {}).unwrap();
    let counts = &out["status_counts"];
    assert_eq!(
        counts["resolver_unavailable"], 2,
        "Yandex and Google without a list"
    );
    assert_eq!(counts["verified"], 1, "the loaded GPTBot list still judges");
    assert_eq!(counts["spoofed"], 0, "a dead resolver never fakes a bot");
    assert_eq!(out["live"]["resolver_available"], false);
    // The control lookup runs once per run (a worker race may add a couple).
    let canary = fake.lookups().iter().filter(|ip| *ip == "8.8.8.8").count();
    assert!(
        (1..=8).contains(&canary),
        "the canary is a single control lookup, got {canary}"
    );
    // Non-verdicts are never written to the cache.
    assert_eq!(out["verdict_cache"]["stored"], 1);
}

#[test]
fn resume_after_an_interrupted_run_only_redoes_what_is_left() {
    // Addresses with no PTR record: every lookup ends in a clean, decisive
    // `ptr_failed`, so the verdict split cannot drift with worker timing.
    let hits: Vec<(String, &str)> = (0..20u8)
        .map(|n| (format!("66.249.70.{}", n + 30), GOOGLE))
        .collect();
    let hits: Vec<(&str, &str)> = hits.iter().map(|(ip, ua)| (ip.as_str(), *ua)).collect();
    let (_tmp, src) = artifact(&hits);
    let fake = Arc::new(FakeResolver::standard());
    let v = verifier(fake.clone());
    let book = RangeBook::new();
    let e = engine(&book, &v);
    // First run: the lookup budget covers only part of the store.
    let first = bot_verification_all(
        &src,
        &e,
        &AllQuery {
            mode: Mode::Dns,
            max_dns: 5,
            ..AllQuery::default()
        },
        1.0e9,
        &|_, _| {},
    )
    .unwrap();
    assert_eq!(first["live"]["dns_lookups"], 5, "{first}");
    // Resume: the budget is back, but only the undecided rows look up.
    let second = bot_verification_all(
        &src,
        &e,
        &AllQuery {
            mode: Mode::Dns,
            ..AllQuery::default()
        },
        1.0e9 + 60.0,
        &|_, _| {},
    )
    .unwrap();
    assert_eq!(second["done"], 20);
    assert!(
        second["verdict_cache"]["hits"].as_i64().unwrap() >= 5,
        "{second}"
    );
    let left = second["live"]["dns_lookups"].as_i64().unwrap();
    assert_eq!(
        left, 15,
        "exactly the rows without a verdict are redone: {second}"
    );
}

struct Flaky {
    calls: AtomicUsize,
    fail: usize,
}

impl RangeFetcher for Flaky {
    fn fetch(&self, _provider: &str) -> Result<String, String> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n < self.fail {
            Err(format!("attempt {n} failed"))
        } else {
            Ok(format!(r#"["10.{n}.0.0/8"]"#))
        }
    }
}

fn no_sleep(_: Duration) {}

#[test]
fn a_range_list_retries_with_backoff_and_reports_its_status() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = RangeCache::new(tmp.path().join("r"));
    cache.sleep = no_sleep;
    let providers = vec!["gptbot".to_string()];
    let mut book = RangeBook::new();
    // Succeeds on the third attempt.
    let flaky = Flaky {
        calls: AtomicUsize::new(0),
        fail: 2,
    };
    let out = &cache.load(&mut book, Some(&flaky), &providers, 1.0e9)[0];
    assert_eq!(
        out.origin,
        spiderhead_log_analyzer::rangecache::Origin::Fresh
    );
    assert_eq!(out.attempts, 3);
    let doc = out.to_json();
    assert_eq!(doc["available"], true);

    // Permanently down: explicit unavailable status, and the old copy stays.
    let down = Flaky {
        calls: AtomicUsize::new(0),
        fail: usize::MAX,
    };
    let later = 1.0e9 + spiderhead_log_analyzer::rangecache::DEFAULT_MAX_AGE + 1.0;
    let out = &cache.load(&mut book, Some(&down), &providers, later)[0];
    let doc = out.to_json();
    assert_eq!(doc["origin"], "stale", "the last good copy survives");
    assert_eq!(doc["available"], true, "a stale list is still usable");
    assert_eq!(doc["attempts"], 3, "1 try + 2 retries");
    assert!(doc["error"].as_str().unwrap().contains("failed"));

    // Nothing at all on disk: explicitly unavailable.
    let mut empty = RangeBook::new();
    let out = &cache.load(&mut empty, Some(&down), &["claudebot".into()], later)[0];
    let doc = out.to_json();
    assert_eq!(doc["origin"], "missing");
    assert_eq!(doc["available"], false);
}

#[test]
fn every_published_google_list_authenticates_its_ips() {
    let (_tmp, src) = artifact(&[
        ("66.249.66.9", GOOGLE),            // common-crawlers range
        ("2001:4860:4801:2000::9", GOOGLE), // common-crawlers IPv6
        ("192.178.2.9", ADSBOT),            // special-crawlers range
        ("74.125.10.9", FEED),              // user-triggered-fetchers range
        ("35.191.10.9", FEED),              // user-triggered-fetchers-google range
        ("8.34.208.9", FEED),               // user-triggered-agents range
        ("8.8.4.4", GOOGLE),                // outside every list: not list-verified
    ]);
    let v = verifier(Arc::new(FakeResolver::standard()));
    let book = google_book();
    let e = engine(&book, &v);
    let out = bot_verification_all(
        &src,
        &e,
        &AllQuery {
            mode: Mode::Ranges,
            ..AllQuery::default()
        },
        1.0e9,
        &|_, _| {},
    )
    .unwrap();
    // Every list member verified by ip_list; the outsider is not a list verdict.
    assert_eq!(out["method_counts"]["ip_list"], 6);
    assert_eq!(out["status_counts"]["verified"], 6);
    // The outsider keeps a non-list outcome (ranges mode runs no DNS).
    assert_eq!(out["status_counts"]["verified"], 6);
    assert_eq!(out["status_counts"]["unverified"], 1);
}

#[test]
fn a_google_owned_fetcher_ptr_confirms_through_the_google_suffix() {
    let mut resolver = FakeResolver::standard();
    resolver.ptr.insert(
        "35.191.60.9".into(),
        "google-proxy-35-191-60-9.google.com".into(),
    );
    resolver.forward.insert(
        "google-proxy-35-191-60-9.google.com".into(),
        vec!["35.191.60.9".into()],
    );
    let (_tmp, src) = artifact(&[("35.191.60.9", FEED)]);
    let v = verifier(Arc::new(resolver));
    let book = RangeBook::new(); // no list: rDNS must carry the verdict
    let e = engine(&book, &v);
    let out = bot_verification_all(
        &src,
        &e,
        &AllQuery {
            mode: Mode::Dns,
            ..AllQuery::default()
        },
        1.0e9,
        &|_, _| {},
    )
    .unwrap();
    assert_eq!(out["status_counts"]["verified"], 1);
    assert_eq!(out["method_counts"]["dns_reverse_forward"], 1);
}

struct RecordingClock {
    slept: Mutex<Duration>,
}
impl Clock for RecordingClock {
    fn monotonic(&self) -> f64 {
        spiderhead_log_analyzer::dns::SystemClock.monotonic()
    }
    fn wall(&self) -> f64 {
        spiderhead_log_analyzer::dns::SystemClock.wall()
    }
    fn sleep(&self, duration: Duration) {
        *self.slept.lock().unwrap() += duration;
    }
}

#[test]
fn the_lookup_rate_limit_paces_fresh_lookups() {
    let hits: Vec<(String, &str)> = (0..4u8)
        .map(|n| (format!("66.249.66.{n}"), GOOGLE))
        .collect();
    let hits: Vec<(&str, &str)> = hits.iter().map(|(ip, ua)| (ip.as_str(), *ua)).collect();
    let (_tmp, src) = artifact(&hits);
    let fake = Arc::new(FakeResolver::standard());
    let v = verifier(fake);
    let book = RangeBook::new();
    let clock = RecordingClock {
        slept: Mutex::new(Duration::ZERO),
    };
    let e = Engine {
        presets: PresetSet::builtin(),
        book: &book,
        verifier: &v,
        asn: None,
        clock: &clock,
    };
    bot_verification_all(
        &src,
        &e,
        &AllQuery {
            mode: Mode::Dns,
            workers: 1,
            dns_per_second: 2,
            ..AllQuery::default()
        },
        1.0e9,
        &|_, _| {},
    )
    .unwrap();
    let slept = *clock.slept.lock().unwrap();
    assert!(
        slept >= Duration::from_secs(1),
        "4 lookups at 2/s must pace about 1.5 s, slept {slept:?}"
    );
}

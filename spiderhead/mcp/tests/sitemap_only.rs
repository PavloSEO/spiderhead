//! `crawl_site --sitemap-only` and native scan resume over a scripted loopback
//! origin (the reference suite, the reference suite).

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};
use spiderhead_core::crawl_settings::{self, LoadInput};
use spiderhead_core::host::SystemHost;
use spiderhead_crawl::oracle;
use spiderhead_crawl::spider::{self, CrawlOptions};
use spiderhead_mcp::handlers;
use spiderhead_store::scan_schema::NativeScanSchemaMixin;

/// A loopback HTTP origin: path → (content-type, body) routes, every request
/// path recorded for the spidering assertions.
struct Site {
    base: String,
    hits: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Site {
    fn start<F: FnOnce(&str) -> Vec<(String, String, String)>>(build: F) -> Site {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let port = listener.local_addr().expect("local addr").port();
        let base = format!("http://127.0.0.1:{port}");
        let routes: std::collections::HashMap<String, (String, String)> = build(&base)
            .into_iter()
            .map(|(path, kind, body)| (path, (kind, body)))
            .collect();
        let hits = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let hits = Arc::clone(&hits);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            // Accepted sockets inherit the listener's nonblocking
                            // flag; the read loop below needs a blocking socket.
                            stream.set_nonblocking(false).expect("blocking stream");
                            stream
                                .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                                .expect("read timeout");
                            serve(&mut stream, &routes, &hits);
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
            })
        };
        Site {
            base,
            hits,
            stop,
            thread: Some(thread),
        }
    }

    fn hits(&self) -> Vec<String> {
        self.hits.lock().expect("hits").clone()
    }
}

impl Drop for Site {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(
    stream: &mut std::net::TcpStream,
    routes: &std::collections::HashMap<String, (String, String)>,
    hits: &Mutex<Vec<String>>,
) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut request = String::new();
    for _ in 0..64 {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let done = line == "\r\n" || line == "\n";
        request.push_str(&line);
        if done {
            break;
        }
    }
    let path = request
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/")
        .to_owned();
    hits.lock().expect("hits").push(path.clone());
    let (status, kind, body) = match routes.get(&path) {
        Some((kind, body)) if body.starts_with("REDIRECT:") => {
            ("302 Found", "text/plain".to_owned(), body.clone())
        }
        Some((kind, body)) if body.starts_with("STATUS:") => (
            match body.as_str() {
                "STATUS:503" => "503 Service Unavailable",
                "STATUS:500" => "500 Internal Server Error",
                _ => "500 Internal Server Error",
            },
            kind.clone(),
            "unavailable".to_owned(),
        ),
        Some((kind, body)) => ("200 OK", kind.clone(), body.clone()),
        None => (
            "404 Not Found",
            "text/plain".to_owned(),
            "not found".to_owned(),
        ),
    };
    if let Some(location) = body.strip_prefix("REDIRECT:") {
        let response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: {kind}\r\nlocation: {location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        );
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.shutdown(std::net::Shutdown::Both);
        return;
    }
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let mut body = body.as_bytes();
    let _ = std::io::copy(&mut body, stream);
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

/// One XML urlset over `base` + `members`, the shape `tests/serve` writes.
fn sitemap_xml(base: &str, members: &[&str]) -> String {
    let urls: String = members
        .iter()
        .map(|member| {
            let url = if member.starts_with("http") {
                member.to_string()
            } else {
                format!("{base}{member}")
            };
            format!("    <url><loc>{url}</loc></url>\n")
        })
        .collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n{urls}</urlset>\n"
    )
}

const ROBOTS: &str = "User-agent: *\nAllow: /\n";
const PAGE: &str = "<html><head><title>p</title></head><body>\
                    <a href=\"/\">home</a><a href=\"/other\">other</a></body></html>";

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sh-sitemap-only-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn documents(scan_path: &Path) -> i64 {
    let con = rusqlite::Connection::open(scan_path).expect("scan open");
    con.query_row("SELECT COUNT(*) FROM documents", [], |row| row.get(0))
        .expect("documents count")
}

fn context_count(scan_path: &Path, kind: &str) -> i64 {
    let con = rusqlite::Connection::open(scan_path).expect("scan open");
    con.query_row(
        "SELECT COUNT(*) FROM context_items WHERE kind=?",
        [kind],
        |row| row.get(0),
    )
    .expect("context count")
}

// (both url_budget cases: an explicit 0 budget means no cap)
#[test]
fn sitemap_only_crawls_only_declared_members() {
    for max_urls in [0, 8] {
        let site = Site::start(|base| {
            vec![
                (
                    "/robots.txt".to_owned(),
                    "text/plain".to_owned(),
                    ROBOTS.to_owned(),
                ),
                (
                    "/sitemap.xml".to_owned(),
                    "application/xml".to_owned(),
                    sitemap_xml(
                        base,
                        &["/member", "/jump", "http://outside.localhost/not-in-scope"],
                    ),
                ),
                (
                    "/member".to_owned(),
                    "text/html".to_owned(),
                    "<html><body><a href=\"/root-only\">root</a>\
                     <link rel=\"canonical\" href=\"/canonical-only\"></body></html>"
                        .to_owned(),
                ),
                (
                    "/jump".to_owned(),
                    "text/html".to_owned(),
                    "REDIRECT:/redirect-destination".to_owned(),
                ),
            ]
        });
        let work = scratch(&format!("members-{max_urls}"));
        let scan = work.join("scan.sqlite");
        let result = handlers::dispatch(
            "crawl_site",
            &json!({
                "sitemap": format!("{}/sitemap.xml", site.base),
                "sitemap_only": true,
                "max_urls": max_urls,
                "scan_out": scan.to_string_lossy(),
            }),
        )
        .expect("sitemap-only crawl");
        assert_eq!(result["finish_reason"], "finished", "{result:?}");
        assert_eq!(result["resumed"], false);
        assert_eq!(result["partial"], false);
        // The outside member is declared but not collected.
        assert_eq!(result["urls_collected"], json!(2));
        // Only sitemap work happened: the start URL, page link targets and
        // redirect destinations were never requested.
        let hits: BTreeSet<String> = site.hits().into_iter().collect();
        let allowed: BTreeSet<String> = ["/robots.txt", "/sitemap.xml", "/member", "/jump"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        assert!(
            hits.is_subset(&allowed),
            "unexpected requests: {:?}",
            hits.difference(&allowed).collect::<Vec<_>>()
        );
        assert!(!hits.contains("/"));
        assert!(!hits.contains("/root-only"));
        assert!(!hits.contains("/redirect-destination"));
        // Declared membership is retained in the scan context — all three
        // members, including the outside one.
        assert_eq!(context_count(&scan, "sitemap_declared_url"), 3);
        let con = rusqlite::Connection::open(&scan).expect("scan open");
        let source: String = con
            .query_row(
                "SELECT json_extract(payload_json,'$.source') FROM context_items \
                 WHERE kind='sitemap_declaration'",
                [],
                |row| row.get(0),
            )
            .expect("declaration source");
        assert_eq!(source, "sitemap-only");
        // The outside member is recorded as a scope refusal, not fetched.
        let outside: i64 = con
            .query_row(
                "SELECT COUNT(*) FROM decisions WHERE reason='outside_host'",
                [],
                |row| row.get(0),
            )
            .expect("outside decision");
        assert_eq!(outside, 1);
        assert_eq!(documents(&scan), 2);
        let _ = std::fs::remove_dir_all(&work);
    }
}

// sitemap input raises ValueError("sitemap_only requires an explicit sitemap")
// before any crawl starts; no dedicated oracle test pins it.
#[test]
fn sitemap_only_requires_an_explicit_sitemap() {
    let work = scratch("no-sitemap");
    let error = handlers::dispatch(
        "crawl_site",
        &json!({
            "url": "http://127.0.0.1:9/",
            "sitemap_only": true,
            "scan_out": work.join("scan.sqlite").to_string_lossy(),
        }),
    )
    .expect_err("sitemap-only without a sitemap must fail");
    assert_eq!(
        error.to_string(),
        "sitemap_only requires an explicit sitemap"
    );
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn sitemap_only_population_is_budgeted() {
    let site = Site::start(|base| {
        vec![
            (
                "/robots.txt".to_owned(),
                "text/plain".to_owned(),
                ROBOTS.to_owned(),
            ),
            (
                "/sitemap.xml".to_owned(),
                "application/xml".to_owned(),
                sitemap_xml(base, &["/one", "/two", "/three", "/four"]),
            ),
            ("/one".to_owned(), "text/html".to_owned(), PAGE.to_owned()),
        ]
    });
    let work = scratch("budget");
    let error = handlers::dispatch(
        "crawl_site",
        &json!({
            "sitemap": format!("{}/sitemap.xml", site.base),
            "sitemap_only": true,
            "max_urls": 2,
            "scan_out": work.join("scan.sqlite").to_string_lossy(),
        }),
    )
    .expect_err("an overflowing declared population must fail");
    assert_eq!(
        error.to_string(),
        "sitemap-only population exceeds the configured URL budget"
    );
    // Nothing was crawled: the budget gates the declared population up front.
    let hits: BTreeSet<String> = site.hits().into_iter().collect();
    assert!(hits.is_subset(&BTreeSet::from([
        "/robots.txt".to_owned(),
        "/sitemap.xml".to_owned()
    ])));
    let _ = std::fs::remove_dir_all(&work);
}

// (all three parametrized cases: an empty urlset and malformed XML raise, an
// HTTP failure interrupts the run instead)
#[test]
fn sitemap_only_unusable_xml_never_falls_back_to_root() {
    // `raises` — a declared-but-empty population is a ScanError, not a crawl.
    for (case, body, raises) in [
        (
            "empty",
            "<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\"></urlset>".to_owned(),
            true,
        ),
        ("malformed", "<urlset>".to_owned(), true),
        ("http-failure", "STATUS:503".to_owned(), false),
    ] {
        let site = Site::start(|_base| {
            vec![
                (
                    "/robots.txt".to_owned(),
                    "text/plain".to_owned(),
                    ROBOTS.to_owned(),
                ),
                (
                    "/sitemap.xml".to_owned(),
                    "application/xml".to_owned(),
                    body,
                ),
            ]
        });
        let work = scratch(&format!("unusable-{case}"));
        let scan = work.join("scan.sqlite");
        let outcome = handlers::dispatch(
            "crawl_site",
            &json!({
                "sitemap": format!("{}/sitemap.xml", site.base),
                "sitemap_only": true,
                "scan_out": scan.to_string_lossy(),
            }),
        );
        let hits: BTreeSet<String> = site.hits().into_iter().collect();
        assert!(!hits.contains("/"), "{case}: the start URL was crawled");
        assert!(hits.contains("/sitemap.xml"), "{case}");
        if raises {
            let error = outcome.expect_err(&format!("{case}: unusable sitemap must fail"));
            assert_eq!(
                error.to_string(),
                "sitemap-only source declared no crawlable URL members",
                "{case}"
            );
            if scan.exists() {
                let con = rusqlite::Connection::open(&scan).expect("scan open");
                let lifecycle: String = con
                    .query_row("SELECT lifecycle FROM scan", [], |row| row.get(0))
                    .expect("lifecycle");
                assert_ne!(lifecycle, "finished", "{case}");
            }
        } else {
            let result = outcome
                .unwrap_or_else(|_| panic!("{case}: an HTTP failure is a run, not an error"));
            assert!(
                result["partial"] == json!(true) || result["finish_reason"] != json!("finished"),
                "{case}: {result:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&work);
    }
}

/// Phase 1 of a resume test: a sitemap-only run parked mid-flight, exactly
/// like a killed process — the inflight lease stays, nothing is finalized.
fn interrupted_sitemap_scan(scan_path: &Path, sitemap_url: &str) {
    interrupted_scan_with(scan_path, sitemap_url, true, Map::new());
}

fn interrupted_sitemap_scan_with(
    scan_path: &Path,
    sitemap_url: &str,
    overrides: Map<String, Value>,
) {
    interrupted_scan_with(scan_path, sitemap_url, true, overrides);
}

fn interrupted_scan_with(
    scan_path: &Path,
    start_url: &str,
    sitemap_only: bool,
    overrides: Map<String, Value>,
) {
    let host = SystemHost;
    let settings_value = crawl_settings::load(
        &LoadInput {
            file: None,
            overrides,
            base_overrides: Map::new(),
        },
        &host,
    )
    .expect("settings");
    let settings = settings_value.as_object().expect("settings object").clone();
    let roots: Vec<(String, String)> = if sitemap_only {
        vec![(start_url.to_owned(), "sitemap-only".to_owned())]
    } else {
        Vec::new()
    };
    let mut scan = NativeScanSchemaMixin::create(
        scan_path,
        start_url,
        &settings,
        None,
        oracle::WRITER_VERSION,
        oracle::WRITER_REVISION,
        &oracle::runtime_versions(),
        &oracle::limitations(),
        None,
        &roots,
        "scan.v1",
    )
    .expect("scan create");
    let options = CrawlOptions {
        config: Value::Object(settings.clone()),
        interrupt_after_pages: Some(1),
        client: spiderhead_fetch::client::build_client(15.0).expect("client"),
        start_url: start_url.to_owned(),
        seed_urls: roots.iter().map(|(url, _)| url.clone()).collect(),
        user_agent: "SpiderHeadBot/3.2.0 (+https://github.com/PavloSEO/spiderhead)".into(),
        robots_token: "SpiderHeadBot".into(),
        robots_policy: "respect".into(),
        min_delay: 0.02,
        max_delay: 60.0,
        concurrency: 1,
        max_urls: 200,
        max_depth: 5,
        max_requests: 600,
        max_crawl_seconds: 400.0,
        timeout_seconds: 15.0,
        retry_on_timeout: 0,
        retry_server_errors: 0,
        max_response_bytes: 5_242_880,
        entity_capture_limit: 5_242_880,
        max_url_length: 2000,
        parse_options: Default::default(),
        crawl_redirects: true,
        sitemap_only,
        resumed: false,
        rate_override: None,
        on_batch: None,
        request_gate: None,
    };
    let outcome = tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(spider::crawl_site(&mut scan, &options))
        .expect("phase 1");
    assert_eq!(outcome.finish_reason, "interrupted");
    scan.close();
}

// (the killed-process phase is an unclosed inflight lease — identical on disk)
#[test]
fn sitemap_only_resume_replays_stored_membership() {
    let site = Site::start(|base| {
        vec![
            (
                "/robots.txt".to_owned(),
                "text/plain".to_owned(),
                ROBOTS.to_owned(),
            ),
            (
                "/sitemap.xml".to_owned(),
                "application/xml".to_owned(),
                sitemap_xml(base, &["/one", "/two", "/three"]),
            ),
            ("/one".to_owned(), "text/html".to_owned(), PAGE.to_owned()),
            ("/two".to_owned(), "text/html".to_owned(), PAGE.to_owned()),
            ("/three".to_owned(), "text/html".to_owned(), PAGE.to_owned()),
        ]
    });
    let work = scratch("resume");
    let scan_path = work.join("scan.sqlite");
    let sitemap_url = format!("{}/sitemap.xml", site.base);
    interrupted_sitemap_scan(&scan_path, &sitemap_url);
    let phase_one: BTreeSet<String> = site.hits().into_iter().collect();

    // Phase 2: the resume route. Stored membership drives the frontier — the
    // sitemap itself is never re-fetched and the site root is never visited.
    let result = handlers::dispatch(
        "crawl_site",
        &json!({
            "resume": scan_path.to_string_lossy(),
        }),
    )
    .expect("resume");
    assert_eq!(result["resumed"], true, "{result:?}");
    assert_eq!(result["finish_reason"], "finished");
    assert_eq!(result["partial"], false);
    assert_eq!(documents(&scan_path), 3);
    let phase_two: Vec<String> = site
        .hits()
        .into_iter()
        .filter(|hit| !phase_one.contains(hit))
        .collect();
    assert!(
        !phase_two.contains(&"/sitemap.xml".to_owned()),
        "resume re-fetched the sitemap: {phase_two:?}"
    );
    assert!(!phase_two.contains(&"/robots.txt".to_owned()));
    let all: BTreeSet<String> = site.hits().into_iter().collect();
    assert!(!all.contains("/"));
    assert!(!all.contains("/other"));
    let _ = std::fs::remove_dir_all(&work);
}

// and the reference suite
#[test]
fn resume_flag_checks() {
    let work = scratch("flags");
    // A nonexistent artifact hits the file check before anything else.
    let missing = work.join("missing.sqlite");
    let error = handlers::dispatch("crawl_site", &json!({"resume": missing.to_string_lossy()}))
        .expect_err("missing scan");
    assert_eq!(
        error.to_string(),
        format!(
            "scan to resume does not exist: {}",
            missing.to_string_lossy()
        )
    );
    // Crawl-shaping arguments conflict by name, before the artifact is read.
    for (name, value) in [
        ("sitemap", json!("http://127.0.0.1:1/sitemap.xml")),
        ("max_urls", json!(5)),
        ("user_agent", json!("SomeBot")),
    ] {
        let error = handlers::dispatch(
            "crawl_site",
            &json!({"resume": missing.to_string_lossy(), name: value}),
        )
        .expect_err("conflicting input");
        assert_eq!(
            error.to_string(),
            format!(
                "resume continues the crawl its artifact already describes and reads every \
                 setting from it; it cannot be combined with {name}"
            ),
            "{name}"
        );
    }
    // A stored false `sitemap_only` is normalized away by both surfaces —
    // it reaches the artifact check instead of conflicting.
    let error = handlers::dispatch(
        "crawl_site",
        &json!({"resume": missing.to_string_lossy(), "sitemap_only": false}),
    )
    .expect_err("normalized false should not conflict");
    assert!(
        error
            .to_string()
            .starts_with("scan to resume does not exist:"),
        "explicit false must not conflict: {error}"
    );
    let _ = std::fs::remove_dir_all(&work);
}

// accepted restatements (a file that is not a scan, a finished scan, a
// mismatched start URL or producing build, a credentialed scan, the same
// scan_out path restated).
#[test]
fn resume_flag_artifact_refusals() {
    let site = Site::start(|base| {
        vec![
            (
                "/robots.txt".to_owned(),
                "text/plain".to_owned(),
                ROBOTS.to_owned(),
            ),
            (
                "/sitemap.xml".to_owned(),
                "application/xml".to_owned(),
                sitemap_xml(base, &["/one", "/two"]),
            ),
            ("/one".to_owned(), "text/html".to_owned(), PAGE.to_owned()),
            ("/two".to_owned(), "text/html".to_owned(), PAGE.to_owned()),
        ]
    });
    let work = scratch("refusals");
    let resume = |name: &str| {
        let scan_path = work.join(name);
        interrupted_sitemap_scan(&scan_path, &format!("{}/sitemap.xml", site.base));
        scan_path.to_string_lossy().into_owned()
    };

    // A file that is not a scan at all.
    let garbage = work.join("notes.txt");
    std::fs::write(&garbage, b"not a sqlite scan").expect("garbage");
    let error = handlers::dispatch("crawl_site", &json!({"resume": garbage.to_string_lossy()}))
        .expect_err("not a scan");
    assert!(
        error.to_string().contains("cannot inspect native scan"),
        "{error}"
    );

    // A start URL different from the stored one is refused by name.
    let path = resume("scan-a.sqlite");
    let error = handlers::dispatch(
        "crawl_site",
        &json!({"resume": path, "url": "http://127.0.0.1:9/other"}),
    )
    .expect_err("different start url");
    assert!(
        error
            .to_string()
            .contains("refusing to resume one crawl as another"),
        "{error}"
    );
    // Both URLs are named: the message must say which crawl the artifact was
    // and which start the caller asked to resume as.
    assert!(error
        .to_string()
        .contains(&format!("{}/sitemap.xml", site.base)));
    assert!(error.to_string().contains("http://127.0.0.1:9/other"));
    // A producing build different from the artifact's is refused by name.
    let error = handlers::dispatch(
        "crawl_site",
        &json!({"resume": path, "producer_build": "0".repeat(40)}),
    )
    .expect_err("different build");
    assert!(
        error.to_string().contains("refusing a mixed-build resume"),
        "{error}"
    );
    // The same scan_out path restated is accepted — and finishes the scan.
    let result = handlers::dispatch("crawl_site", &json!({"resume": path, "scan_out": path}))
        .expect("same scan_out accepted");
    assert_eq!(result["finish_reason"], "finished", "{result:?}");
    // A scan that already finished has nothing left to resume.
    let error =
        handlers::dispatch("crawl_site", &json!({"resume": path})).expect_err("finished scan");
    assert!(
        error
            .to_string()
            .contains("there is nothing left to resume"),
        "{error}"
    );

    // A credentialed scan cannot be reconstructed from redacted settings.
    std::env::set_var("SH_CREDENTIAL_TEST", "placeholder");
    let credentialed = work.join("scan-b.sqlite");
    let mut overrides = Map::new();
    overrides.insert("http.credentials_acknowledged".into(), json!(true));
    overrides.insert(
        "http.credential_headers".into(),
        json!([{"host": "127.0.0.1", "headers": {"Authorization": "env:SH_CREDENTIAL_TEST"}}]),
    );
    interrupted_sitemap_scan_with(
        &credentialed,
        &format!("{}/sitemap.xml", site.base),
        overrides,
    );
    let error = handlers::dispatch(
        "crawl_site",
        &json!({"resume": credentialed.to_string_lossy()}),
    )
    .expect_err("credentialed scan");
    let text = error.to_string();
    assert!(text.contains("redacted"), "{text}");
    assert!(
        text.contains("--config") && text.contains("--scan-out"),
        "{text}"
    );
    let _ = std::fs::remove_dir_all(&work);
}

// (both parametrized cases)
#[test]
fn resume_applies_politeness_overrides_as_a_live_rate() {
    let stored_throttle = |scan_path: &Path| {
        let con = rusqlite::Connection::open(scan_path).expect("scan open");
        let text: String = con
            .query_row(
                "SELECT throttle_state_json FROM resume_state WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .expect("throttle state");
        serde_json::from_str::<Value>(&text).expect("throttle json")
    };
    let stored_config = |scan_path: &Path| {
        let con = rusqlite::Connection::open(scan_path).expect("scan open");
        con.query_row(
            "SELECT config_json FROM scan WHERE singleton=1",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("stored config")
    };
    // (arguments, expected min_delay floor)
    for (arguments, want_delay) in [
        (json!({"min_delay": 0.1}), 0.1),
        (
            json!({"overrides": {"speed.min_delay_seconds": 0.2, "speed.concurrency": 4}}),
            0.2,
        ),
    ] {
        let site = Site::start(|base| {
            vec![
                (
                    "/robots.txt".to_owned(),
                    "text/plain".to_owned(),
                    ROBOTS.to_owned(),
                ),
                (
                    "/sitemap.xml".to_owned(),
                    "application/xml".to_owned(),
                    sitemap_xml(base, &["/one", "/two"]),
                ),
                ("/one".to_owned(), "text/html".to_owned(), PAGE.to_owned()),
                ("/two".to_owned(), "text/html".to_owned(), PAGE.to_owned()),
            ]
        });
        let work = scratch("rate");
        let scan_path = work.join("scan.sqlite");
        let sitemap_url = format!("{}/sitemap.xml", site.base);
        interrupted_sitemap_scan(&scan_path, &sitemap_url);
        let config_before = stored_config(&scan_path);

        let mut request = json!({"resume": scan_path.to_string_lossy()});
        request
            .as_object_mut()
            .expect("request")
            .extend(arguments.as_object().expect("arguments").clone());
        let result = handlers::dispatch("crawl_site", &request).expect("resume");
        assert_eq!(result["finish_reason"], "finished", "{result:?}");
        // The override moved the live pacing floor; a raised concurrency
        // ceiling still widens adaptively, so the live count keeps its clamp.
        let throttle = stored_throttle(&scan_path);
        assert_eq!(
            throttle["delay_seconds"].as_f64(),
            Some(want_delay),
            "{throttle:?}"
        );
        assert_eq!(throttle["concurrency"].as_f64(), Some(1.0), "{throttle:?}");
        // The stored settings — and so the resume fingerprint — stay untouched.
        assert_eq!(stored_config(&scan_path), config_before);
        let _ = std::fs::remove_dir_all(&work);
    }
}

#[test]
fn filtered_decision_survives_native_resume_with_the_same_filters() {
    let site = Site::start(|base| {
        let _ = base;
        vec![
            (
                "/robots.txt".to_owned(),
                "text/plain".to_owned(),
                ROBOTS.to_owned(),
            ),
            (
                "/".to_owned(),
                "text/html".to_owned(),
                "<html><body><a href=\"/manual.pdf\">pdf</a>\
                 <a href=\"/next.html\">next</a></body></html>"
                    .to_owned(),
            ),
            (
                "/manual.pdf".to_owned(),
                "application/pdf".to_owned(),
                "pdf".to_owned(),
            ),
            (
                "/next.html".to_owned(),
                "text/html".to_owned(),
                "<html><body>resumed</body></html>".to_owned(),
            ),
        ]
    });
    let work = scratch("filters");
    let scan_path = work.join("scan.sqlite");
    let start = format!("{}/", site.base);

    // Phase 1: a plain crawl with file-type filters, parked mid-flight.
    let host = SystemHost;
    let mut overrides = Map::new();
    overrides.insert("scope.exclude_extensions".into(), json!(["pdf"]));
    overrides.insert("scope.include_media_types".into(), json!(["text/html"]));
    let settings_value = crawl_settings::load(
        &LoadInput {
            file: None,
            overrides,
            base_overrides: Map::new(),
        },
        &host,
    )
    .expect("settings");
    let settings = settings_value.as_object().expect("settings object").clone();
    let mut scan = NativeScanSchemaMixin::create(
        &scan_path,
        &start,
        &settings,
        None,
        oracle::WRITER_VERSION,
        oracle::WRITER_REVISION,
        &oracle::runtime_versions(),
        &oracle::limitations(),
        None,
        &[],
        "scan.v1",
    )
    .expect("scan create");
    let options = CrawlOptions {
        config: Value::Object(settings.clone()),
        interrupt_after_pages: Some(1),
        client: spiderhead_fetch::client::build_client(15.0).expect("client"),
        start_url: start.clone(),
        seed_urls: Vec::new(),
        user_agent: "SpiderHeadBot/3.2.0 (+https://github.com/PavloSEO/spiderhead)".into(),
        robots_token: "SpiderHeadBot".into(),
        robots_policy: "respect".into(),
        min_delay: 0.02,
        max_delay: 60.0,
        concurrency: 1,
        max_urls: 200,
        max_depth: 5,
        max_requests: 600,
        max_crawl_seconds: 400.0,
        timeout_seconds: 15.0,
        retry_on_timeout: 0,
        retry_server_errors: 0,
        max_response_bytes: 5_242_880,
        entity_capture_limit: 5_242_880,
        max_url_length: 2000,
        parse_options: Default::default(),
        crawl_redirects: true,
        sitemap_only: false,
        resumed: false,
        rate_override: None,
        on_batch: None,
        request_gate: None,
    };
    let outcome = tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(spider::crawl_site(&mut scan, &options))
        .expect("phase 1");
    assert_eq!(outcome.finish_reason, "interrupted");
    scan.close();
    let phase_one: BTreeSet<String> = site.hits().into_iter().collect();

    // Phase 2: resume replays the stored frontier — the filtered decision is
    // not re-taken and the extension-filtered URL is never fetched.
    let result = handlers::dispatch(
        "crawl_site",
        &json!({"resume": scan_path.to_string_lossy()}),
    )
    .expect("resume");
    assert_eq!(result["resumed"], true, "{result:?}");
    assert_eq!(result["finish_reason"], "finished");
    let phase_two: Vec<String> = site
        .hits()
        .into_iter()
        .filter(|hit| !phase_one.contains(hit))
        .collect();
    assert_eq!(phase_two, vec!["/next.html".to_owned()], "{phase_two:?}");
    let hits: BTreeSet<String> = site.hits().into_iter().collect();
    assert!(
        !hits.contains("/manual.pdf"),
        "filtered url fetched: {hits:?}"
    );
    let con = rusqlite::Connection::open(&scan_path).expect("scan open");
    let reason: String = con
        .query_row(
            "SELECT reason FROM decisions WHERE url LIKE '%/manual.pdf'",
            [],
            |row| row.get(0),
        )
        .expect("decision");
    assert_eq!(reason, "excluded_by_extension");
    let linked: i64 = con
        .query_row(
            "SELECT COUNT(*) FROM links l JOIN urls d ON d.url_id=l.destination_url_id \
             WHERE d.url LIKE '%/manual.pdf'",
            [],
            |row| row.get(0),
        )
        .expect("link");
    assert!(linked > 0, "link to the filtered destination retained");
    let _ = std::fs::remove_dir_all(&work);
}

// fetches every URL exactly as often as one uninterrupted run, the stored
// audit records the resume provenance, and the resumed artifact is its own
// run identity.
#[test]
fn killed_crawl_resumed_fetches_every_url_once_and_records_resumed() {
    let link_page = |href: &str| format!("<html><body><a href=\"{href}\">next</a></body></html>");
    let site = Site::start(|_base| {
        vec![
            (
                "/robots.txt".to_owned(),
                "text/plain".to_owned(),
                ROBOTS.to_owned(),
            ),
            ("/".to_owned(), "text/html".to_owned(), link_page("/a")),
            ("/a".to_owned(), "text/html".to_owned(), link_page("/b")),
            ("/b".to_owned(), "text/html".to_owned(), link_page("/c")),
            (
                "/c".to_owned(),
                "text/html".to_owned(),
                "<html><body>leaf</body></html>".to_owned(),
            ),
        ]
    });
    let work = scratch("chain");

    // The uninterrupted reference run.
    let whole = work.join("whole.sqlite");
    let result = handlers::dispatch(
        "crawl_site",
        &json!({
            "url": format!("{}/", site.base),
            "scan_out": whole.to_string_lossy(),
            "min_delay": 0,
        }),
    )
    .expect("whole crawl");
    assert_eq!(result["finish_reason"], "finished", "{result:?}");
    let count_hits = |hits: &[String]| {
        hits.iter()
            .fold(std::collections::BTreeMap::new(), |mut map, hit| {
                *map.entry(hit.clone()).or_insert(0usize) += 1;
                map
            })
    };
    let whole_hits = count_hits(&site.hits());
    assert!(!whole_hits.is_empty());
    let whole_len = site.hits().len();

    // The killed-and-resumed run over the same site, under the same settings
    // the uninterrupted run used (min_delay=0, like the reference fixture).
    let resumable = work.join("resumed.sqlite");
    let mut overrides = Map::new();
    overrides.insert("speed.min_delay_seconds".into(), json!(0));
    interrupted_scan_with(&resumable, &format!("{}/", site.base), false, overrides);
    let killed_len = site.hits().len();
    let interrupted_hits = count_hits(&site.hits()[whole_len..]);
    assert!(!interrupted_hits.is_empty());

    let result = handlers::dispatch(
        "crawl_site",
        &json!({"resume": resumable.to_string_lossy()}),
    )
    .expect("resume");
    assert_eq!(result["resumed"], true, "{result:?}");
    assert_eq!(result["partial"], false);
    assert_eq!(result["finish_reason"], "finished");
    assert_eq!(result["audit_available"], true, "{result:?}");

    // The property resume exists for: across the killed run and its resume the
    // site was asked for each URL exactly the number of times the
    // uninterrupted crawl asks — never twice.
    let mut resumed_hits = interrupted_hits;
    for (url, count) in count_hits(&site.hits()[killed_len..]) {
        *resumed_hits.entry(url).or_insert(0) += count;
    }
    assert_eq!(resumed_hits, whole_hits, "{resumed_hits:?}");
    assert_eq!(*resumed_hits.values().max().unwrap_or(&0), 1);

    // Each run keeps its own scan identity.
    let uuid_of = |path: &Path| {
        rusqlite::Connection::open(path)
            .expect("scan open")
            .query_row("SELECT scan_uuid FROM scan WHERE singleton=1", [], |row| {
                row.get::<_, String>(0)
            })
            .expect("uuid")
    };
    assert_ne!(uuid_of(&whole), uuid_of(&resumable));

    // Byte-identical evidence must not make the runs indistinguishable: the
    // stored audit names the resume for one and not for the other.
    let resumed_audit =
        spiderhead_store::scan_artifact::read_audit(&resumable).expect("resumed audit");
    let whole_audit = spiderhead_store::scan_artifact::read_audit(&whole).expect("whole audit");
    assert_eq!(resumed_audit["run"]["crawl_resumed"], json!(true));
    assert_eq!(whole_audit["run"]["crawl_resumed"], json!(false));

    // Exactly one semantic difference, and it is the one that must be there:
    // an audit that matched in every other field would leave a resumed run
    // indistinguishable from a whole one.
    let differences: BTreeSet<String> = audit_differences(
        &semantic_audit(&resumed_audit),
        &semantic_audit(&whole_audit),
        "",
    )
    .into_iter()
    .collect();
    assert_eq!(
        differences,
        BTreeSet::from(["/run/crawl_resumed".to_owned()]),
        "{differences:?}"
    );
    let _ = std::fs::remove_dir_all(&work);
}

/// `semantic_audit`: scan-local additive references dropped before comparing
/// verdicts (the stored `run.scan_uuid` also varies by artifact).
fn semantic_audit(document: &Value) -> Value {
    let mut result = document.clone();
    if let Some(summary) = result.get_mut("summary").and_then(Value::as_object_mut) {
        summary.remove("evidence_contract");
        summary.remove("saved_corpus_derivations");
    }
    if let Some(issues) = result.get_mut("issues").and_then(Value::as_array_mut) {
        for issue in issues.iter_mut() {
            let empty = {
                let Some(evidence) = issue.get_mut("evidence").and_then(Value::as_object_mut)
                else {
                    continue;
                };
                evidence.remove("contract");
                evidence.is_empty()
            };
            if empty {
                if let Some(map) = issue.as_object_mut() {
                    map.remove("evidence");
                }
            }
        }
    }
    if let Some(run) = result.get_mut("run").and_then(Value::as_object_mut) {
        run.remove("scan_uuid");
    }
    result
}

/// `_differences`: every path at which two audit documents disagree, timing
/// fields aside.
fn audit_differences(left: &Value, right: &Value, path: &str) -> Vec<String> {
    const TIMING_FIELDS: [&str; 2] = ["generated_at", "response_time"];
    match (left, right) {
        (Value::Object(a), Value::Object(b)) => {
            let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
            let mut diffs = Vec::new();
            for key in keys {
                if TIMING_FIELDS.contains(&key.as_str()) {
                    continue;
                }
                match (a.get(key), b.get(key)) {
                    (None, _) | (_, None) => diffs.push(format!("{path}/{key}")),
                    (Some(one), Some(other)) => {
                        diffs.extend(audit_differences(one, other, &format!("{path}/{key}")));
                    }
                }
            }
            diffs
        }
        (Value::Array(a), Value::Array(b)) => {
            if a.len() != b.len() {
                return vec![format!("{path}[{} vs {}]", a.len(), b.len())];
            }
            a.iter()
                .zip(b.iter())
                .enumerate()
                .flat_map(|(index, (one, other))| {
                    audit_differences(one, other, &format!("{path}[{index}]"))
                })
                .collect()
        }
        _ => {
            if left == right {
                Vec::new()
            } else {
                vec![path.to_owned()]
            }
        }
    }
}

// (a 404 means no restrictions, and the artifact records the parsed shape)
#[test]
fn a_missing_robots_txt_does_not_stop_a_scan_crawl() {
    let site = Site::start(|_base| {
        vec![(
            "/".to_owned(),
            "text/html".to_owned(),
            "<html><body>home</body></html>".to_owned(),
        )]
    });
    let work = scratch("no-robots");
    let scan = work.join("scan.sqlite");
    let result = handlers::dispatch(
        "crawl_site",
        &json!({
            "url": format!("{}/", site.base),
            "scan_out": scan.to_string_lossy(),
            "min_delay": 0,
        }),
    )
    .expect("a missing robots.txt must not stop the crawl");
    assert_eq!(result["urls_collected"], json!(1), "{result:?}");
    let con = rusqlite::Connection::open(&scan).expect("scan open");
    let reason: String = con
        .query_row(
            "SELECT reason FROM context_items WHERE kind='robots_summary'",
            [],
            |row| row.get(0),
        )
        .expect("robots summary row");
    assert_eq!(reason, "no robots.txt");
    let _ = std::fs::remove_dir_all(&work);
}

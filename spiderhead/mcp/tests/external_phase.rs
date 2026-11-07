//! The external-checks phase over two loopback origins: an internal crawl
//! links to external destinations, the phase probes them (HEAD) under the
//! `external_checks` budgets, and every decision lands in `external_checks`
//! with the run summary.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use spiderhead_mcp::handlers;

/// A loopback HTTP origin: path → (status-headers override, body), every
/// requested path recorded. HEAD and GET answer the same routes.
struct Site {
    base: String,
    hits: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Site {
    /// `routes` maps path → (status line, location, content-type, body).
    fn start(routes: Vec<(String, String, String, String, String)>) -> Site {
        Self::start_on("127.0.0.1", routes)
    }

    /// Bind a specific address: the external origin of these tests lives on
    /// `[::1]`, a genuinely different host from the internal `127.0.0.1`.
    fn start_on(bind: &str, routes: Vec<(String, String, String, String, String)>) -> Site {
        let ip: std::net::IpAddr = bind.trim_matches(['[', ']']).parse().expect("bind address");
        let listener = TcpListener::bind((ip, 0)).expect("bind loopback");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let local = listener.local_addr().expect("local addr");
        let host = match local.ip() {
            std::net::IpAddr::V4(v4) => v4.to_string(),
            std::net::IpAddr::V6(v6) => format!("[{v6}]"),
        };
        let base = format!("http://{host}:{}", local.port());
        let routes: std::collections::HashMap<String, (String, String, String, String)> = routes
            .into_iter()
            .map(|(path, status, location, kind, body)| (path, (status, location, kind, body)))
            .collect();
        let hits = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (hits, stop) = (Arc::clone(&hits), Arc::clone(&stop));
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
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

    fn hit_count(&self, path: &str) -> usize {
        self.hits
            .lock()
            .expect("hits")
            .iter()
            .filter(|hit| *hit == path)
            .count()
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
    routes: &std::collections::HashMap<String, (String, String, String, String)>,
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
    let (status, location, kind, body) = match routes.get(&path) {
        Some(found) => found.clone(),
        None => (
            "404 Not Found".to_owned(),
            String::new(),
            "text/plain".to_owned(),
            "not found".to_owned(),
        ),
    };
    let mut response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: {kind}\r\ncontent-length: {}\r\n",
        body.len()
    );
    if !location.is_empty() {
        response.push_str(&format!("location: {location}\r\n"));
    }
    response.push_str("connection: close\r\n\r\n");
    let _ = stream.write_all(response.as_bytes());
    let mut body = body.as_bytes();
    let _ = std::io::copy(&mut body, stream);
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

const ROBOTS: &str = "User-agent: *\nAllow: /\n";

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .canonicalize()
        .unwrap_or_else(|_| std::env::temp_dir())
        .join(format!("sh-external-phase-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn checks(scan: &std::path::Path) -> Vec<Value> {
    let con = rusqlite::Connection::open(scan).expect("scan open");
    let mut stmt = con
        .prepare("SELECT payload_json FROM external_checks ORDER BY ordinal")
        .expect("checks");
    stmt.query_map([], |r| r.get::<_, String>(0))
        .expect("rows")
        .map(|row| serde_json::from_str(&row.expect("row")).expect("payload"))
        .collect()
}

fn summary(scan: &std::path::Path) -> Value {
    let con = rusqlite::Connection::open(scan).expect("scan open");
    let text: String = con
        .query_row(
            "SELECT payload_json FROM external_check_summary WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .expect("summary");
    serde_json::from_str(&text).expect("summary json")
}

/// The internal crawl links to three external destinations; the target budget
/// admits two. Both probed destinations answer, the third is skipped with its
/// reason, and the summary counts every decision.
#[test]
fn external_targets_are_probed_and_budgeted() {
    let external = Site::start_on(
        "[::1]",
        vec![
            (
                "/partner".to_owned(),
                "200 OK".to_owned(),
                String::new(),
                "text/html".to_owned(),
                "<html>partner</html>".to_owned(),
            ),
            (
                "/moved".to_owned(),
                "301 Moved Permanently".to_owned(),
                "/partner".to_owned(),
                "text/html".to_owned(),
                String::new(),
            ),
            (
                "/gone".to_owned(),
                "404 Not Found".to_owned(),
                String::new(),
                "text/plain".to_owned(),
                "gone".to_owned(),
            ),
        ],
    );
    let page = format!(
        "<html><body>\
         <a href=\"{}/partner\">partner</a>\
         <a href=\"{}/moved\">moved</a>\
         <a href=\"{}/gone\">gone</a>\
         </body></html>",
        external.base, external.base, external.base
    );
    let internal = Site::start(vec![
        (
            "/robots.txt".to_owned(),
            "200 OK".to_owned(),
            String::new(),
            "text/plain".to_owned(),
            ROBOTS.to_owned(),
        ),
        (
            "/".to_owned(),
            "200 OK".to_owned(),
            String::new(),
            "text/html".to_owned(),
            page,
        ),
    ]);
    let work = scratch("budgets");
    let scan = work.join("scan.sqlite");

    let answer = handlers::dispatch(
        "crawl_site",
        &json!({
            "url": format!("{}/", internal.base),
            "scan_out": scan.to_string_lossy(),
            "min_delay": 0,
            "overrides": {
                "discovery.external.crawl": true,
                "storage.format_version": "scan.v2",
                "external_checks.max_targets": 2,
            },
        }),
    )
    .expect("crawl");
    assert_eq!(answer["finish_reason"], json!("finished"), "{answer}");

    let rows = checks(&scan);
    assert_eq!(rows.len(), 3, "every destination decided: {rows:#?}");
    let by_url = |path: &str| {
        rows.iter()
            .find(|row| row["url"].as_str().is_some_and(|u| u.ends_with(path)))
            .cloned()
            .unwrap_or_else(|| panic!("no row for {path}: {rows:#?}"))
    };
    let partner = by_url("/partner");
    assert_eq!(partner["outcome"], json!("fetched"));
    assert_eq!(partner["status_code"], json!(200));
    assert_eq!(partner["source_count"], json!(1));
    assert_eq!(
        partner["sources"],
        json!([format!("{}/", internal.base)]),
        "the source page is linked to its external check"
    );

    // The redirect is followed to its last hop within the redirect budget.
    let moved = by_url("/moved");
    assert_eq!(moved["outcome"], json!("fetched"));
    assert_eq!(moved["status_code"], json!(200), "{moved}");
    assert_eq!(
        moved["final_url"],
        json!(format!("{}/partner", external.base))
    );
    assert_eq!(
        moved["redirect_chain"],
        json!([
            format!("{}/moved", external.base),
            format!("{}/partner", external.base)
        ])
    );

    // The third destination never got a request: the target budget admitted two.
    let gone = by_url("/gone");
    assert_eq!(gone["outcome"], json!("skipped"));
    assert_eq!(gone["reason"], json!("targets over the configured budget"));
    assert_eq!(
        external.hit_count("/gone"),
        0,
        "no request for a skipped target"
    );

    let totals = summary(&scan);
    assert_eq!(totals["destinations"], json!(3));
    assert_eq!(totals["fetched"], json!(2));
    assert_eq!(totals["skipped"], json!(1));
    assert_eq!(
        totals["requests"],
        json!(3),
        "moved cost two hops: {totals}"
    );
    assert_eq!(totals["hosts_contacted"], json!(1));
    assert_eq!(totals["finish_reason"], json!("finished"));
    assert_eq!(
        totals["skipped_reasons"]["targets over the configured budget"],
        json!(1)
    );
    assert_eq!(
        totals["policy"]["max_targets"],
        json!(2),
        "the effective policy is echoed"
    );

    // The internal origin saw only robots and the page.
    assert_eq!(internal.hit_count("/"), 1);
    let _ = std::fs::remove_dir_all(&work);
}

/// A failing destination is a decision, not an error: the failed outcome and
/// its status are retained, the crawl finishes.
#[test]
fn a_failing_external_destination_is_a_retained_outcome() {
    let external = Site::start_on("[::1]", vec![]); // everything 404s
    let page = format!(
        "<html><body><a href=\"{}/broken\">broken</a></body></html>",
        external.base
    );
    let internal = Site::start(vec![
        (
            "/robots.txt".to_owned(),
            "200 OK".to_owned(),
            String::new(),
            "text/plain".to_owned(),
            ROBOTS.to_owned(),
        ),
        (
            "/".to_owned(),
            "200 OK".to_owned(),
            String::new(),
            "text/html".to_owned(),
            page,
        ),
    ]);
    let work = scratch("failed");
    let scan = work.join("scan.sqlite");
    let answer = handlers::dispatch(
        "crawl_site",
        &json!({
            "url": format!("{}/", internal.base),
            "scan_out": scan.to_string_lossy(),
            "min_delay": 0,
            "overrides": {
                "discovery.external.crawl": true,
                "storage.format_version": "scan.v2",
            },
        }),
    )
    .expect("crawl");
    assert_eq!(answer["finish_reason"], json!("finished"), "{answer}");
    let rows = checks(&scan);
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert_eq!(
        rows[0]["outcome"],
        json!("fetched"),
        "a 404 answer is a fetched probe"
    );
    assert_eq!(rows[0]["status_code"], json!(404));
    let totals = summary(&scan);
    assert_eq!(totals["fetched"], json!(1));
    assert_eq!(totals["destinations"], json!(1));
    let _ = std::fs::remove_dir_all(&work);
}

/// The external phase refuses a scan.v1 artifact instead of silently skipping.
#[test]
fn external_crawl_refuses_a_v1_artifact() {
    let work = scratch("v1");
    let scan = work.join("scan.sqlite");
    let error = handlers::dispatch(
        "crawl_site",
        &json!({
            "url": "http://127.0.0.1:9/",
            "scan_out": scan.to_string_lossy(),
            "overrides": { "discovery.external.crawl": true },
        }),
    )
    .expect_err("refused")
    .to_string();
    assert_eq!(
        error,
        "discovery.external.crawl requires storage.format_version=scan.v2"
    );
    assert!(!scan.exists());
    let _ = std::fs::remove_dir_all(&work);
}

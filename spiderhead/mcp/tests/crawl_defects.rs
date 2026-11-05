//! Large-site crawl behaviour over a loopback origin: sitemap size, request
//! concurrency and recovery from connections the server drops.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use spiderhead_mcp::handlers;

/// A loopback HTTP/1.1 origin that serves keep-alive connections on one thread
/// each, so requests really overlap. `route` maps a path to (type, body).
struct Site {
    base: String,
    hits: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
    connections: Arc<AtomicUsize>,
    /// Requests being answered at the same moment: (now, peak).
    gauge: Arc<(AtomicUsize, AtomicUsize)>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

struct Behavior {
    /// Pause before every response, to model a server with real latency.
    latency: Duration,
    /// Serve this many requests per connection, then close the next one
    /// without answering it (a server recycling its keep-alive connections).
    recycle_after: Option<usize>,
}

type Route = dyn Fn(&str) -> Option<(&'static str, String)> + Send + Sync;

impl Site {
    fn start(
        behavior: Behavior,
        route: impl Fn(&str, &str) -> Option<(&'static str, String)> + Send + Sync + 'static,
    ) -> Site {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let base = format!(
            "http://127.0.0.1:{}",
            listener.local_addr().expect("addr").port()
        );
        let origin = base.clone();
        let route: Arc<Route> = Arc::new(move |path| route(&origin, path));
        let behavior = Arc::new(behavior);
        let hits = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));
        let connections = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let gauge = Arc::new((AtomicUsize::new(0), AtomicUsize::new(0)));
        let thread = {
            let (hits, dropped, connections, stop, gauge) = (
                hits.clone(),
                dropped.clone(),
                connections.clone(),
                stop.clone(),
                gauge.clone(),
            );
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            connections.fetch_add(1, Ordering::Relaxed);
                            let (route, behavior) = (route.clone(), behavior.clone());
                            let (hits, dropped, stop, gauge) =
                                (hits.clone(), dropped.clone(), stop.clone(), gauge.clone());
                            std::thread::spawn(move || {
                                serve(stream, &*route, &behavior, &hits, &dropped, &gauge, &stop)
                            });
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
            })
        };
        Site {
            base,
            hits,
            dropped,
            connections,
            gauge,
            stop,
            thread: Some(thread),
        }
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
    stream: TcpStream,
    route: &Route,
    behavior: &Behavior,
    hits: &AtomicUsize,
    dropped: &AtomicUsize,
    gauge: &(AtomicUsize, AtomicUsize),
    stop: &AtomicBool,
) {
    stream.set_nonblocking(false).expect("blocking stream");
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .ok();
    let mut writer = stream.try_clone().expect("clone stream");
    let mut reader = BufReader::new(stream);
    let mut served = 0usize;
    while !stop.load(Ordering::Relaxed) {
        let mut request = String::new();
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => return,
                Ok(_) => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    if request.is_empty() && !stop.load(Ordering::Relaxed) {
                        continue;
                    }
                    return;
                }
                Err(_) => return,
            }
            let done = line == "\r\n" || line == "\n";
            request.push_str(&line);
            if done {
                break;
            }
        }
        if behavior.recycle_after == Some(served) {
            dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        served += 1;
        hits.fetch_add(1, Ordering::Relaxed);
        let path = request
            .split_whitespace()
            .nth(1)
            .unwrap_or("/")
            .split('?')
            .next()
            .unwrap_or("/")
            .to_owned();
        let now = gauge.0.fetch_add(1, Ordering::SeqCst) + 1;
        gauge.1.fetch_max(now, Ordering::SeqCst);
        std::thread::sleep(behavior.latency);
        gauge.0.fetch_sub(1, Ordering::SeqCst);
        let (status, kind, body) = match route(&path) {
            Some((kind, body)) => ("200 OK", kind, body),
            None => ("404 Not Found", "text/plain", "not found".to_owned()),
        };
        let response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: keep-alive\r\n\r\n{body}",
            body.len()
        );
        if writer.write_all(response.as_bytes()).is_err() {
            return;
        }
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sh-crawl-defects-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn context_count(scan: &Path, kind: &str) -> i64 {
    let con = rusqlite::Connection::open(scan).expect("scan open");
    con.query_row(
        "SELECT COUNT(*) FROM context_items WHERE kind=?",
        [kind],
        |r| r.get(0),
    )
    .expect("context count")
}

const ROBOTS: &str = "User-agent: *\nAllow: /\n";

fn urlset(origin: &str, from: usize, to: usize) -> String {
    let urls: String = (from..to)
        .map(|n| format!("<url><loc>{origin}/u/{n}</loc></url>\n"))
        .collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n{urls}</urlset>\n"
    )
}

fn page(n: usize) -> String {
    format!("<html><head><title>page {n}</title></head><body><h1>page {n}</h1></body></html>")
}

fn crawl(args: Value) -> Value {
    handlers::dispatch("crawl_site", &args).expect("crawl_site")
}

/// A sitemap index of several children totalling more than 3000 URLs: the
/// declared membership is persisted in bounded chunks and never fails the run.
#[test]
fn a_large_sitemap_index_never_fails_the_crawl() {
    const CHILDREN: usize = 4;
    const PER_CHILD: usize = 800;
    let site = Site::start(
        Behavior {
            latency: Duration::ZERO,
            recycle_after: None,
        },
        |origin, path| {
            if path == "/robots.txt" {
                return Some(("text/plain", ROBOTS.to_owned()));
            }
            if path == "/sitemap_index.xml" {
                let children: String = (0..CHILDREN)
                    .map(|c| format!("<sitemap><loc>{origin}/sitemap-{c}.xml</loc></sitemap>\n"))
                    .collect();
                return Some((
                "application/xml",
                format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                     <sitemapindex xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n{children}</sitemapindex>\n"
                ),
            ));
            }
            if let Some(c) = path
                .strip_prefix("/sitemap-")
                .and_then(|p| p.strip_suffix(".xml"))
            {
                let c: usize = c.parse().ok()?;
                return Some((
                    "application/xml",
                    urlset(origin, c * PER_CHILD, (c + 1) * PER_CHILD),
                ));
            }
            if path == "/" {
                return Some(("text/html", page(0)));
            }
            let n = path.strip_prefix("/u/")?.parse().ok()?;
            Some(("text/html", page(n)))
        },
    );
    let work = scratch("sitemap-index");
    let scan = work.join("scan.sqlite");
    let result = crawl(json!({
        "url": format!("{}/", site.base),
        "sitemap": format!("{}/sitemap_index.xml", site.base),
        "max_urls": 5,
        "min_delay": 0,
        "scan_out": scan.to_string_lossy(),
    }));
    assert_eq!(result["finish_reason"], "url_limit", "{result:?}");
    assert_eq!(
        context_count(&scan, "sitemap_declared_url"),
        (CHILDREN * PER_CHILD) as i64,
        "every declared member is retained"
    );
    let _ = std::fs::remove_dir_all(&work);
}

/// With a fixed rate of 10 requests per second and ten requests in flight, a
/// server that needs 300 ms per answer is crawled in parallel, not one by one.
#[test]
fn a_fixed_rate_crawl_runs_requests_in_parallel() {
    const PAGES: usize = 100;
    let site = Site::start(
        Behavior {
            latency: Duration::from_millis(300),
            recycle_after: None,
        },
        |_origin, path| {
            if path == "/robots.txt" {
                return Some(("text/plain", ROBOTS.to_owned()));
            }
            if path == "/" {
                let links: String = (0..PAGES)
                    .map(|n| format!("<a href=\"/p/{n}\">p{n}</a>"))
                    .collect();
                return Some((
                    "text/html",
                    format!("<html><head><title>home</title></head><body>{links}</body></html>"),
                ));
            }
            let n = path.strip_prefix("/p/")?.parse().ok()?;
            Some(("text/html", page(n)))
        },
    );
    let work = scratch("parallel");
    let scan = work.join("scan.sqlite");
    let started = std::time::Instant::now();
    let result = crawl(json!({
        "url": format!("{}/", site.base),
        "max_urls": 500,
        "min_delay": 0.1,
        "overrides": {"speed.adaptive": false, "speed.concurrency": 10},
        "scan_out": scan.to_string_lossy(),
    }));
    let elapsed = started.elapsed().as_secs_f64();
    assert_eq!(result["finish_reason"], "finished", "{result:?}");
    assert_eq!(result["urls_collected"], json!(PAGES + 1));
    assert!(
        elapsed <= 15.0,
        "{} pages took {elapsed:.1}s; requests were sequential",
        PAGES + 1
    );
    assert!(
        site.gauge.1.load(Ordering::SeqCst) >= 2,
        "requests never overlapped (elapsed {elapsed:.1}s)"
    );
    let _ = std::fs::remove_dir_all(&work);
}

/// A server that recycles its keep-alive connections by closing one without
/// answering: the affected URLs are fetched again on a new connection and are
/// neither page errors nor a reason to stop the crawl.
#[test]
fn dropped_connections_are_reopened_and_not_counted_as_errors() {
    const PAGES: usize = 60;
    let site = Site::start(
        Behavior {
            latency: Duration::ZERO,
            recycle_after: Some(5),
        },
        |_origin, path| {
            if path == "/robots.txt" {
                return Some(("text/plain", ROBOTS.to_owned()));
            }
            if path == "/" {
                let links: String = (0..PAGES)
                    .map(|n| format!("<a href=\"/p/{n}\">p{n}</a>"))
                    .collect();
                return Some((
                    "text/html",
                    format!("<html><head><title>home</title></head><body>{links}</body></html>"),
                ));
            }
            let n = path.strip_prefix("/p/")?.parse().ok()?;
            Some(("text/html", page(n)))
        },
    );
    let work = scratch("recycle");
    let scan = work.join("scan.sqlite");
    let result = crawl(json!({
        "url": format!("{}/", site.base),
        "max_urls": 500,
        "min_delay": 0,
        "overrides": {"speed.adaptive": false, "speed.concurrency": 4},
        "scan_out": scan.to_string_lossy(),
    }));
    assert_eq!(result["finish_reason"], "finished", "{result:?}");
    assert_eq!(result["urls_collected"], json!(PAGES + 1));
    assert!(
        site.dropped.load(Ordering::Relaxed) > 0,
        "the server never dropped a connection"
    );
    assert!(site.connections.load(Ordering::Relaxed) > 1);
    let con = rusqlite::Connection::open(&scan).expect("scan open");
    let failed: i64 = con
        .query_row(
            "SELECT COUNT(*) FROM pages WHERE error_kind!='' OR status_code IS NULL OR status_code!=200",
            [],
            |r| r.get(0),
        )
        .expect("failed pages");
    assert_eq!(failed, 0, "dropped connections leaked into page records");
    let _ = std::fs::remove_dir_all(&work);
}

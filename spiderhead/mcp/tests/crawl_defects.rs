//! Large-site crawl behaviour over a loopback origin: sitemap size, request
//! concurrency and recovery from connections the server drops.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
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
        let thread = {
            let (hits, dropped, connections, stop) = (
                hits.clone(),
                dropped.clone(),
                connections.clone(),
                stop.clone(),
            );
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            connections.fetch_add(1, Ordering::Relaxed);
                            let (route, behavior) = (route.clone(), behavior.clone());
                            let (hits, dropped, stop) =
                                (hits.clone(), dropped.clone(), stop.clone());
                            std::thread::spawn(move || {
                                serve(stream, &*route, &behavior, &hits, &dropped, &stop)
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
        std::thread::sleep(behavior.latency);
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

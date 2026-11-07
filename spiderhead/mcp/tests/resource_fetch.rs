//! `resources.fetch` on the native route: a scan.v2 crawl fetches every
//! declared resource once — images, srcset candidates, posters, video
//! sources, stylesheets, scripts, PDFs, and the `@font-face`/`url()` children
//! of a fetched stylesheet — records each exchange, and excludes what the
//! scope rejects.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use spiderhead_mcp::handlers;

/// A loopback HTTP origin: path → (content-type, body), every requested path
/// recorded with a hit count for the fetched-once assertions.
struct Site {
    base: String,
    hits: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Site {
    fn start(routes: Vec<(String, String, String)>) -> Site {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let port = listener.local_addr().expect("local addr").port();
        let base = format!("http://127.0.0.1:{port}");
        let routes: std::collections::HashMap<String, (String, String)> = routes
            .into_iter()
            .map(|(path, kind, body)| (path, (kind, body)))
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
        Some((kind, body)) => ("200 OK", kind.clone(), body.clone()),
        None => (
            "404 Not Found",
            "text/plain".to_owned(),
            "not found".to_owned(),
        ),
    };
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let mut body = body.as_bytes();
    let _ = std::io::copy(&mut body, stream);
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

const ROBOTS: &str = "User-agent: *\nAllow: /\n";

const PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89,
];

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .canonicalize()
        .unwrap_or_else(|_| std::env::temp_dir())
        .join(format!("sh-resource-fetch-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// (state, status_code) of one resolved URL in the resource graph fetches.
fn fetch_row(scan: &std::path::Path, url: &str) -> (String, Option<i64>) {
    let con = rusqlite::Connection::open(scan).expect("scan open");
    con.query_row(
        "SELECT state,status_code FROM resource_graph_fetches WHERE resolved_url=?",
        [url],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .expect("fetch row")
}

/// The state of every occurrence of one resolved URL.
fn occurrence_states(scan: &std::path::Path, url: &str) -> Vec<String> {
    let con = rusqlite::Connection::open(scan).expect("scan open");
    let mut stmt = con
        .prepare(
            "SELECT state FROM resource_graph_occurrences WHERE resolved_url=? ORDER BY occurrence_id",
        )
        .expect("occurrences");
    stmt.query_map([url], |r| r.get::<_, String>(0))
        .expect("occurrence rows")
        .collect::<Result<Vec<String>, _>>()
        .expect("occurrence states")
}

#[test]
fn every_declared_resource_is_fetched_once_with_its_status() {
    let site = Site::start(vec![
        (
            "/robots.txt".to_owned(),
            "text/plain".to_owned(),
            ROBOTS.to_owned(),
        ),
        (
            "/page".to_owned(),
            "text/html".to_owned(),
            "<html><head>\
             <link rel=\"stylesheet\" href=\"/style.css\">\
             <script src=\"/app.js\"></script></head><body>\
             <img src=\"/img.png\" srcset=\"/img2.png 2x, /img3.png 3x\">\
             <video poster=\"/poster.jpg\"><source src=\"/clip.mp4\"></video>\
             <a href=\"/doc.pdf\">manual</a>\
             <img src=\"http://other.test/external.png\">\
             </body></html>"
                .to_owned(),
        ),
        (
            "/style.css".to_owned(),
            "text/css".to_owned(),
            "@font-face { font-family: X; src: url(/font.woff2); } \
             body { background: url(/bg.png); }"
                .to_owned(),
        ),
        (
            "/app.js".to_owned(),
            "text/javascript".to_owned(),
            "1;".to_owned(),
        ),
        (
            "/img.png".to_owned(),
            "image/png".to_owned(),
            String::from_utf8_lossy(PNG).into_owned(),
        ),
        (
            "/img2.png".to_owned(),
            "image/png".to_owned(),
            String::from_utf8_lossy(PNG).into_owned(),
        ),
        (
            "/img3.png".to_owned(),
            "image/png".to_owned(),
            String::from_utf8_lossy(PNG).into_owned(),
        ),
        (
            "/poster.jpg".to_owned(),
            "image/jpeg".to_owned(),
            "JPEGDATA".to_owned(),
        ),
        (
            "/clip.mp4".to_owned(),
            "video/mp4".to_owned(),
            "MP4DATA".to_owned(),
        ),
        (
            "/font.woff2".to_owned(),
            "font/woff2".to_owned(),
            "FONT".to_owned(),
        ),
        (
            "/bg.png".to_owned(),
            "image/png".to_owned(),
            String::from_utf8_lossy(PNG).into_owned(),
        ),
        (
            "/doc.pdf".to_owned(),
            "application/pdf".to_owned(),
            "%PDF-1.4".to_owned(),
        ),
    ]);
    let work = scratch("all-kinds");
    let scan = work.join("scan.sqlite");

    let answer = handlers::dispatch(
        "crawl_site",
        &json!({
            "url": format!("{}/page", site.base),
            "scan_out": scan.to_string_lossy(),
            "min_delay": 0,
            "overrides": {
                "resources.fetch": true,
                "storage.format_version": "scan.v2",
            },
        }),
    )
    .expect("crawl");
    assert_eq!(answer["finish_reason"], json!("finished"), "{answer}");

    // Every same-origin declaration was fetched exactly once and recorded 200.
    for path in [
        "/style.css",
        "/app.js",
        "/img.png",
        "/img2.png",
        "/img3.png",
        "/poster.jpg",
        "/clip.mp4",
        "/font.woff2",
        "/bg.png",
        "/doc.pdf",
    ] {
        let url = format!("{}{}", site.base, path);
        let (state, status) = fetch_row(&scan, &url);
        assert_eq!(state, "complete", "{path}: {status:?}");
        assert_eq!(status, Some(200), "{path}");
        assert_eq!(site.hit_count(path), 1, "{path} fetched exactly once");
    }

    // The stylesheet's children arrived one nesting level deeper.
    let con = rusqlite::Connection::open(&scan).expect("scan open");
    let (font_kind, font_depth): (String, i64) = con
        .query_row(
            "SELECT kind,nesting_depth FROM resource_graph_occurrences WHERE resolved_url LIKE '%/font.woff2'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("font occurrence");
    assert_eq!(font_kind, "font");
    assert_eq!(font_depth, 1, "a css child declaration");

    // The out-of-scope image was never requested; its occurrence is excluded.
    assert_eq!(site.hit_count("/external.png"), 0, "external never fetched");
    assert_eq!(
        occurrence_states(&scan, "http://other.test/external.png"),
        vec!["excluded".to_owned()],
        "external occurrence excluded"
    );

    // The page itself was crawled once; no discovery happened beyond it.
    assert_eq!(site.hit_count("/page"), 1);
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn resources_fetch_refuses_a_v1_artifact() {
    let work = scratch("v1-refusal");
    let scan = work.join("scan.sqlite");
    let error = handlers::dispatch(
        "crawl_site",
        &json!({
            "url": "http://127.0.0.1:9/",
            "scan_out": scan.to_string_lossy(),
            "overrides": { "resources.fetch": true },
        }),
    )
    .expect_err("refused")
    .to_string();
    assert_eq!(
        error,
        "resources.fetch requires storage.format_version=scan.v2"
    );
    assert!(!scan.exists());
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn a_broken_resource_records_its_status_and_the_crawl_finishes() {
    let site = Site::start(vec![
        (
            "/robots.txt".to_owned(),
            "text/plain".to_owned(),
            ROBOTS.to_owned(),
        ),
        (
            "/page".to_owned(),
            "text/html".to_owned(),
            "<html><body><img src=\"/gone.png\"></body></html>".to_owned(),
        ),
    ]);
    let work = scratch("missing");
    let scan = work.join("scan.sqlite");
    let answer = handlers::dispatch(
        "crawl_site",
        &json!({
            "url": format!("{}/page", site.base),
            "scan_out": scan.to_string_lossy(),
            "min_delay": 0,
            "overrides": {
                "resources.fetch": true,
                "storage.format_version": "scan.v2",
            },
        }),
    )
    .expect("crawl");
    assert_eq!(answer["finish_reason"], json!("finished"), "{answer}");
    let (state, status) = fetch_row(&scan, &format!("{}/gone.png", site.base));
    assert_eq!(state, "failed", "the 404 is a failed resource");
    assert_eq!(status, Some(404));
    assert_eq!(
        occurrence_states(&scan, &format!("{}/gone.png", site.base)),
        vec!["failed".to_owned()]
    );
    let _ = std::fs::remove_dir_all(&work);
}

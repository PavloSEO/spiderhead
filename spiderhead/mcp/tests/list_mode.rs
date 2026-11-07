//! `crawl-site` URL-list modes over a loopback origin: `--urls` and
//! `--urls-file` crawl exactly the declared list with no discovery, every
//! container shape seeds the same list, and a resume of an interrupted list
//! scan completes the list without starting to spider.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};
use spiderhead_mcp::handlers;

/// A loopback HTTP origin: path → (content-type, body) routes, every requested
/// path recorded for the no-discovery assertions.
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

    /// Paths requested so far, in order.
    fn hit_paths(&self) -> Vec<String> {
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

fn link_page(href: &str) -> String {
    format!("<html><body><a href=\"{href}\">next</a></body></html>")
}

/// The site every test below crawls: `/` links to `/a` and `/b`, `/a` links to
/// `/b`, `/b` links to `/c` — a list of `/` and `/a` must never reach `/b`.
fn site() -> Site {
    Site::start(vec![
        (
            "/robots.txt".to_owned(),
            "text/plain".to_owned(),
            ROBOTS.to_owned(),
        ),
        (
            "/".to_owned(),
            "text/html".to_owned(),
            "<html><body><a href=\"/a\">a</a><a href=\"/b\">b</a></body></html>".to_owned(),
        ),
        ("/a".to_owned(), "text/html".to_owned(), link_page("/b")),
        ("/b".to_owned(), "text/html".to_owned(), link_page("/c")),
        (
            "/c".to_owned(),
            "text/html".to_owned(),
            "<html><body>leaf</body></html>".to_owned(),
        ),
    ])
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sh-list-mode-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn crawl(args: Value) -> Result<Value, String> {
    handlers::dispatch("crawl_site", &args).map_err(|e| e.to_string())
}

fn document_count(scan: &Path) -> i64 {
    let con = rusqlite::Connection::open(scan).expect("scan open");
    con.query_row("SELECT COUNT(*) FROM documents", [], |r| r.get(0))
        .expect("documents")
}

/// The list mode crawls exactly the declared URLs: pages linked from listed
/// pages are never requested.
#[test]
fn list_mode_crawls_exactly_the_listed_urls() {
    let site = site();
    let work = scratch("exact");
    let scan = work.join("scan.sqlite");
    let answer = crawl(json!({
        "urls": [format!("{}/", site.base), format!("{}/a", site.base)],
        "scan_out": scan.to_string_lossy(),
        "min_delay": 0,
    }))
    .expect("crawl");
    assert_eq!(answer["finish_reason"], json!("finished"), "{answer}");
    assert_eq!(document_count(&scan), 2, "exactly the listed pages");
    let hits = site.hit_paths();
    assert!(hits.contains(&"/".to_owned()), "{hits:?}");
    assert!(hits.contains(&"/a".to_owned()), "{hits:?}");
    assert!(!hits.contains(&"/b".to_owned()), "no discovery: {hits:?}");
    assert_eq!(
        hits.iter().filter(|path| *path != "/robots.txt").count(),
        2,
        "{hits:?}"
    );
    let _ = std::fs::remove_dir_all(&work);
}

/// The CLI's comma-separated `--urls` spelling seeds the same list.
#[test]
fn the_cli_comma_spelling_seeds_the_same_list() {
    let site = site();
    let work = scratch("comma");
    let scan = work.join("scan.sqlite");
    let answer = crawl(json!({
        "urls": format!("{}/, {}/a", site.base, site.base),
        "scan_out": scan.to_string_lossy(),
        "min_delay": 0,
    }))
    .expect("crawl");
    assert_eq!(answer["finish_reason"], json!("finished"), "{answer}");
    assert_eq!(document_count(&scan), 2);
    assert!(!site.hit_paths().contains(&"/b".to_owned()));
    let _ = std::fs::remove_dir_all(&work);
}

/// Every `--urls-file` container (TXT, CSV, XLSX, XML) carries the same list.
#[test]
fn every_urls_file_container_seeds_the_same_list() {
    let site = site();
    let work = scratch("containers");
    let first = format!("{}/", site.base);
    let second = format!("{}/a", site.base);

    let txt = work.join("list.txt");
    std::fs::write(&txt, format!("# comment\n\n{first}\n{second}\n")).expect("txt");
    let csv = work.join("list.csv");
    std::fs::write(&csv, format!("url,note\n{first},home\n{second},\n")).expect("csv");
    let xml = work.join("list.xml");
    std::fs::write(
        &xml,
        format!(
            "<?xml version=\"1.0\"?>\n<urlset><url><loc>{first}</loc></url>\
             <url><loc>{second}</loc></url></urlset>"
        ),
    )
    .expect("xml");
    let xlsx = work.join("list.xlsx");
    {
        use rust_xlsxwriter::Workbook;
        let mut book = Workbook::new();
        let sheet = book.add_worksheet();
        sheet.write_string(0, 0, "note").expect("header a");
        sheet.write_string(0, 1, "url").expect("header b");
        sheet.write_string(1, 0, "home").expect("row a");
        sheet.write_string(1, 1, &first).expect("row b");
        sheet.write_string(2, 0, "").expect("row c");
        sheet.write_string(2, 1, &second).expect("row d");
        book.save(&xlsx).expect("xlsx");
    }

    for path in [&txt, &csv, &xml, &xlsx] {
        let scan = work.join(format!(
            "{}.sqlite",
            path.extension().unwrap().to_string_lossy()
        ));
        let answer = crawl(json!({
            "urls_file": path.to_string_lossy(),
            "scan_out": scan.to_string_lossy(),
            "min_delay": 0,
        }))
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(answer["finish_reason"], json!("finished"), "{answer}");
        assert_eq!(document_count(&scan), 2, "{}", path.display());
    }
    let hits = site.hit_paths();
    assert!(!hits.contains(&"/b".to_owned()), "{hits:?}");
    let _ = std::fs::remove_dir_all(&work);
}

/// A resume of an interrupted list scan completes the list and does not start
/// to spider: the stored list input is the whole population on resume too.
#[test]
fn a_resume_completes_a_list_scan_without_discovery() {
    use spiderhead_core::crawl_settings::{self, LoadInput};
    use spiderhead_core::host::SystemHost;
    use spiderhead_crawl::oracle;
    use spiderhead_store::scan_lease::NativeScanLease;
    use spiderhead_store::scan_schema::NativeScanSchemaMixin;

    let site = site();
    let work = scratch("resume");
    let scan_path = work.join("scan.sqlite");
    let urls = vec![
        format!("{}/", site.base),
        format!("{}/a", site.base),
        format!("{}/b", site.base),
    ];

    // The artifact a killed list crawl leaves behind: the list stored as the
    // scan's input marker, every entry still queued in the frontier.
    let settings = crawl_settings::load(
        &LoadInput {
            file: None,
            overrides: Map::new(),
            base_overrides: Map::new(),
        },
        &SystemHost,
    )
    .expect("settings");
    let settings = settings.as_object().cloned().expect("settings object");
    let mut scan = NativeScanSchemaMixin::create(
        &scan_path,
        &urls[0],
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
    let payload = json!({ "urls": urls });
    scan.write_context(&[json!({
        "kind": "list_urls",
        "item_key": "run",
        "payload_version": "scan_context.v1",
        "payload_json": spiderhead_store::discovery_ledger::py_dumps(&payload, false, true),
        "completeness": "complete",
        "reason": "",
    })])
    .expect("list marker");
    let entries: Vec<Map<String, Value>> = urls
        .iter()
        .map(|url| {
            let mut entry = Map::new();
            entry.insert("requested_url".into(), json!(url));
            entry.insert("frontier_url".into(), json!(url));
            entry.insert("depth".into(), json!(0));
            entry.insert("reason".into(), json!(""));
            entry.insert("source".into(), json!("sitemap"));
            entry.insert("reserve_query".into(), json!(true));
            entry.insert("seed".into(), json!(true));
            entry
        })
        .collect();
    scan.seed_frontier(&entries).expect("seed");
    scan.interrupt("killed").expect("interrupt");
    scan.close();

    let answer = crawl(json!({
        "resume": scan_path.to_string_lossy(),
    }))
    .expect("resume");
    assert_eq!(answer["finish_reason"], json!("finished"), "{answer}");
    assert_eq!(answer["resumed"], json!(true), "{answer}");
    assert_eq!(document_count(&scan_path), 3, "the whole list crawled");
    let hits = site.hit_paths();
    assert!(!hits.contains(&"/c".to_owned()), "no discovery: {hits:?}");
    let _ = std::fs::remove_dir_all(&work);
}

/// A list longer than the configured URL budget is refused before any request.
#[test]
fn a_list_over_the_budget_is_refused() {
    let site = site();
    let work = scratch("budget");
    let scan = work.join("scan.sqlite");
    let error = crawl(json!({
        "urls": [
            format!("{}/", site.base),
            format!("{}/a", site.base),
            format!("{}/b", site.base),
        ],
        "scan_out": scan.to_string_lossy(),
        "max_urls": 2,
    }))
    .expect_err("refused");
    assert_eq!(error, "url list exceeds the configured URL budget");
    let _ = std::fs::remove_dir_all(&work);
}

/// Input-shape refusals: the two list inputs are mutually exclusive, sitemap
/// inputs do not combine with a list, entries must be absolute http(s) URLs,
/// and a stated `url` must be the first list entry.
#[test]
fn list_mode_input_shapes_are_refused() {
    let site = site();
    let work = scratch("shapes");
    let scan = work.join("scan.sqlite").to_string_lossy().into_owned();
    let base = site.base.clone();

    let error = crawl(json!({
        "urls": [format!("{}/", base)],
        "urls_file": format!("{}/nope.txt", work.display()),
        "scan_out": &scan,
    }))
    .expect_err("refused");
    assert_eq!(
        error,
        "crawl-site urls and urls_file are mutually exclusive"
    );

    let error = crawl(json!({
        "urls": [format!("{}/", base)],
        "sitemap": format!("{}/sitemap.xml", base),
        "scan_out": &scan,
    }))
    .expect_err("refused");
    assert_eq!(
        error,
        "crawl-site list mode does not combine with sitemap inputs"
    );

    let error = crawl(json!({
        "urls": ["/relative/page"],
        "scan_out": &scan,
    }))
    .expect_err("refused");
    assert_eq!(
        error,
        "url list entries must be absolute http(s) URLs: /relative/page"
    );

    let error = crawl(json!({
        "urls": [format!("{}/", base), format!("{}/a", base)],
        "url": format!("{}/a", base),
        "scan_out": &scan,
    }))
    .expect_err("refused");
    assert_eq!(
        error,
        "crawl-site url must be the first entry of the url list"
    );

    let empty = work.join("empty.txt");
    std::fs::write(&empty, "# only a comment\n").expect("empty list");
    let error = crawl(json!({
        "urls_file": empty.to_string_lossy(),
        "scan_out": &scan,
    }))
    .expect_err("refused");
    assert_eq!(
        error,
        format!("urls_file {} declared no URLs", empty.to_string_lossy())
    );
    let _ = std::fs::remove_dir_all(&work);
}

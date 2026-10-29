//! `crawl-site` stderr contract over a scripted loopback origin: the
//! stored-frontier notice on resume, the worst-case rate line on a fresh run,
//! and the outcome line (`stopped early` / `finished`, the `--resume` hint
//! only for stops a resume can pass) — the reference suite.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};
use spiderhead_cli::{registry, run, Output};
use spiderhead_core::crawl_settings::{self, LoadInput};
use spiderhead_core::host::SystemHost;
use spiderhead_crawl::oracle;
use spiderhead_store::scan_lease::NativeScanLease;
use spiderhead_store::scan_schema::NativeScanSchemaMixin;

/// A loopback HTTP origin: path → (content-type, body) routes, every request
/// path recorded for the spidering assertions.
struct Site {
    base: String,
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

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sh-cli-resume-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// An artifact in exactly the state a killed crawl leaves behind: the start
/// URL still queued in the frontier (reference `_interrupted`).
fn interrupted_scan(scan_path: &Path, start_url: &str) {
    let settings_value = crawl_settings::load(
        &LoadInput {
            file: None,
            overrides: Map::new(),
            base_overrides: Map::new(),
        },
        &SystemHost,
    )
    .expect("settings");
    let settings = settings_value.as_object().expect("settings object").clone();
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
        &[],
        "scan.v1",
    )
    .expect("scan create");
    let mut entry = Map::new();
    entry.insert("requested_url".into(), json!(start_url));
    entry.insert("frontier_url".into(), json!(start_url));
    entry.insert("depth".into(), json!(0));
    entry.insert("reason".into(), json!(""));
    entry.insert("source".into(), json!("start"));
    entry.insert("reserve_query".into(), json!(false));
    entry.insert("seed".into(), json!(false));
    scan.seed_frontier(&[entry]).expect("seed");
    scan.interrupt("killed").expect("interrupt");
    scan.close();
}

fn run_cli(argv: &[String]) -> (i32, Output) {
    let tree = registry();
    let mut out = Output::default();
    let code = run(argv, &tree, &mut out).expect("cli run");
    (i32::from(code.0), out)
}

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

#[test]
fn cli_resume_loads_the_stored_frontier_and_names_the_lines() {
    let site = Site::start(vec![
        (
            "/robots.txt".to_owned(),
            "text/plain".to_owned(),
            ROBOTS.to_owned(),
        ),
        ("/".to_owned(), "text/html".to_owned(), link_page("/a")),
        ("/a".to_owned(), "text/html".to_owned(), link_page("/b")),
        (
            "/b".to_owned(),
            "text/html".to_owned(),
            "<html><body>leaf</body></html>".to_owned(),
        ),
    ]);
    let work = scratch("resume");
    let scan_path = work.join("scan.sqlite");
    interrupted_scan(&scan_path, &format!("{}/", site.base));
    let scan_text = scan_path.to_string_lossy().into_owned();

    let (code, out) = run_cli(&argv(&["crawl-site", "--resume", &scan_text]));

    assert_eq!(code, 0, "stderr: {}", out.stderr);
    // The frontier notice rides along because a resumed crawl is exactly the
    // long run worth watching; the pre-run rate line describes flags a resume
    // refuses, so it is not printed.
    let expected = format!(
        "seohead: crawl-site\nresuming {scan_text} (0 MB): loading the stored frontier...\n\
         crawl-site: finished; 3 URLs fetched (this run continued an earlier one)\n"
    );
    assert_eq!(out.stderr, expected, "stderr mismatch");
    assert!(!out.stderr.contains("worst-case request rate"));
    let document: Value = serde_json::from_str(&out.stdout).expect("stdout json");
    assert_eq!(document["resumed"], json!(true));
    assert_eq!(document["partial"], json!(false));

    // A fresh run on the same shape prints the rate the flags resolved to,
    // then the same outcome line — without the continued-earlier marker.
    let fresh_path = work.join("fresh.sqlite");
    let fresh_text = fresh_path.to_string_lossy().into_owned();
    let (code, out) = run_cli(&argv(&[
        "crawl-site",
        "--url",
        &format!("{}/", site.base),
        "--scan-out",
        &fresh_text,
        "--min-delay",
        "0",
    ]));
    assert_eq!(code, 0, "stderr: {}", out.stderr);
    assert!(out
        .stderr
        .contains("crawl-site: effective worst-case request rate to one host: unbounded"));
    assert!(out.stderr.contains("crawl-site: finished; 3 URLs fetched"));
    assert!(!out.stderr.contains("continued an earlier one"));
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn cli_says_a_crawl_stopped_early_and_how_to_continue_it() {
    // An origin whose robots.txt cannot be fetched stops before the first
    // page when the robots policy is `respect`.
    let work = scratch("stopped");
    let scan_path = work.join("scan.sqlite");
    let scan_text = scan_path.to_string_lossy().into_owned();
    let (code, out) = run_cli(&argv(&[
        "crawl-site",
        "--url",
        "http://127.0.0.1:9/",
        "--scan-out",
        &scan_text,
    ]));
    assert_eq!(code, 0, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("crawl-site: stopped early ("),
        "{}",
        out.stderr
    );
    assert!(out.stderr.contains("URLs fetched"));
    assert!(
        out.stderr
            .contains(&format!("seohead crawl-site --resume {scan_text}")),
        "{}",
        out.stderr
    );
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn cli_does_not_offer_a_resume_for_a_budget_stop() {
    let site = Site::start(vec![
        (
            "/robots.txt".to_owned(),
            "text/plain".to_owned(),
            ROBOTS.to_owned(),
        ),
        ("/".to_owned(), "text/html".to_owned(), link_page("/a")),
        (
            "/a".to_owned(),
            "text/html".to_owned(),
            "<html><body>leaf</body></html>".to_owned(),
        ),
    ]);
    let work = scratch("budget");
    let scan_path = work.join("scan.sqlite");
    let scan_text = scan_path.to_string_lossy().into_owned();
    let (code, out) = run_cli(&argv(&[
        "crawl-site",
        "--url",
        &format!("{}/", site.base),
        "--scan-out",
        &scan_text,
        "--max-urls",
        "1",
        "--min-delay",
        "0",
    ]));
    assert_eq!(code, 0, "stderr: {}", out.stderr);
    assert!(out.stderr.contains("stopped early"), "{}", out.stderr);
    // A resume reads its limits back from the artifact, so it cannot get past
    // them: the setting is named, --resume is not offered.
    assert!(out.stderr.contains("limits.max_urls"), "{}", out.stderr);
    assert!(!out.stderr.contains("--resume"), "{}", out.stderr);
    let document: Value = serde_json::from_str(&out.stdout).expect("stdout json");
    assert_eq!(document["finish_reason"], json!("url_limit"));
    let _ = std::fs::remove_dir_all(&work);
}

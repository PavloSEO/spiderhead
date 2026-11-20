//! `crawl-site --urls` list mode over a loopback origin: a comma-separated
//! list queues every URL and the URL budget counts crawled pages honestly —
//! the whole list crawls when the cap is above it (issue #95).

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use spiderhead_cli::{registry, run, Output};

/// A loopback HTTP origin: path → (content-type, body) routes, requests recorded.
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
            let hits = Arc::clone(&hits);
            let stop = Arc::clone(&stop);
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

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sh-cli-urls-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
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

/// Six comma-listed URLs under a URL cap above the list: every listed page is
/// crawled and the run finishes — the queued list itself is not counted
/// against the budget twice.
#[test]
fn a_comma_separated_url_list_crawls_every_url_under_the_cap() {
    const PAGES: usize = 6;
    let mut routes = vec![(
        "/robots.txt".to_owned(),
        "text/plain".to_owned(),
        ROBOTS.to_owned(),
    )];
    for n in 0..PAGES {
        let path = if n == 0 {
            "/".to_owned()
        } else {
            format!("/p{n}")
        };
        routes.push((
            path,
            "text/html".to_owned(),
            "<html><body>leaf</body></html>".to_owned(),
        ));
    }
    let site = Site::start(routes);
    let list: Vec<String> = (0..PAGES)
        .map(|n| {
            if n == 0 {
                format!("{}/", site.base)
            } else {
                format!("{}/p{n}", site.base)
            }
        })
        .collect();

    let work = scratch("comma");
    let scan_path = work.join("scan.sqlite");
    let (code, out) = run_cli(&argv(&[
        "crawl-site",
        "--urls",
        &list.join(","),
        "--scan-out",
        &scan_path.to_string_lossy(),
        "--max-urls",
        "8",
        "--min-delay",
        "0",
    ]));

    assert_eq!(code, 0, "stderr: {}", out.stderr);
    let document: Value = serde_json::from_str(&out.stdout).expect("stdout json");
    assert_eq!(document["finish_reason"], json!("finished"), "{document}");
    assert_eq!(document["partial"], json!(false), "{document}");
    assert_eq!(
        document["urls_collected"],
        json!(PAGES),
        "every listed URL crawled: {document}"
    );
    let hits = site.hit_paths();
    for n in 1..PAGES {
        assert!(hits.contains(&format!("/p{n}")), "hits: {hits:?}");
    }
    let _ = std::fs::remove_dir_all(&work);
}

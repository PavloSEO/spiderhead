//! `scan-body-read --input` inline JSON: the command's options are named
//! `scan`/`url` in the help, so a `--input` object with those keys must work
//! without restating them as flags (issue #97).

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use spiderhead_cli::{registry, run, Output};

/// A loopback HTTP origin serving one page with a readable body.
struct Site {
    base: String,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Site {
    fn start() -> Site {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let port = listener.local_addr().expect("local addr").port();
        let base = format!("http://127.0.0.1:{port}");
        let routes: std::collections::HashMap<String, (String, String)> = [(
            "/".to_owned(),
            (
                "text/html".to_owned(),
                "<html><head><title>home</title></head><body>readable body marker</body></html>"
                    .to_owned(),
            ),
        )]
        .into_iter()
        .collect();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = Arc::clone(&stop);
            let routes = routes.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            stream.set_nonblocking(false).expect("blocking stream");
                            stream
                                .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                                .expect("read timeout");
                            serve(&mut stream, &routes);
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
    let (kind, body) = routes
        .get(&path)
        .cloned()
        .unwrap_or_else(|| ("text/plain".to_owned(), "not found".to_owned()));
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let mut body = body.as_bytes();
    let _ = std::io::copy(&mut body, stream);
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sh-cli-sbr-{name}-{}", std::process::id()));
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

/// Retain one page, then read its body twice: once through the inline
/// `--input` JSON with the documented `scan`/`url` keys, once through the
/// classic `--scan`/`--url` flags. Both spellings answer the same body.
#[test]
fn inline_input_json_names_the_scan_and_url_keys() {
    let site = Site::start();
    let work = scratch("inline");
    let scan_path = work.join("scan.sqlite");
    let scan = scan_path.to_string_lossy().into_owned();
    let home = format!("{}/", site.base);

    let (code, out) = run_cli(&argv(&[
        "crawl-site",
        "--urls",
        &home,
        "--scan-out",
        &scan,
        "--min-delay",
        "0",
    ]));
    assert_eq!(code, 0, "stderr: {}", out.stderr);

    let input = json!({"scan": &scan, "url": &home}).to_string();
    let (code, out) = run_cli(&argv(&["scan-body-read", "--input", &input]));
    assert_eq!(code, 0, "stderr: {}", out.stderr);
    let document: Value = serde_json::from_str(&out.stdout).expect("stdout json");
    assert_eq!(document["ok"], json!(true), "{document}");
    assert_eq!(document["state"], json!("available"), "{document}");
    assert!(
        document["text"]
            .as_str()
            .is_some_and(|text| text.contains("readable body marker")),
        "{document}"
    );

    // The flag spelling keeps working, and a flag wins over the JSON base.
    let (code, out) = run_cli(&argv(&[
        "scan-body-read",
        "--input",
        &json!({"url": &home}).to_string(),
        "--scan",
        &scan,
        "--url",
        &home,
    ]));
    assert_eq!(code, 0, "stderr: {}", out.stderr);
    let document: Value = serde_json::from_str(&out.stdout).expect("stdout json");
    assert_eq!(document["state"], json!("available"), "{document}");
    let _ = std::fs::remove_dir_all(&work);
}

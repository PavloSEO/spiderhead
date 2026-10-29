//! Paused-artifact steering through `crawl-control --scan` (#1485): the two
//! offline actions (`narrow-scope`, `add-urls`) and the refusals that keep the
//! route steering-only — the reference suite, the paused path.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use spiderhead_cli::{registry, run, Output};
use spiderhead_core::crawl_settings::{self, LoadInput};
use spiderhead_core::host::SystemHost;
use spiderhead_crawl::oracle;
use spiderhead_store::scan_lease::NativeScanLease;
use spiderhead_store::scan_schema::NativeScanSchemaMixin;

const START: &str = "http://example.test/";

/// The way a crawl leaves its artifact: start queued, then interrupted.
fn paused_scan(path: &Path) {
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
        path,
        START,
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
    entry.insert("requested_url".into(), json!(START));
    entry.insert("frontier_url".into(), json!(START));
    entry.insert("depth".into(), json!(0));
    entry.insert("reason".into(), json!(""));
    entry.insert("source".into(), json!("start"));
    entry.insert("reserve_query".into(), json!(false));
    entry.insert("seed".into(), json!(false));
    scan.seed_frontier(&[entry]).expect("seed");
    scan.interrupt("paused").expect("interrupt");
    scan.close();
}

fn queue(path: &Path, urls: &[&str]) {
    let mut scan = NativeScanSchemaMixin::open(path, None, None, None, false).expect("open");
    let entries: Vec<(String, i64)> = urls.iter().map(|u| ((*u).to_owned(), 1)).collect();
    scan.enqueue(&entries, "enqueue").expect("queue");
    scan.close();
}

/// `url suffix → frontier state`, suffixes relative to the start URL.
fn states(path: &Path) -> std::collections::BTreeMap<String, String> {
    let mut scan = NativeScanSchemaMixin::open(path, None, None, None, false).expect("open");
    let mut out = std::collections::BTreeMap::new();
    let rows: Vec<(String, String)> = {
        let mut statement = scan
            .con()
            .prepare("SELECT u.url, f.state FROM frontier f JOIN urls u USING(url_id)")
            .expect("frontier query");
        let mut rows = statement.query([]).expect("rows");
        let mut collected = Vec::new();
        while let Some(row) = rows.next().expect("row") {
            collected.push((row.get(0).expect("url"), row.get(1).expect("state")));
        }
        collected
    };
    for (url, state) in rows {
        let suffix = url.strip_prefix(START).unwrap_or(&url).to_owned();
        out.insert(
            if suffix.is_empty() {
                "/".to_owned()
            } else {
                suffix
            },
            state,
        );
    }
    scan.close();
    out
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sh-control-scan-{name}-{}", std::process::id()));
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

/// One request-answer cycle: the routed page, or a 404 for anything else.
fn serve_once(
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

#[test]
fn crawl_control_narrows_a_paused_artifact_and_reports_the_drop_count() {
    let work = scratch("narrow");
    let path = work.join("paused.sqlite");
    paused_scan(&path);
    queue(&path, &[&format!("{START}tag/x"), &format!("{START}page")]);
    let scan_text = path.to_string_lossy().into_owned();

    let (code, out) = run_cli(&argv(&[
        "crawl-control",
        "--scan",
        &scan_text,
        "--action",
        "narrow-scope",
        "--exclude",
        "/tag/",
    ]));
    assert_eq!(code, 0, "stderr: {}", out.stderr);
    let document: Value = serde_json::from_str(&out.stdout).expect("stdout json");
    assert_eq!(
        document,
        json!({
            "ok": true,
            "scan": scan_text,
            "state": "narrowed",
            "exclude": "/tag/",
            "include": Value::Null,
            "dropped": 1,
            "queued": 2,
        })
    );
    assert_eq!(states(&path)["tag/x"], "excluded");
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn crawl_control_adds_urls_to_a_paused_artifact() {
    let work = scratch("add");
    let path = work.join("paused.sqlite");
    paused_scan(&path);
    let scan_text = path.to_string_lossy().into_owned();

    // The fragment is stripped before the identity check, so the second URL
    // lands on the same frontier row as the first.
    let (code, out) = run_cli(&argv(&[
        "crawl-control",
        "--scan",
        &scan_text,
        "--action",
        "add-urls",
        "--urls",
        &format!("{START}boost,{START}boost#frag"),
    ]));
    assert_eq!(code, 0, "stderr: {}", out.stderr);
    let document: Value = serde_json::from_str(&out.stdout).expect("stdout json");
    assert_eq!(
        document,
        json!({
            "ok": true,
            "scan": scan_text,
            "state": "urls_added",
            "added": 1,
            "already_known": 0,
        })
    );
    assert_eq!(states(&path)["boost"], "queued");
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn paused_steering_refuses_a_finished_artifact() {
    let work = scratch("finished");
    let path = work.join("finished.sqlite");
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
        &path,
        START,
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
    scan.finish_capture("finished", 10.0).expect("finish");
    scan.close();
    let scan_text = path.to_string_lossy().into_owned();

    for args in [
        vec!["narrow-scope", "--exclude", "/tag/"],
        vec!["add-urls", "--urls", &format!("{START}boost")],
    ] {
        let mut full = argv(&["crawl-control", "--scan", &scan_text, "--action"]);
        full.extend(args.iter().map(|a| a.to_string()));
        let (code, out) = run_cli(&full);
        assert_ne!(code, 0, "expected refusal: {}", out.stderr);
        assert!(out.stderr.contains("already finished"), "{}", out.stderr);
    }
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn the_scan_route_is_steering_only() {
    let work = scratch("steering-only");
    let path = work.join("paused.sqlite");
    paused_scan(&path);
    let scan_text = path.to_string_lossy().into_owned();

    // A run_id belongs to a live run: it cannot combine with a saved artifact.
    let (code, out) = run_cli(&argv(&[
        "crawl-control",
        "--scan",
        &scan_text,
        "--run-id",
        &"a".repeat(32),
        "--action",
        "narrow-scope",
        "--exclude",
        "/x",
    ]));
    assert_ne!(code, 0);
    assert!(out.stderr.contains("live run by run_id"), "{}", out.stderr);

    // pause/stop apply to a live run, not a saved artifact.
    let (code, out) = run_cli(&argv(&[
        "crawl-control",
        "--scan",
        &scan_text,
        "--action",
        "pause",
    ]));
    assert_ne!(code, 0);
    assert!(
        out.stderr.contains("live run, not a saved artifact"),
        "{}",
        out.stderr
    );

    // Without scan the route needs a project and a run_id.
    let (code, out) = run_cli(&argv(&[
        "crawl-control",
        "--action",
        "narrow-scope",
        "--exclude",
        "/x",
    ]));
    assert_ne!(code, 0);
    assert!(out.stderr.contains("project and run_id"), "{}", out.stderr);

    // narrow-scope needs at least one pattern; add-urls needs urls.
    let (code, out) = run_cli(&argv(&[
        "crawl-control",
        "--scan",
        &scan_text,
        "--action",
        "narrow-scope",
    ]));
    assert_ne!(code, 0);
    assert!(out.stderr.contains("exclude or include"), "{}", out.stderr);

    let (code, out) = run_cli(&argv(&[
        "crawl-control",
        "--scan",
        &scan_text,
        "--action",
        "add-urls",
    ]));
    assert_ne!(code, 0);
    assert!(out.stderr.contains("requires urls"), "{}", out.stderr);
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn the_paused_route_reaches_the_handler_through_the_cli() {
    let work = scratch("cli-include");
    let path = work.join("paused.sqlite");
    paused_scan(&path);
    let scan_text = path.to_string_lossy().into_owned();
    let (code, out) = run_cli(&argv(&[
        "crawl-control",
        "--scan",
        &scan_text,
        "--action",
        "narrow-scope",
        "--include",
        "/blog/",
    ]));
    assert_eq!(code, 0, "stderr: {}", out.stderr);
    let document: Value = serde_json::from_str(&out.stdout).expect("stdout json");
    assert_eq!(document["state"], json!("narrowed"));
    assert_eq!(document["include"], json!("/blog/"));
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn a_paused_narrowing_holds_across_the_resume() {
    // A minimal loopback origin: the start page links to an already-dropped
    // URL and to a fresh one, so the narrowed scope must reject exactly one.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let routes: std::collections::HashMap<String, (String, String)> = [
        (
            "/robots.txt".to_owned(),
            (
                "text/plain".to_owned(),
                "User-agent: *\nAllow: /\n".to_owned(),
            ),
        ),
        (
            "/".to_owned(),
            (
                "text/html".to_owned(),
                "<html><body><a href=\"/tag/x\">tag</a> <a href=\"/blog/a\">blog</a></body></html>"
                    .to_owned(),
            ),
        ),
        (
            "/blog/a".to_owned(),
            (
                "text/html".to_owned(),
                "<html><body>leaf</body></html>".to_owned(),
            ),
        ),
    ]
    .into_iter()
    .collect();
    // The server thread lives for the test; the listener moves into it.
    let _server = std::thread::spawn(move || {
        while let Ok((mut stream, _)) = listener.accept() {
            serve_once(&mut stream, &routes);
        }
    });
    let work = scratch("holds");
    let path = work.join("resume.sqlite");
    // The paused artifact starts the same origin; the narrow drops its one
    // queued /tag/x before the resume.
    let start = format!("{}/", base);
    {
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
            &path,
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
        let mut entry = Map::new();
        entry.insert("requested_url".into(), json!(start));
        entry.insert("frontier_url".into(), json!(start));
        entry.insert("depth".into(), json!(0));
        entry.insert("reason".into(), json!(""));
        entry.insert("source".into(), json!("start"));
        entry.insert("reserve_query".into(), json!(false));
        entry.insert("seed".into(), json!(false));
        scan.seed_frontier(&[entry]).expect("seed");
        scan.interrupt("paused").expect("interrupt");
        scan.close();
    }
    queue(&path, &[&format!("{}/tag/x", base)]);
    let scan_text = path.to_string_lossy().into_owned();

    let (code, out) = run_cli(&argv(&[
        "crawl-control",
        "--scan",
        &scan_text,
        "--action",
        "narrow-scope",
        "--exclude",
        "/tag/",
    ]));
    assert_eq!(code, 0, "stderr: {}", out.stderr);

    // The resume reads the narrowed scope back from the artifact: /tag/x is
    // re-discovered through the start page's links and rejected by pattern,
    // /blog/a is fetched.
    let (code, out) = run_cli(&argv(&["crawl-site", "--resume", &scan_text]));
    assert_eq!(code, 0, "stderr: {}", out.stderr);
    let document: Value = serde_json::from_str(&out.stdout).expect("stdout json");
    assert_eq!(document["urls_collected"], json!(2), "{}", out.stdout);

    let tag_url = format!("{}/tag/x", base);
    // The resumed artifact is finished: read the decision back read-only.
    let con =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("readonly open");
    let mut statement = con
        .prepare("SELECT url, reason FROM decisions WHERE url=?")
        .expect("decisions query");
    let mut rows = statement.query([&tag_url]).expect("rows");
    // Both the narrow drop and the resume's discovery rejection record the
    // pair; the assertion is membership, like the reference set.
    let mut decided = std::collections::BTreeSet::new();
    while let Some(row) = rows.next().expect("row") {
        decided.insert((
            row.get::<_, String>(0).expect("url"),
            row.get::<_, String>(1).expect("reason"),
        ));
    }
    assert!(decided.contains(&(tag_url, "excluded_by_pattern".to_owned())));
    let _ = std::fs::remove_dir_all(&work);
}

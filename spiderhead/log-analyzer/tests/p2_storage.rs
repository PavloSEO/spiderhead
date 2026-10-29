//! Per-request storage, cross-call dedupe, stale-import cleanup and the derived views.

mod common;

use std::path::Path;

use rusqlite::Connection;
use serde_json::{json, Value};
use spiderhead_log_analyzer::import::{import_logs, ImportContext, ImportOptions};
use spiderhead_log_analyzer::store::{cleanup_stale_imports, open_log_source};
use spiderhead_log_analyzer::summary::{log_summary, SummaryQuery};

use common::{make_project, DirLoader};

const CHROME: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/120.0 Safari/537.36";
const BOT: &str = "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)";

fn line(ip: &str, path: &str, second: u32, ua: &str, bytes: u32) -> String {
    format!("{ip} - - [10/Oct/2026:10:00:{second:02} +0000] \"GET {path} HTTP/1.1\" 200 {bytes} \"https://ref.test/\" \"{ua}\"\n")
}

fn run(root: &Path, source: &Path, options: ImportOptions) -> Value {
    let free = |_: &Path| Ok(u64::MAX);
    let now = || 1_790_000_000.0;
    let ctx = ImportContext {
        loader: &DirLoader,
        free_space: &free,
        now: &now,
    };
    import_logs(
        &ctx,
        &source.to_string_lossy(),
        &root.to_string_lossy(),
        options,
    )
}

fn append() -> ImportOptions {
    ImportOptions {
        append: true,
        ..ImportOptions::default()
    }
}

fn count(source: &str, table: &str) -> i64 {
    Connection::open(source)
        .unwrap()
        .query_row(
            &format!(
                "SELECT COALESCE(SUM({}),0) FROM {table}",
                if table == "visits" { "visits" } else { "1" }
            ),
            [],
            |r| r.get(0),
        )
        .unwrap()
}

fn summary(source: &str, view: &str) -> Value {
    log_summary(
        source,
        &SummaryQuery {
            view: view.into(),
            ..SummaryQuery::default()
        },
    )
}

fn project() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("proj");
    make_project(&root, "https://example.test");
    (tmp, root)
}

#[test]
fn every_request_is_kept_and_aggregates_sit_on_top() {
    let (tmp, root) = project();
    let log = tmp.path().join("a.log");
    let same = line("1.1.1.1", "/a", 1, CHROME, 100);
    std::fs::write(
        &log,
        format!("{same}{same}{}", line("66.249.66.1", "/b", 2, BOT, 5)),
    )
    .unwrap();
    let r = run(&root, &log, ImportOptions::default());
    assert_eq!(r["ok"], true, "{r}");
    let src = r["source"].as_str().unwrap();
    assert_eq!(
        count(src, "requests"),
        3,
        "a repeated request inside one file is real traffic"
    );
    assert_eq!(count(src, "visits"), 3);
    assert!(
        open_log_source(src).is_ok(),
        "artifact with requests validates"
    );
}

#[test]
fn reimporting_the_same_file_in_a_separate_call_adds_nothing() {
    let (tmp, root) = project();
    let log = tmp.path().join("a.log");
    let same = line("1.1.1.1", "/a", 1, CHROME, 100);
    std::fs::write(
        &log,
        format!("{same}{same}{}", line("66.249.66.1", "/b", 2, BOT, 5)),
    )
    .unwrap();
    let first = run(&root, &log, append());
    let again = run(&root, &log, append());
    assert_eq!(again["ok"], true, "{again}");
    assert_eq!(
        again["source"], first["source"],
        "appended into the same artifact"
    );
    let src = again["source"].as_str().unwrap();
    assert_eq!(count(src, "requests"), 3);
    assert_eq!(count(src, "visits"), 3);
    assert_eq!(again["lines"]["duplicate"], 3);
    assert_eq!(
        again["lines"]["parsed"], 3,
        "parsed counts only the first run"
    );
    assert_eq!(std::fs::read_dir(root.join("logs")).unwrap().count(), 1);
}

#[test]
fn appending_a_longer_log_adds_only_the_new_lines() {
    let (tmp, root) = project();
    let log = tmp.path().join("a.log");
    let a = line("1.1.1.1", "/a", 1, CHROME, 10);
    let b = line("1.1.1.1", "/b", 2, CHROME, 20);
    std::fs::write(&log, &a).unwrap();
    run(&root, &log, append());
    std::fs::write(&log, format!("{a}{b}")).unwrap();
    let r = run(&root, &log, append());
    let src = r["source"].as_str().unwrap();
    assert_eq!(count(src, "requests"), 2);
    assert_eq!(count(src, "visits"), 2);
    assert_eq!(r["lines"]["parsed"], 2);
    assert_eq!(r["complete"], true);
}

#[test]
fn stale_unfinished_artifacts_are_removed_at_the_next_import() {
    let (tmp, root) = project();
    let logs = root.join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::write(logs.join("dead.sqlite"), b"").unwrap();
    // Young file: might be a running import, so it stays.
    assert!(cleanup_stale_imports(&root, 0.0, 3600.0)
        .unwrap()
        .is_empty());
    assert!(logs.join("dead.sqlite").exists());
    let later = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
        + 7200.0;
    let log = tmp.path().join("a.log");
    std::fs::write(&log, line("1.1.1.1", "/a", 1, CHROME, 1)).unwrap();
    let finished = run(&root, &log, ImportOptions::default());
    let kept = finished["source"].as_str().unwrap();
    let removed = cleanup_stale_imports(&root, later, 3600.0).unwrap();
    assert_eq!(removed.len(), 1);
    assert!(!logs.join("dead.sqlite").exists());
    assert!(
        Path::new(kept).exists(),
        "finished artifacts are never removed"
    );
}

#[test]
fn derived_views_group_by_file_type_browser_and_url() {
    let (tmp, root) = project();
    let log = tmp.path().join("a.log");
    let body = [
        line("1.1.1.1", "/page", 1, CHROME, 100),
        line("1.1.1.1", "/app.js", 2, CHROME, 300),
        line(
            "1.1.1.1",
            "/logo.png",
            3,
            &format!("{CHROME} Edg/120.0"),
            500,
        ),
        line("66.249.66.1", "/page", 4, BOT, 100),
    ]
    .concat();
    std::fs::write(&log, body).unwrap();
    let r = run(&root, &log, ImportOptions::default());
    let src = r["source"].as_str().unwrap();

    let types = summary(src, "file_types");
    let names: Vec<&str> = types["file_types"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["file_type"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["html", "image", "javascript"]);

    let browsers = summary(src, "browsers");
    let rows = browsers["browsers"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{browsers}");
    assert_eq!(rows[0]["browser"], "chrome");
    assert_eq!(rows[0]["visits"], 2);
    assert_eq!(rows[1]["browser"], "edge");

    let detail = summary(src, "urls_detail");
    let top = &detail["urls_detail"][0];
    assert_eq!(top["url"], "/page");
    assert_eq!(top["visits"], 2);
    assert_eq!(top["avg_bytes"], json!(100.0));
    assert_eq!(top["first_date"], "2026-10-10");
    assert_eq!(top["days_since_last"], 0);
    assert_eq!(top["bots"], 2);
}

#[test]
fn browsers_view_without_request_rows_says_why() {
    let (tmp, root) = project();
    let log = tmp.path().join("a.log");
    std::fs::write(&log, line("1.1.1.1", "/a", 1, CHROME, 1)).unwrap();
    let r = run(
        &root,
        &log,
        ImportOptions {
            keep_requests: false,
            ..ImportOptions::default()
        },
    );
    let out = summary(r["source"].as_str().unwrap(), "browsers");
    assert_eq!(out["ok"], false);
    assert!(out["error"].as_str().unwrap().contains("keep_requests"));
}

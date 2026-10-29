//! Re-importing the same file, or files that overlap, must not double visits.

mod common;

use std::path::Path;

use rusqlite::Connection;
use spiderhead_log_analyzer::import::{import_logs, ImportContext, ImportOptions};

use common::{make_project, DirLoader};

fn line(ip: &str, path: &str, second: u32) -> String {
    format!(
        "{ip} - - [10/Oct/2026:10:00:{second:02} +0000] \"GET {path} HTTP/1.1\" 200 512 \"-\" \"Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)\"\n"
    )
}

fn run(root: &Path, source: &Path) -> serde_json::Value {
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
        ImportOptions::default(),
    )
}

fn total_visits(result: &serde_json::Value) -> i64 {
    let con = Connection::open(result["source"].as_str().unwrap()).unwrap();
    con.query_row("SELECT COALESCE(SUM(visits),0) FROM visits", [], |r| {
        r.get(0)
    })
    .unwrap()
}

#[test]
fn identical_copy_in_one_directory_adds_no_visits() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("proj");
    make_project(&root, "https://example.test");
    let logs = tmp.path().join("in");
    std::fs::create_dir_all(&logs).unwrap();
    let body = format!(
        "{}{}",
        line("66.249.66.1", "/a", 1),
        line("66.249.66.1", "/b", 2)
    );
    std::fs::write(logs.join("access.log"), &body).unwrap();
    std::fs::write(logs.join("access-copy.log"), &body).unwrap();
    let result = run(&root, &logs);
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(total_visits(&result), 2);
    assert_eq!(result["lines"]["duplicate"], 2);
}

#[test]
fn overlapping_files_count_the_shared_lines_once() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("proj");
    make_project(&root, "https://example.test");
    let logs = tmp.path().join("in");
    std::fs::create_dir_all(&logs).unwrap();
    let (a, b, c) = (
        line("66.249.66.1", "/a", 1),
        line("66.249.66.1", "/b", 2),
        line("66.249.66.1", "/c", 3),
    );
    std::fs::write(logs.join("1.log"), format!("{a}{b}")).unwrap();
    std::fs::write(logs.join("2.log"), format!("{b}{c}")).unwrap();
    let result = run(&root, &logs);
    assert_eq!(total_visits(&result), 3);
}

#[test]
fn repeated_lines_inside_one_file_are_real_traffic() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("proj");
    make_project(&root, "https://example.test");
    let file = tmp.path().join("one.log");
    let a = line("66.249.66.1", "/a", 1);
    std::fs::write(&file, format!("{a}{a}{a}")).unwrap();
    let result = run(&root, &file);
    assert_eq!(total_visits(&result), 3);
    assert!(result["lines"].get("duplicate").is_none());
}

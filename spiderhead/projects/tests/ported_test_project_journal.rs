//! land in events.jsonl and log.md.
//!
//! `project_event_append` is an mcp handler and the `reports.build_report`
//! monkeypatch belongs to the reports crate — both stay deferred.

#[path = "common/testkit.rs"]
mod testkit;

use serde_json::Value;
use spiderhead_projects::coverage::{coverage_status, initialize_coverage, record_execution};
use spiderhead_projects::journal;
use spiderhead_projects::run_observation;

fn initialized(name: &str) -> (testkit::Scratch, std::path::PathBuf) {
    let (dir, root) = testkit::project("journal", name);
    initialize_coverage(&root, None).expect("initialize_coverage");
    (dir, root)
}

fn events(root: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(root.join("events.jsonl"))
        .expect("events.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).expect("event json"))
        .collect()
}

fn log_bullets(root: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(root.join("log.md"))
        .expect("log.md")
        .lines()
        .filter(|line| line.starts_with("- "))
        .map(str::to_owned)
        .collect()
}

fn live_run(root: &std::path::Path) -> Value {
    std::fs::create_dir_all(root.join("scans")).expect("scans dir");
    std::fs::write(root.join("scans/live.sqlite"), b"").expect("artifact");
    run_observation::start(
        root,
        "native",
        "spider",
        100,
        20_000,
        0,
        None,
        "synthetic",
        Some(root.join("scans/live.sqlite").to_str().unwrap()),
        false,
        None,
        None,
        None,
        None,
    )
    .expect("run start")
}

#[test]
fn record_writes_event_and_mirrors_bullet_to_log() {
    let (_dir, root) = initialized("record");

    let result = journal::record(&root, "user", "user", "Pause\nthe crawl").expect("record");

    assert_eq!(result["ok"], true);
    assert_eq!(events(&root).last().unwrap()["text"], "Pause\nthe crawl");
    assert!(log_bullets(&root)
        .last()
        .unwrap()
        .ends_with("user/user · Pause the crawl"));
}

#[test]
fn note_is_silent_for_a_directory_that_is_not_a_project() {
    let dir = testkit::scratch("journal", "not-a-project");

    journal::note(&dir.0, "scans", "agent", "crawl x finished");
    assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 0);
}

#[test]
fn crawl_finish_is_journaled() {
    let (_dir, root) = initialized("crawl-finish");
    let run = live_run(&root);
    let run_id = run["id"].as_str().unwrap();

    run_observation::finish(&root, run_id, "finished", "all URLs fetched", None).expect("finish");

    let text = format!("crawl {run_id} finished: all URLs fetched");
    let last = events(&root).last().unwrap().clone();
    assert_eq!(last["source"], "scans");
    assert_eq!(last["text"], text);
    assert!(log_bullets(&root)
        .last()
        .unwrap()
        .ends_with(&format!("scans/agent · {text}")));
}

#[test]
fn run_start_and_first_measured_progress_are_journaled() {
    let (_dir, root) = initialized("run-start");
    let run = live_run(&root);
    let run_id = run["id"].as_str().unwrap();

    run_observation::progress(
        &root,
        run_id,
        Some(2),
        Some(3),
        Some(1),
        Some(0),
        None,
        None,
        "separate",
        "collector",
    )
    .expect("progress 1");
    run_observation::progress(
        &root,
        run_id,
        Some(3),
        Some(2),
        Some(1),
        Some(0),
        None,
        None,
        "separate",
        "collector",
    )
    .expect("progress 2");

    let rows = events(&root);
    let texts: Vec<&str> = rows
        .iter()
        .map(|row| row["text"].as_str().unwrap())
        .collect();
    assert_eq!(
        texts,
        [
            format!("run {run_id} started (native/spider)"),
            format!("run {run_id} progress: fetched=2, queued=3, inflight=1"),
        ]
    );
    assert_eq!(log_bullets(&root).len(), 2);
}

#[test]
fn checklist_attempt_is_journaled() {
    let (_dir, root) = initialized("checklist");
    let catalogue = spiderhead_projects::catalogue::catalogue().expect("catalogue");
    let item_id = catalogue.keys().next().unwrap().clone();
    record_execution(
        &root,
        &item_id,
        &serde_json::json!({"status": "running", "reason": "Synthetic attempt"}),
        coverage_status(&root, true).expect("status")["revision"]
            .as_i64()
            .unwrap(),
    )
    .expect("record_execution");

    let rows = events(&root);
    let last = rows.last().unwrap();
    assert_eq!(last["source"], "agent");
    assert_eq!(last["text"], format!("checklist {item_id}: running"));
}

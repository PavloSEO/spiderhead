//! moves the automatic checklist items it measured.
//!
//! `check:BROKEN_PAGE_4XX` rides the `audited-cap500.sqlite` fixture — the
//! oracle audit records it under `checks_silent_ids`, which `record_scan`
//! counts as executed. The `_saved_audit` monkeypatch case and the mcp
//! `crawl_site` handler case stay deferred; the reference file itself is marked
//! `the reference suite.mark.slow` — the `record_failure` cases iterate every automatic
//! checklist item.

#[path = "common/testkit.rs"]
mod testkit;

use serde_json::Value;
use spiderhead_projects::coverage::{coverage_status, initialize_coverage};
use spiderhead_projects::crawl_evidence::{record_failure, record_scan};

fn initialized(name: &str) -> (testkit::Scratch, std::path::PathBuf) {
    let (dir, root) = testkit::project("cevidence", name);
    initialize_coverage(&root, None).expect("initialize_coverage");
    (dir, root)
}

fn source(root: &std::path::Path, name: &str) {
    std::fs::create_dir_all(root.join("scans")).expect("scans dir");
    std::fs::copy(
        testkit::fixture("crawl-evidence.sqlite"),
        root.join("scans").join(name),
    )
    .expect("scan");
}

fn row<'a>(status: &'a Value, item_id: &str) -> &'a Value {
    status["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == item_id)
        .expect("checklist row")
}

#[test]
fn executed_check_becomes_evidence_and_progress_moves() {
    let (_dir, root) = initialized("executed");
    source(&root, "crawl.sqlite");
    let before = coverage_status(&root, true).expect("status")["counts"]["remaining"]
        .as_i64()
        .unwrap();

    let evidence = record_scan(&root, &root.join("scans/crawl.sqlite")).expect("record_scan");

    assert!(evidence["succeeded"]
        .as_array()
        .unwrap()
        .iter()
        .any(|id| id == "check:BROKEN_PAGE_4XX"));
    let status = coverage_status(&root, true).expect("status");
    let measured = row(&status, "check:BROKEN_PAGE_4XX");
    assert_eq!(measured["state"], "run");
    assert_eq!(measured["complete"], true);
    assert!(matches!(
        measured["measurement"]["state"].as_str().unwrap(),
        "measured" | "limited"
    ));
    assert!(status["counts"]["remaining"].as_i64().unwrap() < before);
}

#[test]
fn unusable_crawl_marks_automatic_checks_failed() {
    let (_dir, root) = initialized("unusable");
    std::fs::create_dir_all(root.join("scans")).expect("scans dir");
    std::fs::write(root.join("scans/broken.sqlite"), b"not a scan").expect("broken");

    let evidence = record_scan(&root, &root.join("scans/broken.sqlite")).expect("record_scan");

    assert!(evidence["failed"]
        .as_array()
        .unwrap()
        .iter()
        .any(|id| id == "check:BROKEN_PAGE_4XX"));
    let status = coverage_status(&root, true).expect("status");
    let failed = row(&status, "check:BROKEN_PAGE_4XX");
    assert_eq!(failed["attempt_status"], "failed");
    assert_eq!(failed["state"], "not_run");
}

#[test]
fn crashed_crawl_records_failure_reason() {
    let (_dir, root) = initialized("crashed");

    let result = record_failure(&root, "KeyboardInterrupt").expect("record_failure");

    assert!(result["failed"]
        .as_array()
        .unwrap()
        .iter()
        .any(|id| id == "check:BROKEN_PAGE_4XX"));
    assert_eq!(
        row(
            &coverage_status(&root, true).expect("status"),
            "check:BROKEN_PAGE_4XX"
        )["reason"],
        "KeyboardInterrupt"
    );
}

#[test]
fn uninitialized_checklist_is_skipped_without_error() {
    let (_dir, root) = testkit::project("cevidence", "bare");
    source(&root, "crawl.sqlite");

    assert!(record_scan(&root, &root.join("scans/crawl.sqlite"))
        .expect("record_scan")
        .get("skipped")
        .is_some());
    assert!(record_failure(&root, "boom")
        .expect("record_failure")
        .get("skipped")
        .is_some());
}

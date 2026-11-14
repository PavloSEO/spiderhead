//! revisioned, attributed and separate from execution approval.

#[path = "common/testkit.rs"]
mod testkit;

use serde_json::{json, Value};
use spiderhead_projects::coverage::{
    append_task_comment, coverage_status, initialize_coverage, update_item,
};
use spiderhead_projects::observer::{checklist_page, task_detail};

#[test]
fn custom_task_metadata_comments_and_unknown_affected_urls() {
    let (_dir, root) = testkit::project("taskcontract", "project");
    let initialized = initialize_coverage(&root, None).expect("initialize_coverage");
    let task_id = "custom:canonical-review";
    let update = update_item(
        &root,
        &json!({
            "id": task_id,
            "title": "Review the canonical correction",
            "assignee": "web-team",
            "description": "Confirm the canonical points to the preferred URL.",
            "acceptance_criterion": "The retained rescan shows the expected canonical.",
            "finding_ref": {
                "scan_id": "scan-synthetic-2026-10-12",
                "finding_id": "finding-synthetic-001",
            },
            "method_ref": "scenario:project-control",
            "competitors": [{"url": "https://competitor.example.test/", "max_urls": 250}],
        }),
        initialized["revision"].as_i64().unwrap(),
    )
    .expect("update_item");

    let page = checklist_page(
        &root,
        0,
        50,
        "canonical-review",
        None,
        None,
        "id",
        false,
        None,
    )
    .expect("checklist_page");
    let row = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == task_id)
        .expect("task row");
    assert_eq!(row["task"]["executor"], "web-team");
    assert_eq!(row["display_state"], "not_run");
    assert_eq!(row["attempt_status"], "not_run");
    assert_eq!(row["task"]["state"], "remaining");
    assert_eq!(
        row["task"]["competitors"],
        json!([{"url": "https://competitor.example.test/", "max_urls": 250}])
    );
    let affected = &row["affected_urls"];
    assert!(affected["count"].is_null());
    assert_eq!(affected["state"], "not_measured");
    assert_eq!(affected["scope_state"], "unknown");
    assert_eq!(affected["source"], "finding_ref");
    assert_eq!(
        affected["reason"],
        "referenced scan is not retained in this project"
    );

    append_task_comment(
        &root,
        task_id,
        "The finding is ready for owner review.",
        "agent/review-session",
        update["revision"].as_i64().unwrap(),
        Some(&json!({
            "kind": "finding",
            "scan_id": "scan-synthetic-2026-10-12",
            "id": "finding-synthetic-001",
        })),
    )
    .expect("append_task_comment");
    let detail = task_detail(&root, task_id).expect("task_detail");
    assert_eq!(detail["item"]["display_state"], "not_run");
    assert_eq!(detail["item"]["task"]["state"], "remaining");
    assert_eq!(detail["item"]["task"]["comments_total"], 1);
    assert_eq!(
        detail["item"]["task"]["comments"][0]["actor"],
        "agent/review-session"
    );
    assert_eq!(
        detail["item"]["task"]["comments"][0]["reference"]["kind"],
        "finding"
    );
    let document: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("coverage.json")).expect("coverage.json"),
    )
    .expect("coverage json");
    assert_eq!(document["format"], "spiderhead.coverage.v4");
    assert!(
        coverage_status(&root, true).expect("coverage_status")["task_kpis"]["total"]
            .as_i64()
            .unwrap()
            >= 1
    );
}

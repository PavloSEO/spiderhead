//! (unscoped progress, scoped percent, state view, pagination/validation).
//! The two cli/mcp wiring tests stay deferred to those slices.

#[path = "common/testkit.rs"]
mod testkit;

use serde_json::{json, Value};
use spiderhead_projects::coverage::{
    coverage_status, initialize_coverage, initialize_coverage_full, record_execution, update_item,
};
use spiderhead_projects::progress::project_progress;
use spiderhead_projects::workspace::create_project;

fn project(dir: &testkit::Scratch) -> std::path::PathBuf {
    let root = dir.0.join("project");
    create_project(
        &root,
        "https://example.test/",
        Some("Synthetic project"),
        None,
        None,
        None,
    )
    .expect("create");
    initialize_coverage(&root, None).expect("coverage");
    root
}

fn revision(root: &std::path::Path) -> i64 {
    coverage_status(root, false).expect("coverage_status")["revision"]
        .as_i64()
        .expect("revision")
}

fn edit(root: &std::path::Path, item: Value) {
    let rev = revision(root);
    update_item(root, &item, rev).expect("update_item");
}

fn record(root: &std::path::Path, item_id: &str, rec: Value) {
    let rev = revision(root);
    record_execution(root, item_id, &rec, rev).expect("record_execution");
}

fn plan(item_ids: &[&str]) -> Value {
    json!({
        "reviewer": "Synthetic reviewer",
        "population": {
            "kind": "unknown",
            "size": Value::Null,
            "urls": [],
            "name": Value::Null,
            "source": "Synthetic scope fixture",
            "reason": "URL population intentionally unknown in this fixture",
            "templates": {},
        },
        "tasks": {"kind": "selection", "ids": item_ids, "source": "Synthetic task selection"},
    })
}

#[test]
fn unscoped_progress_has_no_percent_or_implicit_completion() {
    let dir = testkit::scratch("progress", "unscoped");
    let root = project(&dir);
    let before = std::fs::read(root.join("coverage.json")).expect("coverage.json");

    let result = project_progress(&root, 5, 0, None, None).expect("progress");

    let ratio = &result["audit_task_completion"];
    assert_eq!(ratio["label"], "Audit-task completion");
    assert!(ratio["percent"].is_null());
    assert!(ratio["numerator"].is_null() && ratio["denominator"].is_null());
    assert!(ratio["reason"]
        .as_str()
        .unwrap()
        .contains("explicit audit plan"));
    assert_eq!(result["url_population"]["state"], "unknown");
    assert_eq!(result["audit_complete"], false);
    assert_eq!(result["next_actions"][0]["id"], "scope:agree");
    assert!(result["items"].as_array().unwrap().len() <= 5);
    assert_eq!(
        std::fs::read(root.join("coverage.json")).expect("coverage.json"),
        before
    );
}

#[test]
fn task_percent_reuses_core_axis_and_stays_separate_from_url_coverage() {
    let dir = testkit::scratch("progress", "percent");
    let root = project(&dir);
    edit(
        &root,
        json!({"id": "custom:manual-review", "title": "Review synthetic page sample"}),
    );
    let rev = revision(&root);
    initialize_coverage_full(
        &root,
        None,
        Some(rev),
        Some(&plan(&["custom:manual-review"])),
    )
    .expect("reinit");
    let rev = revision(&root);
    let status = record_execution(
        &root,
        "custom:manual-review",
        &json!({
            "status": "succeeded",
            "reason": "Synthetic reviewer signed off",
            "reviewer": "Synthetic reviewer",
            "signoff": true,
        }),
        rev,
    )
    .expect("record");

    let result = project_progress(&root, 50, 0, None, None).expect("progress");
    let axis = &status["coverage"]["audit_tasks"];
    let ratio = &result["audit_task_completion"];

    assert_eq!(result["scope"]["recorded_for_all_sites"], true);
    assert_eq!(ratio["state"], "measured");
    assert_eq!(ratio["numerator"], axis["numerator"]);
    assert_eq!(ratio["numerator"], 1);
    assert_eq!(ratio["denominator"], axis["denominator"]);
    assert_eq!(ratio["denominator"], 1);
    assert_eq!(ratio["percent"], 100.0);
    assert_eq!(result["url_population"]["state"], "unknown");
    assert_eq!(result["audit_complete"], false);
}

#[test]
fn state_view_distinguishes_running_blocked_unavailable_stale_review_and_deliverable() {
    let dir = testkit::scratch("progress", "states");
    let root = project(&dir);
    let scope = json!({"site": "https://example.test/", "template": Value::Null, "urls": []});
    for (id, title) in [
        ("custom:remaining", "Remaining automatic task"),
        ("custom:running", "Running task"),
        ("custom:unavailable", "Unavailable task"),
        ("custom:stale", "Stale manual review"),
        ("custom:review", "Manual review"),
        ("custom:not-run", "Never attempted manual check"),
        ("custom:deliverable", "Deliverable review"),
    ] {
        edit(&root, json!({"id": id, "title": title}));
    }
    for id in ["custom:remaining", "custom:running", "custom:unavailable"] {
        edit(
            &root,
            json!({
                "id": id,
                "execution_kind": "automatic",
                "operation": "check:BROKEN_PAGE_4XX",
                "scope": scope,
            }),
        );
    }
    edit(
        &root,
        json!({"id": "custom:blocked", "title": "Blocked task", "dependencies": ["custom:remaining"]}),
    );
    edit(
        &root,
        json!({"id": "custom:deliverable", "execution_kind": "deliverable"}),
    );
    let item_ids = [
        "custom:remaining",
        "custom:running",
        "custom:unavailable",
        "custom:stale",
        "custom:review",
        "custom:not-run",
        "custom:deliverable",
        "custom:blocked",
    ];
    let rev = revision(&root);
    initialize_coverage_full(&root, None, Some(rev), Some(&plan(&item_ids))).expect("reinit");
    record(
        &root,
        "custom:running",
        json!({"status": "running", "reason": "Synthetic run in progress"}),
    );
    record(
        &root,
        "custom:unavailable",
        json!({"status": "unavailable", "reason": "Synthetic input missing"}),
    );
    record(
        &root,
        "custom:stale",
        json!({
            "status": "succeeded",
            "reason": "Synthetic signoff",
            "reviewer": "Synthetic reviewer",
            "signoff": true,
        }),
    );
    edit(
        &root,
        json!({"id": "custom:stale", "title": "Changed after the recorded review"}),
    );

    let result = project_progress(&root, 100, 0, None, None).expect("progress");
    let row_states: std::collections::HashMap<_, _> = result["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| (i["id"].as_str().unwrap().to_string(), i["state"].clone()))
        .collect();

    assert_eq!(row_states["custom:remaining"], "remaining");
    assert_eq!(row_states["custom:running"], "running");
    assert_eq!(row_states["custom:unavailable"], "unavailable");
    assert_eq!(row_states["custom:stale"], "stale");
    assert_eq!(row_states["custom:review"], "not_run");
    assert_eq!(row_states["custom:not-run"], "not_run");
    assert_eq!(row_states["custom:deliverable"], "deliverable");
    assert_eq!(row_states["custom:blocked"], "blocked");
    assert_eq!(result["state_counts"]["running"], 1);
    assert!(result["state_counts"]["blocked"].as_i64().unwrap() >= 1);
    assert_eq!(result["state_counts"]["unavailable"], 1);
    assert_eq!(result["state_counts"]["stale"], 1);
    assert!(result["state_counts"]["not_run"].as_i64().unwrap() >= 2);
    assert_eq!(result["state_counts"]["deliverable"], 1);
}

#[test]
fn progress_pages_are_bounded_and_validate_arguments() {
    let dir = testkit::scratch("progress", "pages");
    let root = project(&dir);
    let first = project_progress(&root, 4, 0, None, None).expect("first");
    let second = project_progress(&root, 4, 4, None, None).expect("second");

    assert_eq!(first["items"].as_array().unwrap().len(), 4);
    assert_eq!(
        first["pagination"],
        json!({
            "limit": 4,
            "offset": 0,
            "total": second["pagination"]["total"],
            "next_offset": 4,
        })
    );
    let ids: std::collections::HashSet<_> = first["items"]
        .as_array()
        .unwrap()
        .iter()
        .chain(second["items"].as_array().unwrap())
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 8);
    assert!(first["next_actions"].as_array().unwrap().len() <= 5);
    let err = project_progress(&root, 0, 0, None, None).expect_err("limit 0 refuses");
    assert!(err.to_string().contains("limit"), "got: {err}");
    let err = project_progress(&root, 50, -1, None, None).expect_err("negative offset refuses");
    assert!(err.to_string().contains("offset"), "got: {err}");
}

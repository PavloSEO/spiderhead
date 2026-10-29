//! coverage must not inherit a primary site's known denominator.

#[path = "common/testkit.rs"]
mod testkit;

use serde_json::{json, Value};
use spiderhead_projects::coverage::{coverage_status, initialize_coverage};
use spiderhead_projects::runtime::{aggregate_coverage, PREPARATION_FORMAT};
use spiderhead_projects::workspace::create_project;

fn preparation(primary_uuid: &Value, competitor: &Value) -> Value {
    json!({
        "format": PREPARATION_FORMAT,
        "project_uuid": primary_uuid,
        "revision": 1,
        "state": "partial",
        "steps": {},
        "competitors": [
            {
                "url": competitor["site"]["target"],
                "directory": "competitors/candidate",
                "project_uuid": competitor["project_uuid"],
            }
        ],
    })
}

fn aggregate(root: &std::path::Path) -> Value {
    let primary = coverage_status(root, true).expect("coverage_status");
    aggregate_coverage(&root.to_string_lossy(), primary, None, true).expect("aggregate_coverage")
}

#[test]
fn uninitialized_competitor_withholds_aggregate_state_and_counts() {
    let dir = testkit::scratch("aggcov", "uninitialized");
    let root = dir.0.join("project");
    let primary = create_project(&root, "https://example.test/", None, None, None, None)
        .expect("create_project");
    initialize_coverage(&root, None).expect("initialize_coverage");
    let child = root.join("competitors/candidate");
    std::fs::create_dir_all(child.parent().unwrap()).expect("competitors dir");
    let competitor = create_project(&child, "https://competitor.test/", None, None, None, None)
        .expect("child project");
    std::fs::write(
        root.join("preparation.json"),
        serde_json::to_string(&preparation(
            &primary["project"]["project_uuid"],
            &competitor["project"],
        ))
        .unwrap(),
    )
    .expect("preparation.json");

    let result = aggregate(&root);
    assert_eq!(result["state"], "not_initialized");
    assert_eq!(result["complete"], false);
    assert_eq!(result["counts_known"], false);
    assert!(result["counts"].is_null());
    assert_eq!(result["items"], json!([]));
    assert_eq!(result["sites"][0]["state"], "initialized");
    assert_eq!(result["sites"][1]["state"], "not_initialized");
}

#[test]
fn competitor_manifest_missing_site_leaves_only_its_row_unavailable() {
    let dir = testkit::scratch("aggcov", "missing-site");
    let root = dir.0.join("project");
    let primary = create_project(&root, "https://example.test/", None, None, None, None)
        .expect("create_project");
    initialize_coverage(&root, None).expect("initialize_coverage");
    let child = root.join("competitors/candidate");
    std::fs::create_dir_all(child.parent().unwrap()).expect("competitors dir");
    let competitor = create_project(&child, "https://competitor.test/", None, None, None, None)
        .expect("child project");
    let manifest = child.join("project.json");
    let mut damaged: Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest).expect("manifest"))
            .expect("manifest json");
    damaged.as_object_mut().unwrap().remove("site");
    std::fs::write(&manifest, serde_json::to_string(&damaged).unwrap()).expect("write manifest");
    std::fs::write(
        root.join("preparation.json"),
        serde_json::to_string(&preparation(
            &primary["project"]["project_uuid"],
            &competitor["project"],
        ))
        .unwrap(),
    )
    .expect("preparation.json");

    let result = aggregate(&root);
    assert_eq!(result["sites"][0]["state"], "initialized");
    assert_eq!(result["sites"][1]["state"], "unavailable");
    assert_eq!(result["sites"][1]["directory"], "competitors/candidate");
}

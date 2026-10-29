//! crawl the URL as entered, open with a damaged competitor, and keep
//! competitor crawl results out of owner items.
//!
//! `_project_start_url`, `_evidence_root` and `_record_scoped_failure` are
//! mcp-handler helpers; the port asserts the same contract at the projects
//! layer — the `entered_url` fact the handler reads, and per-workspace
//! evidence isolation.

#[path = "common/testkit.rs"]
mod testkit;

use std::path::PathBuf;

use serde_json::{json, Value};
use spiderhead_projects::coverage::initialize_coverage;
use spiderhead_projects::runtime::add_competitors;
use spiderhead_projects::workspace::{create_project, open_project, project_status};

fn owner_with_www_competitor(name: &str) -> (testkit::Scratch, PathBuf, PathBuf) {
    let dir = testkit::scratch("compflow", name);
    let root = dir.0.join("owner");
    create_project(&root, "https://owner.example.test/", None, None, None, None)
        .expect("create_project");
    initialize_coverage(&root, None).expect("initialize_coverage");
    let candidate = json!({
        "url": "https://www.iana.org/",
        "source": "desktop paste",
        "observed_at": "2026-10-11T00:00:00Z",
    });
    let rows = add_competitors(&root, &json!([candidate])).expect("add_competitors");
    let directory = rows["competitors"][0]["directory"]
        .as_str()
        .expect("competitor directory");
    (dir, root.clone(), root.join(directory))
}

fn entered_url(project: &Value) -> Option<&str> {
    project["facts"].as_array().and_then(|facts| {
        facts.iter().find_map(|fact| {
            (fact["name"] == "entered_url")
                .then(|| fact["value"].as_str())
                .flatten()
        })
    })
}

#[test]
fn competitor_crawl_starts_from_the_url_as_entered() {
    let (_dir, _root, child) = owner_with_www_competitor("entered");

    let opened = open_project(&child, None).expect("open_project");
    assert_eq!(opened["project"]["site"]["target"], "https://iana.org/");
    // `_project_start_url` returns this fact's value for the same target.
    assert_eq!(
        entered_url(&opened["project"]),
        Some("https://www.iana.org/")
    );
}

#[test]
fn owner_project_still_starts_from_its_own_target() {
    let (_dir, root, _child) = owner_with_www_competitor("owner");

    let opened = open_project(&root, None).expect("open_project");
    assert_eq!(entered_url(&opened["project"]), None);
    assert_eq!(
        opened["project"]["site"]["target"],
        "https://owner.example.test/"
    );
}

#[test]
fn competitor_crawl_result_never_reaches_owner_items() {
    let (_dir, root, child) = owner_with_www_competitor("isolation");
    let before = project_status(&root, None, true).expect("status")["checklist"]["counts"].clone();

    // One bounded `record_execution` in the competitor's own workspace — the
    // projects-layer half of `_record_scoped_failure`: evidence lands in the
    // workspace it was written to and owner checklist counts do not move.
    let status = spiderhead_projects::coverage::coverage_status(&child, true)
        .expect("child coverage_status");
    let item = status["items"]
        .as_array()
        .expect("child items")
        .iter()
        .find(|item| item["execution_kind"] == "automatic")
        .expect("automatic item")
        .clone();
    spiderhead_projects::coverage::record_execution(
        &child,
        item["id"].as_str().unwrap(),
        &json!({"status": "failed", "reason": "crawl interrupted"}),
        status["revision"].as_i64().unwrap(),
    )
    .expect("record_execution");

    assert_eq!(
        project_status(&root, None, true).expect("status")["checklist"]["counts"],
        before
    );
}

#[test]
fn opening_a_project_survives_a_damaged_competitor_workspace() {
    let (_dir, root, child) = owner_with_www_competitor("damaged");
    let manifest = child.join("project.json");
    let mut document: Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest).expect("manifest"))
            .expect("manifest json");
    document["project_uuid"] = json!("00000000-0000-4000-8000-000000000000");
    std::fs::write(&manifest, serde_json::to_string(&document).unwrap()).expect("write manifest");

    let opened = open_project(&root, None).expect("open_project");
    assert_eq!(opened["ok"], true);
    let rows = project_status(&root, None, true).expect("status")["checklist"]["sites"]
        .as_array()
        .expect("sites")
        .clone();
    assert_eq!(rows[1]["state"], "unavailable");
    assert_eq!(
        project_status(&root, None, true).expect("status")["checklist"]["complete"],
        false
    );
}

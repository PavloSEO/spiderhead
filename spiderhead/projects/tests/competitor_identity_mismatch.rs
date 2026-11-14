//! competitor workspace must not make the owner project unreadable.
//! (`project_competitors_add`/`project_status` here are the projects-layer
//! calls the mcp handlers delegate to.)

#[path = "common/testkit.rs"]
mod testkit;

use serde_json::json;
use spiderhead_projects::coverage::initialize_coverage;
use spiderhead_projects::runtime::add_competitors;
use spiderhead_projects::workspace::{create_project, project_status};

#[test]
fn replaced_competitor_workspace_is_reported_not_fatal() {
    let (_dir, root) = testkit::project("compmismatch", "owner");
    initialize_coverage(&root, None).expect("initialize_coverage");
    add_competitors(&root, &json!(["https://competitor.example.test/"])).expect("add_competitors");
    let workspace = std::fs::read_dir(root.join("competitors"))
        .expect("competitors dir")
        .next()
        .expect("one workspace")
        .expect("entry")
        .path();
    std::fs::remove_dir_all(&workspace).expect("remove workspace");
    create_project(
        &workspace,
        "https://competitor.example.test/",
        None,
        None,
        None,
        None,
    )
    .expect("recreated competitor workspace");

    let status = project_status(&root, None, true).expect("project_status");

    let checklist = &status["checklist"];
    let sites = checklist["sites"].as_array().expect("sites");
    let (owner, competitor) = (&sites[0], &sites[1]);
    assert_eq!(owner["directory"], ".");
    assert_eq!(owner["state"], "initialized");
    assert_eq!(competitor["state"], "unavailable");
    assert_eq!(competitor["reason"], "competitor project identity mismatch");
    assert!(checklist["counts"].is_null());
    assert_eq!(checklist["counts_known"], false);
    assert_eq!(checklist["complete"], false);
}

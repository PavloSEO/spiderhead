//! untouched manual items reporting `not_run`, and checklist attempts reaching
//! the desktop journal. The CLI-written-entries case stays deferred (needs the
//! `project-inbox-submit` command) and the audit.v2 read-model case needs the
//! retained-scan fixtures from the reference suite.

use std::path::PathBuf;

use serde_json::{json, Value};
use spiderhead_projects::coverage::{
    coverage_status, initialize_coverage, initialize_coverage_full, record_execution, update_item,
};
use spiderhead_projects::event_log;
use spiderhead_projects::inbox;
use spiderhead_projects::progress::project_progress;
use spiderhead_projects::workspace::create_project;

/// Per-test scratch directory, removed on drop.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(name: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!("sh-proj-inbox-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Scratch(dir)
}

/// `_project` from the reference suite.
fn project(dir: &Scratch) -> PathBuf {
    let root = dir.0.join("project");
    create_project(
        &root,
        "https://example.test/",
        Some("Synthetic project"),
        None,
        None,
        None,
    )
    .unwrap();
    initialize_coverage(&root, None).unwrap();
    root
}

fn revision(root: &std::path::Path) -> i64 {
    coverage_status(root, true).unwrap()["revision"]
        .as_i64()
        .unwrap()
}

/// `_edit` from the reference suite.
fn edit(root: &std::path::Path, item: Value) {
    update_item(root, &item, revision(root)).unwrap();
}

/// `_record` from the reference suite.
fn record(root: &std::path::Path, item_id: &str, rec: Value) {
    record_execution(root, item_id, &rec, revision(root)).unwrap();
}

#[test]
fn goal_ids_are_accepted_as_inbox_references() {
    let dir = scratch("refs");
    let root = dir.0.join("project");
    create_project(
        &root,
        "https://example.test/",
        Some("Synthetic inbox fixture"),
        None,
        None,
        None,
    )
    .unwrap();
    let goal = inbox::submit(
        &root,
        "Compare the named competitor",
        "proposed_goal",
        None,
        "specialist",
        None,
    )
    .unwrap()["entry"]
        .clone();
    let note = inbox::submit(
        &root,
        "Follow up on the goal",
        "note",
        Some(&json!([goal["id"].clone()])),
        "specialist",
        None,
    )
    .unwrap()["entry"]
        .clone();
    assert_eq!(note["references"], json!([goal["id"]]));
    for bogus in [
        json!(["inbox:not-a-uuid"]),
        json!([format!(
            "inbox:{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        )]),
    ] {
        let err = inbox::submit(
            &root,
            "Bogus goal",
            "note",
            Some(&bogus),
            "specialist",
            None,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("invalid project reference"),
            "unexpected error: {err}"
        );
    }
}

#[test]
fn never_attempted_manual_item_is_not_run_not_review() {
    let dir = scratch("notrun");
    let root = project(&dir);
    edit(
        &root,
        json!({"id": "custom:manual", "title": "Manual check", "execution_kind": "manual"}),
    );
    initialize_coverage_full(
        &root,
        None,
        Some(revision(&root)),
        Some(&json!({
            "reviewer": "Synthetic reviewer",
            "population": {
                "kind": "unknown",
                "size": null,
                "urls": [],
                "name": null,
                "source": "Synthetic scope fixture",
                "reason": "URL population intentionally unknown in this fixture",
                "templates": {},
            },
            "tasks": {"kind": "selection", "ids": ["custom:manual"], "source": "Synthetic"},
        })),
    )
    .unwrap();
    let states: std::collections::BTreeMap<String, String> =
        project_progress(&root, 100, 0, None, None).unwrap()["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row["id"].as_str().unwrap().to_owned(),
                    row["state"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
    assert_eq!(states["custom:manual"], "not_run");
}

#[test]
fn checklist_attempts_are_journaled_for_the_desktop_journal() {
    let dir = scratch("journal");
    let root = project(&dir);
    edit(
        &root,
        json!({"id": "custom:attempt", "title": "Attempted check", "execution_kind": "manual"}),
    );
    record(
        &root,
        "custom:attempt",
        json!({"status": "unavailable", "reason": "Synthetic input missing"}),
    );
    let events = event_log::page(&root, 0, 50, None, "", None, false, 0).unwrap();
    let texts: Vec<&str> = events["items"]
        .as_array()
        .map(|items| items.iter().filter_map(|i| i["text"].as_str()).collect())
        .unwrap_or_default();
    assert!(
        texts
            .iter()
            .any(|t| t.contains("checklist custom:attempt: unavailable")),
        "events: {events}"
    );
    let log = std::fs::read_to_string(root.join("log.md")).unwrap();
    assert!(log.contains("checklist custom:attempt: unavailable"));
}

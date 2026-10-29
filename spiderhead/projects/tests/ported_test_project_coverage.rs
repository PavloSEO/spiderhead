//! Ported the reference suite cases that use only the public coverage API.
//!
//! Deferred (reference-only seams): `os.replace` interruption, `load_catalogue`
//! monkeypatching, and `_digest` call counting.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use spiderhead_projects::coverage::{
    coverage_status, initialize_coverage, record_execution, update_item,
};
use spiderhead_projects::workspace::{create_project, project_status};

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cov-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn project(dir: &Path) -> PathBuf {
    let root = dir.join("shop");
    create_project(&root, "https://example.test/", None, None, None, None).unwrap();
    initialize_coverage(&root, None).unwrap();
    root
}

fn edit(root: &Path, item: Value) -> Value {
    let revision = coverage_status(root, true).unwrap()["revision"]
        .as_i64()
        .unwrap();
    update_item(root, &item, revision).unwrap()
}

fn record(root: &Path, item_id: &str, entry: Value) -> Value {
    let revision = coverage_status(root, true).unwrap()["revision"]
        .as_i64()
        .unwrap();
    record_execution(root, item_id, &entry, revision).unwrap()
}

fn try_record(root: &Path, item_id: &str, entry: Value) -> Result<Value, String> {
    let revision = coverage_status(root, true).unwrap()["revision"]
        .as_i64()
        .unwrap();
    record_execution(root, item_id, &entry, revision).map_err(|e| e.to_string())
}

fn try_edit(root: &Path, item: Value) -> Result<Value, String> {
    let revision = coverage_status(root, true).unwrap()["revision"]
        .as_i64()
        .unwrap();
    update_item(root, &item, revision).map_err(|e| e.to_string())
}

fn row<'a>(status: &'a Value, item_id: &str) -> &'a Value {
    status["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"].as_str() == Some(item_id))
        .unwrap()
}

fn source_scan(root: &Path, name: &str) -> PathBuf {
    std::fs::create_dir_all(root.join("scans")).unwrap();
    let dest = root.join("scans").join(name);
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/crawl-evidence.sqlite"),
        &dest,
    )
    .unwrap();
    dest
}

fn coverage_bytes(root: &Path) -> Vec<u8> {
    std::fs::read(root.join("coverage.json")).unwrap()
}

const SCOPE: fn() -> Value = || {
    json!({
        "site": "https://example.test/",
        "template": "product",
        "urls": ["https://example.test/"],
    })
};

#[test]
fn ecommerce_history_and_completion_axes() {
    let dir = tmp("ecommerce");
    let project = project(&dir);
    let initial = coverage_status(&project, true).unwrap()["counts"]["remaining"]
        .as_i64()
        .unwrap();
    let template = json!({
        "format": "spiderhead.checklist-template.v1",
        "items": [
            {
                "id": "custom:product-auto",
                "title": "Product response check",
                "execution_kind": "automatic",
                "operation": "check:BROKEN_PAGE_4XX",
                "scope": SCOPE(),
            },
            {
                "id": "custom:product-review",
                "title": "Review product purchase flow",
                "dependencies": ["custom:product-auto"],
                "scope": SCOPE(),
            },
        ],
    });
    initialize_coverage(&project, Some(&template)).unwrap();
    assert_eq!(
        coverage_status(&project, true).unwrap()["counts"]["remaining"],
        json!(initial + 2)
    );
    let err = try_record(
        &project,
        "custom:product-review",
        json!({
            "status": "succeeded",
            "reason": "Reviewed purchase flow",
            "reviewer": "Specialist",
            "signoff": true,
        }),
    )
    .unwrap_err();
    assert!(err.contains("dependencies"), "{err}");
    source_scan(&project, "source.sqlite");
    let original = std::fs::read(project.join("scans/source.sqlite")).unwrap();
    let status = record(
        &project,
        "custom:product-auto",
        json!({
            "status": "succeeded",
            "reason": "Saved product sample checked",
            "artifact": "scans/source.sqlite",
        }),
    );
    let measured = row(&status, "custom:product-auto");
    assert!(measured["complete"].as_bool().unwrap());
    assert_eq!(measured["measurement"]["state"], "limited");
    assert_eq!(
        std::fs::read(project.join("scans/source.sqlite")).unwrap(),
        original
    );
    let status = record(
        &project,
        "custom:product-review",
        json!({
            "status": "succeeded",
            "reason": "Reviewed purchase flow",
            "reviewer": "Specialist",
            "signoff": true,
        }),
    );
    assert_eq!(status["counts"]["remaining"], json!(initial));
    let status = edit(
        &project,
        json!({"id": "custom:product-review", "title": "Review updated purchase flow"}),
    );
    assert!(row(&status, "custom:product-review")["stale"]
        .as_bool()
        .unwrap());
    assert_eq!(
        row(&status, "custom:product-review")["definition_versions"],
        2
    );
    assert_eq!(row(&status, "custom:product-review")["attempts"], 1);
    assert!(!row(&status, "custom:product-auto")["stale"]
        .as_bool()
        .unwrap());
    let status = record(
        &project,
        "custom:product-review",
        json!({
            "status": "not_applicable",
            "reason": "Purchase flow is out of the agreed scope",
            "reviewer": "Specialist",
            "evidence": "Agreed scope change order 2026-09-30",
        }),
    );
    assert_eq!(status["counts"]["remaining"], json!(initial));
    let reviewed = row(&status, "custom:product-review");
    assert_eq!(reviewed["applicability"], "excluded");
    assert_eq!(
        reviewed["exclusion"]["basis"],
        json!({"evidence": "Agreed scope change order 2026-09-30"})
    );
    assert_eq!(status["coverage"]["audit_tasks"]["excluded"], 1);
    assert_eq!(
        project_status(&project, None, true).unwrap()["checklist"],
        coverage_status(&project, true).unwrap()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn competitor_missing_failed_and_wrong_site_stay_pending() {
    let dir = tmp("competitor");
    let project = project(&dir);
    edit(
        &project,
        json!({
            "id": "custom:competitor",
            "execution_kind": "automatic",
            "operation": "check:BROKEN_PAGE_4XX",
            "scope": {"site": "https://competitor.test/", "template": null, "urls": []},
        }),
    );
    for status in ["failed", "unavailable", "running"] {
        let result = record(
            &project,
            "custom:competitor",
            json!({"status": status, "reason": "Competitor source pending"}),
        );
        assert!(!row(&result, "custom:competitor")["complete"]
            .as_bool()
            .unwrap());
    }
    let err = try_record(
        &project,
        "custom:competitor",
        json!({
            "status": "succeeded",
            "reason": "Attempt",
            "artifact": "scans/missing.sqlite",
        }),
    )
    .unwrap_err();
    assert!(err.contains("missing"), "{err}");
    source_scan(&project, "primary.sqlite");
    let err = try_record(
        &project,
        "custom:competitor",
        json!({
            "status": "succeeded",
            "reason": "Attempt",
            "artifact": "scans/primary.sqlite",
        }),
    )
    .unwrap_err();
    assert!(err.contains("site"), "{err}");
    assert!(!row(
        &coverage_status(&project, true).unwrap(),
        "custom:competitor"
    )["complete"]
        .as_bool()
        .unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn manual_and_deliverable_review_are_distinct() {
    let dir = tmp("deliverable");
    let project = project(&dir);
    edit(&project, json!({"id": "custom:manual"}));
    edit(
        &project,
        json!({"id": "custom:delivery", "execution_kind": "deliverable"}),
    );
    let output = project.join("reports/client.md");
    std::fs::create_dir_all(project.join("reports")).unwrap();
    std::fs::write(&output, "Reviewed synthetic client deliverable").unwrap();
    let err = try_record(
        &project,
        "custom:delivery",
        json!({
            "status": "succeeded",
            "reason": "Created file",
            "artifact": "reports/client.md",
            "reviewer": "Specialist",
        }),
    )
    .unwrap_err();
    assert!(err.contains("approved"), "{err}");
    let err = try_record(
        &project,
        "custom:delivery",
        json!({
            "status": "succeeded",
            "reason": "Signed",
            "reviewer": "Specialist",
            "signoff": true,
        }),
    )
    .unwrap_err();
    assert!(err.contains("approved"), "{err}");
    let result = record(
        &project,
        "custom:delivery",
        json!({
            "status": "succeeded",
            "reason": "Reviewed package",
            "artifact": "reports/client.md",
            "reviewer": "Specialist",
            "review": "approved",
        }),
    );
    assert!(result["views"]["deliverable_ready"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "custom:delivery"));
    record(
        &project,
        "custom:manual",
        json!({
            "status": "succeeded",
            "reason": "Reviewed flow",
            "reviewer": "Specialist",
            "signoff": true,
        }),
    );
    std::fs::write(&output, "Changed after review").unwrap();
    let status = coverage_status(&project, true).unwrap();
    assert!(row(&status, "custom:delivery")["stale"].as_bool().unwrap());
    assert!(row(&status, "custom:manual")["complete"].as_bool().unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn concurrent_conflicts_preserve_successful_write() {
    let dir = tmp("concurrent");
    let project = project(&dir);
    let revision = coverage_status(&project, true).unwrap()["revision"]
        .as_i64()
        .unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut handles = Vec::new();
    for n in 0..2 {
        let barrier = barrier.clone();
        let project = project.clone();
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            update_item(
                &project,
                &json!({"id": format!("custom:writer{n}")}),
                revision,
            )
            .is_ok()
        }));
    }
    let results: Vec<bool> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| **r).count(), 1);
    let status = coverage_status(&project, true).unwrap();
    assert_eq!(status["revision"], json!(revision + 1));
    assert_eq!(
        status["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["id"]
                .as_str()
                .unwrap_or_default()
                .starts_with("custom:writer"))
            .count(),
        1
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn invalid_definitions_do_not_change_file() {
    let items = vec![
        json!({"id": "custom:../bad"}),
        json!({"id": "custom:x", "execution_kind": "shell"}),
        json!({"id": "custom:x", "operation": "rm -rf"}),
        json!({"id": "custom:x", "priority": "P3"}),
        json!({"id": "custom:x", "enabled": 1}),
        json!({"id": "custom:x", "dependencies": ["custom:missing"]}),
        json!({
            "id": "custom:x",
            "scope": {
                "site": "https://example.test/",
                "template": null,
                "urls": ["https://competitor.test/"],
            },
        }),
    ];
    for item in items {
        let dir = tmp("invalid");
        let project = project(&dir);
        let before = coverage_bytes(&project);
        assert!(try_edit(&project, item).is_err());
        assert_eq!(coverage_bytes(&project), before);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn schema_identity_and_path_refusals() {
    let dir = tmp("schema");
    let project = project(&dir);
    let path = project.join("coverage.json");
    let original: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for (key, value) in [("version", json!(99)), ("project_uuid", json!("other"))] {
        let mut mutated = original.clone();
        mutated[key] = value;
        std::fs::write(&path, serde_json::to_string(&mutated).unwrap()).unwrap();
        assert!(coverage_status(&project, true).is_err());
    }
    std::fs::write(&path, serde_json::to_string(&original).unwrap()).unwrap();
    edit(&project, json!({"id": "custom:manual"}));
    for artifact in [
        "../outside",
        "/tmp/outside",
        "project.json",
        "reports/missing",
    ] {
        let err = try_record(
            &project,
            "custom:manual",
            json!({
                "status": "succeeded",
                "reason": "Review",
                "artifact": artifact,
                "reviewer": "Specialist",
                "review": "approved",
            }),
        )
        .unwrap_err();
        assert!(!err.is_empty());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn disable_and_order_do_not_erase_history() {
    let dir = tmp("disable");
    let project = project(&dir);
    edit(&project, json!({"id": "custom:x"}));
    record(
        &project,
        "custom:x",
        json!({
            "status": "succeeded",
            "reason": "Reviewed",
            "reviewer": "Specialist",
            "signoff": true,
        }),
    );
    let result = edit(
        &project,
        json!({"id": "custom:x", "enabled": false, "order": 999}),
    );
    assert_eq!(result["counts"]["disabled"], 1);
    assert_eq!(row(&result, "custom:x")["attempts"], 1);
    assert!(result["views"]["remaining"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "custom:x"));
    assert_eq!(
        row(&result, "custom:x")["applicability"],
        "pending_exclusion"
    );
    assert!(result["views"]["pending_exclusion"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "custom:x"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn malformed_execution_refuses_without_write() {
    for value in [Value::Null, json!([]), json!("bad"), json!({"status": []})] {
        let dir = tmp("malformed-exec");
        let project = project(&dir);
        let before = coverage_bytes(&project);
        let revision = coverage_status(&project, true).unwrap()["revision"]
            .as_i64()
            .unwrap();
        assert!(record_execution(&project, "check:BROKEN_PAGE_4XX", &value, revision).is_err());
        assert_eq!(coverage_bytes(&project), before);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn malformed_history_and_definitions_refuse() {
    let dir = tmp("malformed-history");
    let project = project(&dir);
    edit(&project, json!({"id": "custom:manual"}));
    record(
        &project,
        "custom:manual",
        json!({
            "status": "succeeded",
            "reason": "Reviewed",
            "reviewer": "Specialist",
            "signoff": true,
        }),
    );
    let path = project.join("coverage.json");
    let original = std::fs::read_to_string(&path).unwrap();
    for mutation in ["record", "definition", "approval"] {
        let mut doc: Value = serde_json::from_str(&original).unwrap();
        let item = &mut doc["items"]["custom:manual"];
        match mutation {
            "record" => {
                item["records"]
                    .as_array_mut()
                    .unwrap()
                    .last_mut()
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .remove("definition_hash");
            }
            "definition" => {
                item["definition"]["execution_kind"] = json!([]);
                item["definitions"]
                    .as_array_mut()
                    .unwrap()
                    .last_mut()
                    .unwrap()["definition"]["execution_kind"] = json!([]);
            }
            _ => {
                item["records"]
                    .as_array_mut()
                    .unwrap()
                    .last_mut()
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .remove("signoff");
            }
        }
        std::fs::write(&path, serde_json::to_string(&doc).unwrap()).unwrap();
        assert!(coverage_status(&project, true).is_err(), "{mutation}");
    }
    std::fs::write(&path, original).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn skipped_check_and_absent_template_never_complete() {
    let dir = tmp("skipped");
    let project = project(&dir);
    source_scan(&project, "source.sqlite");
    edit(
        &project,
        json!({
            "id": "custom:absent",
            "execution_kind": "automatic",
            "operation": "check:BROKEN_PAGE_4XX",
            "scope": {"site": "https://example.test/", "template": "product", "urls": []},
        }),
    );
    let err = try_record(
        &project,
        "custom:absent",
        json!({
            "status": "succeeded",
            "reason": "Try source",
            "artifact": "scans/source.sqlite",
        }),
    )
    .unwrap_err();
    assert!(err.contains("sample URLs"), "{err}");
    let con = spiderhead_store::scan_artifact::open_scan(
        &project.join("scans/source.sqlite"),
        false,
        None,
    )
    .unwrap();
    let document: String = con
        .query_row(
            "SELECT document_json FROM audit WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let audit: Value = serde_json::from_str(&document).unwrap();
    let skipped = audit["run"]["checks_skipped"][0]["id"].as_str().unwrap();
    let err = try_record(
        &project,
        &format!("check:{skipped}"),
        json!({
            "status": "succeeded",
            "reason": "Try source",
            "artifact": "scans/source.sqlite",
        }),
    )
    .unwrap_err();
    assert!(err.contains("does not prove"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn malformed_update_identifier_refuses_cleanly() {
    for identifier in [json!([]), json!({}), Value::Null] {
        let dir = tmp("malformed-id");
        let project = project(&dir);
        let before = coverage_bytes(&project);
        assert!(try_edit(&project, json!({"id": identifier})).is_err());
        assert_eq!(coverage_bytes(&project), before);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn agent_review_leaves_a_deliverable_waiting_for_owner_completion() {
    let dir = tmp("agent-review");
    let project = project(&dir);
    edit(
        &project,
        json!({"id": "custom:delivery", "execution_kind": "deliverable"}),
    );
    std::fs::create_dir_all(project.join("reports")).unwrap();
    std::fs::write(
        project.join("reports/client.md"),
        "Reviewed synthetic client deliverable",
    )
    .unwrap();
    let result = record(
        &project,
        "custom:delivery",
        json!({
            "status": "succeeded",
            "reason": "Agent reviewed package",
            "artifact": "reports/client.md",
            "reviewer": "Claude Code",
            "review": "approved",
        }),
    );
    assert_eq!(row(&result, "custom:delivery")["state"], "run");
    assert!(!row(&result, "custom:delivery")["complete"]
        .as_bool()
        .unwrap());
    assert!(result["views"]["waiting_for_manual_review"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "custom:delivery"));
    assert!(!result["views"]["deliverable_ready"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "custom:delivery"));
    let owner = record(
        &project,
        "custom:delivery",
        json!({
            "status": "succeeded",
            "reason": "Owner approved package",
            "artifact": "reports/client.md",
            "reviewer": "Pavel",
            "review": "approved",
        }),
    );
    assert!(row(&owner, "custom:delivery")["complete"]
        .as_bool()
        .unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

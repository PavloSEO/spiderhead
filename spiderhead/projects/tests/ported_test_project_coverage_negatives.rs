//! for project checklist writes and stale source definitions.
//!
//! The parametrized non-string identifiers/revisions (`[]`, `{"id": ...}`,
//! `object()`, `None`, `True`, `"1"`, `1.0`) cannot reach the typed Rust
//! signatures — the boundary validation is covered by the oracle dispatch
//! layer. The `load_catalogue` monkeypatch is a reference-only seam; both stay
//! deferred in `oracle/deferred-tests.toml`.

#[path = "common/testkit.rs"]
mod testkit;

use serde_json::{json, Value};
use spiderhead_projects::coverage::{coverage_status, initialize_coverage, record_execution};

fn initialized_project(name: &str) -> (testkit::Scratch, std::path::PathBuf) {
    let (dir, root) = testkit::project("covneg", name);
    initialize_coverage(&root, None).expect("initialize_coverage");
    (dir, root)
}

// (the string-expressible row; non-string rows are impossible at the Rust signature)
#[test]
fn malformed_item_identifier_refuses_before_any_checklist_write() {
    let (_dir, root) = initialized_project("identifier");
    let path = root.join("coverage.json");
    let before = std::fs::read(&path).expect("coverage.json");

    let err = record_execution(
        &root,
        "not an identifier",
        &json!({"status": "running", "reason": "Synthetic incomplete attempt"}),
        coverage_status(&root, true).expect("status")["revision"]
            .as_i64()
            .unwrap(),
    )
    .expect_err("must refuse");
    assert!(
        err.to_string().contains("checklist item identifier"),
        "got: {err}"
    );

    assert_eq!(std::fs::read(&path).expect("coverage.json"), before);
}

#[test]
fn malformed_definition_metadata_refuses_with_a_named_error() {
    for malformation in ["priority_origin", "definition_history"] {
        let (_dir, root) = initialized_project(malformation);
        let path = root.join("coverage.json");
        let mut document: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("coverage.json"))
                .expect("coverage json");
        let items = document["items"].as_object_mut().unwrap();
        let item = items.values_mut().next().unwrap();
        if malformation == "priority_origin" {
            item["definition"]["priority_origin"] = json!([]);
            item["definitions"]
                .as_array_mut()
                .unwrap()
                .last_mut()
                .unwrap()["definition"]["priority_origin"] = json!([]);
        } else {
            *item["definitions"]
                .as_array_mut()
                .unwrap()
                .last_mut()
                .unwrap() = json!([]);
        }
        std::fs::write(&path, serde_json::to_string(&document).unwrap()).expect("write");
        let before = std::fs::read(&path).expect("coverage.json");

        let err = coverage_status(&root, true).expect_err("must refuse");
        let message = err.to_string();
        assert!(
            message.contains("priority origin") || message.contains("definition"),
            "got: {message}"
        );
        assert_eq!(std::fs::read(&path).expect("coverage.json"), before);
    }
}

//! `the reference suite.raises(match=)` becomes error-message substring assertions.

#[path = "common/testkit.rs"]
mod testkit;

use serde_json::json;
use spiderhead_projects::coverage::{coverage_status, initialize_coverage_full, population};
use spiderhead_projects::workspace::create_project;

const SITE: &str = "https://example.test/";

#[test]
fn sample_needs_only_kind_size_name_and_source() {
    let value = population(
        &json!({"kind": "sample", "size": 50, "name": "top sellers", "source": "memo"}),
        SITE,
        false,
    )
    .expect("population");
    assert_eq!(value["kind"], "sample");
    assert_eq!(value["size"], 50);
    assert_eq!(value["urls"], json!([]));
    assert!(value["reason"].is_null());
    assert_eq!(value["templates"], json!({}));
}

#[test]
fn missing_source_is_named() {
    let err = population(&json!({"kind": "complete_set", "size": 10}), SITE, false)
        .expect_err("missing source");
    assert!(
        err.to_string().contains("missing required key(s): source"),
        "got: {err}"
    );
}

#[test]
fn missing_kind_is_named() {
    let err =
        population(&json!({"size": 10, "source": "memo"}), SITE, false).expect_err("missing kind");
    assert!(
        err.to_string().contains("missing required key(s): kind"),
        "got: {err}"
    );
}

#[test]
fn sample_without_size_or_urls_names_size() {
    let err = population(
        &json!({"kind": "sample", "name": "x", "source": "memo"}),
        SITE,
        false,
    )
    .expect_err("missing size");
    assert!(
        err.to_string()
            .contains("missing required key(s): size (or urls)"),
        "got: {err}"
    );
}

#[test]
fn unknown_key_is_named_with_the_allowed_set() {
    let err = population(
        &json!({"kind": "complete_set", "size": 10, "source": "memo", "colour": "red"}),
        SITE,
        false,
    )
    .expect_err("unknown key");
    let text = err.to_string();
    assert!(text.contains("unsupported key(s): colour"), "got: {text}");
    assert!(text.contains("allowed:"), "got: {text}");
}

#[test]
fn partial_population_in_a_checklist_plan_is_stored() {
    let dir = testkit::scratch("population", "plan");
    let root = dir.0.join("project");
    create_project(&root, SITE, None, None, None, None).expect("create");
    initialize_coverage_full(
        &root,
        None,
        None,
        Some(&json!({
            "reviewer": "Lead auditor",
            "population": {"kind": "complete_set", "size": 300, "source": "crawl memo"},
        })),
    )
    .expect("initialize");
    let stored = coverage_status(&root, false).expect("status")["plan"]["population"].clone();
    assert_eq!(stored["kind"], "complete_set");
    assert_eq!(stored["size"], 300);
    assert_eq!(stored["source"], "crawl memo");
}

#[test]
fn nested_template_population_defaults_apply() {
    let value = population(
        &json!({
            "kind": "complete_set",
            "size": 10,
            "source": "memo",
            "templates": {
                "product": {"kind": "sample", "size": 4, "name": "p", "source": "memo"}
            },
        }),
        SITE,
        false,
    )
    .expect("population");
    assert_eq!(value["templates"]["product"]["size"], 4);
    assert_eq!(value["templates"]["product"]["urls"], json!([]));
}

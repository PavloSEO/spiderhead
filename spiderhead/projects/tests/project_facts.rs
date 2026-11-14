//! `tools` callables; Rust uses the same seam through the `DetectTools` trait.

#[path = "common/testkit.rs"]
mod testkit;

use std::cell::RefCell;

use serde_json::{json, Value};
use spiderhead_projects::coverage::initialize_coverage;
use spiderhead_projects::facts::{project_facts, DetectTools, DETECTED_PREFIX};
use spiderhead_projects::priorities::project_priorities;
use spiderhead_projects::workspace::create_project;

/// `_tools`: injected stand-ins for the shared single-page tools.
struct Tools {
    technologies: Vec<Value>,
    robots: Option<Value>,
    detection: Option<Value>,
    calls: RefCell<Vec<String>>,
}

impl Tools {
    fn new() -> Self {
        Self {
            technologies: Vec::new(),
            robots: None,
            detection: None,
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl DetectTools for Tools {
    fn robots_check(&self, _url: &str, _paths: &[String]) -> Value {
        self.calls.borrow_mut().push("robots_check".into());
        self.robots
            .clone()
            .unwrap_or_else(|| json!({"ok": true, "path_checks": []}))
    }

    fn tech_detect(&self, _url: &str) -> Value {
        self.calls.borrow_mut().push("tech_detect".into());
        if let Some(d) = &self.detection {
            return d.clone();
        }
        let mut by_category = serde_json::Map::new();
        for row in &self.technologies {
            let key = row["category"].as_str().unwrap().to_string();
            by_category
                .entry(key)
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .unwrap()
                .push(row.clone());
        }
        json!({"ok": true, "by_category": Value::Object(by_category)})
    }
}

fn project(dir: &testkit::Scratch) -> std::path::PathBuf {
    let root = dir.0.join("project");
    create_project(&root, "https://example.test/", None, None, None, None).expect("create");
    root
}

#[test]
fn detection_records_stack_facts_that_priorities_then_read() {
    let dir = testkit::scratch("facts", "stack");
    let root = project(&dir);
    let mut tools = Tools::new();
    tools.technologies = vec![
        json!({"category": "framework", "name": "Next.js", "evidence": "html: /_next/static"}),
        json!({"category": "cms", "name": "WordPress", "evidence": "html: /wp-content/"}),
    ];

    let result = project_facts(&root, None, true, true, Some(&tools)).expect("facts");

    assert_eq!(result["applied"], true);
    assert_eq!(result["detection"]["state"], "run");
    let recorded: std::collections::HashMap<_, _> = result["facts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (f["name"].as_str().unwrap().to_string(), f))
        .collect();
    assert_eq!(recorded["framework"]["value"], "Next.js");
    assert!(recorded["framework"]["provenance"]
        .as_str()
        .unwrap()
        .starts_with(DETECTED_PREFIX));
    assert_eq!(
        recorded["framework"]["observed_at"],
        result["detection"]["observed_at"]
    );
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("project.json")).expect("manifest"),
    )
    .expect("manifest json");
    assert_eq!(manifest["facts"], result["facts"]);

    initialize_coverage(&root, None).expect("coverage");
    let decisions = project_priorities(&root, None, false, None, true).expect("priorities");
    let decision = decisions["decisions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "skill:workflow/js-render-check")
        .expect("js-render-check decision");
    assert_eq!(
        decision["after"],
        json!({"priority": "P0", "priority_origin": "policy"})
    );
    assert_eq!(decision["consulted_facts"], json!(["framework"]));
}

#[test]
fn supplied_facts_are_recorded_as_operator_decisions() {
    let dir = testkit::scratch("facts", "operator");
    let root = project(&dir);
    let result = project_facts(
        &root,
        Some(&json!([{"name": "site_type", "value": "publisher", "provenance": "client brief", "observed_at": Value::Null}])),
        false,
        true,
        None,
    )
    .expect("facts");
    assert_eq!(result["applied"], true);
    assert_eq!(
        result["changes"],
        json!([{
            "name": "site_type",
            "action": "recorded",
            "value": "publisher",
            "provenance": "client brief",
            "reason": "operator-entered decision",
        }])
    );
    assert_eq!(result["detection"]["state"], "not_run");
}

#[test]
fn detection_never_overwrites_an_operator_entered_fact() {
    let dir = testkit::scratch("facts", "decided");
    let root = dir.0.join("decided");
    create_project(
        &root,
        "https://example.test/",
        None,
        Some(&json!([{"name": "cms", "value": "WordPress", "provenance": "client", "observed_at": Value::Null}])),
        None,
        None,
    )
    .expect("create");
    let before = std::fs::read(root.join("project.json")).expect("manifest");
    let mut tools = Tools::new();
    tools.technologies =
        vec![json!({"category": "cms", "name": "Drupal", "evidence": "header: x-drupal-cache"})];

    let result = project_facts(&root, None, true, true, Some(&tools)).expect("facts");

    assert_eq!(result["applied"], false);
    assert_eq!(
        std::fs::read(root.join("project.json")).expect("manifest"),
        before
    );
    assert_eq!(
        result["changes"],
        json!([{
            "name": "cms",
            "action": "kept_operator",
            "value": "WordPress",
            "provenance": "client",
            "detected_value": "Drupal",
            "reason": "an operator-entered fact is a decision; detection is evidence",
        }])
    );
}

#[test]
fn a_later_detection_refreshes_its_own_earlier_evidence() {
    let dir = testkit::scratch("facts", "refresh");
    let root = project(&dir);
    let mut tools = Tools::new();
    tools.technologies =
        vec![json!({"category": "cms", "name": "Drupal", "evidence": "header: x-drupal-cache"})];
    project_facts(&root, None, true, true, Some(&tools)).expect("first");

    let again = project_facts(&root, None, true, true, Some(&tools)).expect("again");

    assert_eq!(
        again["changes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["action"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["confirmed"]
    );
    assert_eq!(again["facts"][0]["value"], "Drupal");
}

#[test]
fn an_unavailable_detection_leaves_the_fact_absent_with_a_reason() {
    let dir = testkit::scratch("facts", "unavailable");
    let root = project(&dir);
    let mut tools = Tools::new();
    tools.detection = Some(json!({"ok": false, "error": "connection refused"}));

    let result = project_facts(&root, None, true, true, Some(&tools)).expect("facts");

    assert_eq!(result["detection"]["state"], "unavailable");
    assert!(result["detection"]["reason"]
        .as_str()
        .unwrap()
        .contains("connection refused"));
    assert_eq!(result["facts"], json!([]));
    assert_eq!(result["applied"], false);
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("project.json")).expect("manifest"),
    )
    .expect("manifest json");
    assert_eq!(manifest["facts"], json!([]));
}

#[test]
fn a_disallowed_target_is_not_fetched() {
    let dir = testkit::scratch("facts", "disallowed");
    let root = project(&dir);
    let mut tools = Tools::new();
    tools.robots = Some(json!({"ok": true, "path_checks": [{"path": "/", "allowed": false}]}));
    tools.technologies = vec![json!({"category": "cms", "name": "Drupal", "evidence": "x"})];

    let result = project_facts(&root, None, true, false, Some(&tools)).expect("facts");

    assert_eq!(result["detection"]["state"], "unavailable");
    assert!(result["detection"]["reason"]
        .as_str()
        .unwrap()
        .contains("robots.txt disallows"));
    assert_eq!(*tools.calls.borrow(), vec!["robots_check".to_string()]);
}

#[test]
fn unreadable_robots_rules_are_a_refusal_to_fetch() {
    let dir = testkit::scratch("facts", "robots503");
    let root = project(&dir);
    let mut tools = Tools::new();
    tools.robots =
        Some(json!({"ok": false, "error": "robots.txt returned 503; rules could not be read"}));

    let result = project_facts(&root, None, true, false, Some(&tools)).expect("facts");

    assert_eq!(result["detection"]["state"], "unavailable");
    assert!(result["detection"]["reason"]
        .as_str()
        .unwrap()
        .contains("503"));
    assert_eq!(*tools.calls.borrow(), vec!["robots_check".to_string()]);
}

#[test]
fn ambiguous_candidates_are_reported_instead_of_guessed() {
    let dir = testkit::scratch("facts", "ambiguous");
    let root = project(&dir);
    let mut tools = Tools::new();
    tools.technologies = vec![
        json!({"category": "cms", "name": "WordPress", "evidence": "html: /wp-content/"}),
        json!({"category": "framework", "name": "Nuxt", "evidence": "html: /_nuxt/"}),
        json!({"category": "framework", "name": "React", "evidence": "html: data-reactroot"}),
    ];

    let result = project_facts(&root, None, true, true, Some(&tools)).expect("facts");

    assert_eq!(result["detection"]["state"], "partial");
    assert_eq!(
        result["facts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["cms"]
    );
    assert_eq!(
        result["detection"]["unavailable"],
        json!([{
            "name": "framework",
            "reason": "2 framework candidates matched (Nuxt, React); not guessed",
        }])
    );
}

#[test]
fn a_generator_version_label_is_not_a_second_candidate() {
    let dir = testkit::scratch("facts", "version-label");
    let root = project(&dir);
    let mut tools = Tools::new();
    tools.technologies = vec![
        json!({"category": "cms", "name": "WordPress", "evidence": "html: /wp-content/"}),
        json!({"category": "cms", "name": "WordPress 6.4.2", "evidence": "meta name=generator"}),
    ];

    let result = project_facts(&root, None, true, false, Some(&tools)).expect("facts");

    assert_eq!(
        result["facts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| (f["name"].as_str().unwrap(), f["value"].as_str().unwrap()))
            .collect::<Vec<_>>(),
        vec![("cms", "WordPress")]
    );
}

#[test]
fn no_signature_match_names_the_missing_category() {
    let dir = testkit::scratch("facts", "no-match");
    let root = project(&dir);
    let tools = Tools::new();

    let result = project_facts(&root, None, true, false, Some(&tools)).expect("facts");

    assert_eq!(result["detection"]["state"], "partial");
    assert_eq!(result["facts"], json!([]));
    let unavailable = result["detection"]["unavailable"].as_array().unwrap();
    assert_eq!(unavailable[0]["name"], "cms");
    assert_eq!(unavailable[1]["name"], "framework");
    assert!(unavailable[0]["reason"]
        .as_str()
        .unwrap()
        .contains("no cms signature matched"));
}

#[test]
fn preview_is_offline_and_writes_nothing() {
    let dir = testkit::scratch("facts", "preview");
    let root = project(&dir);
    let before = std::fs::read(root.join("project.json")).expect("manifest");

    // A preview without detection must not call a tool; Rust proves it by
    // construction — the trait impl is never invoked without `detect`.
    let result = project_facts(
        &root,
        Some(&json!([{"name": "cms", "value": "MODX", "provenance": "client", "observed_at": Value::Null}])),
        false,
        false,
        None,
    )
    .expect("facts");

    assert_eq!(result["applied"], false);
    assert_eq!(
        result["detection"],
        json!({
            "state": "not_run",
            "reason": "detection was not requested; no request was made",
            "facts": [],
            "unavailable": [],
        })
    );
    assert_eq!(
        std::fs::read(root.join("project.json")).expect("manifest"),
        before
    );
}

#[test]
fn supplied_facts_cannot_claim_detection_provenance() {
    let dir = testkit::scratch("facts", "forged-provenance");
    let root = project(&dir);
    let err = project_facts(
        &root,
        Some(&json!([{
            "name": "cms",
            "value": "WordPress",
            "provenance": format!("{DETECTED_PREFIX} at https://example.test/"),
            "observed_at": Value::Null,
        }])),
        false,
        false,
        None,
    )
    .expect_err("forged provenance refuses");
    assert!(
        err.to_string().contains("claim detection provenance"),
        "got: {err}"
    );
}

#[test]
fn no_input_lists_the_saved_facts() {
    let dir = testkit::scratch("facts", "list");
    let root = project(&dir);
    let result = project_facts(&root, None, false, false, None).expect("facts");
    assert_eq!(result["ok"], true);
    assert_eq!(result["state"], "saved");
    assert!(result["facts"].is_array());
    assert_eq!(
        result["count"].as_i64().unwrap() as usize,
        result["facts"].as_array().unwrap().len()
    );
}

#[test]
fn detection_requires_its_injected_tools() {
    let dir = testkit::scratch("facts", "tools-required");
    let root = project(&dir);
    let err =
        project_facts(&root, None, true, false, None).expect_err("detection without tools refuses");
    assert!(
        err.to_string()
            .contains("requires an injected robots_check tool"),
        "got: {err}"
    );
}

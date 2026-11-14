//! and a summary of its semantic core, never the core itself. Detection is
//! injected and never makes a request.

#[path = "common/testkit.rs"]
mod testkit;

use serde_json::{json, Value};
use spiderhead_projects::facts::project_facts;

fn semcore_facts() -> Value {
    json!([
        {
            "name": "semcore_project",
            "value": "semantics/semcore/example-shop",
            "provenance": "operator: semantic core project directory",
            "observed_at": null,
        },
        {
            "name": "semcore_cluster_count",
            "value": 42,
            "provenance": "operator: semcore report summary",
            "observed_at": null,
        },
        {
            "name": "semcore_landing_mapping",
            "value": "38 of 42 clusters mapped to a landing page",
            "provenance": "operator: semcore landing-page mapping summary",
            "observed_at": null,
        },
    ])
}

fn saved_facts(root: &std::path::Path) -> Vec<Value> {
    let document: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("project.json")).expect("project.json"),
    )
    .expect("project json");
    document["facts"].as_array().cloned().unwrap_or_default()
}

#[test]
fn semcore_link_is_recorded_as_operator_facts_and_read_back() {
    let (_dir, root) = testkit::project("semcore", "record");

    let result =
        project_facts(&root, Some(&semcore_facts()), false, true, None).expect("project_facts");

    assert_eq!(result["applied"], true);
    let actions: Vec<&str> = result["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["action"].as_str().unwrap())
        .collect();
    assert_eq!(actions, ["recorded", "recorded", "recorded"]);
    let saved = saved_facts(&root);
    let recorded: std::collections::BTreeMap<&str, &Value> = saved
        .iter()
        .map(|f| (f["name"].as_str().unwrap(), &f["value"]))
        .collect();
    assert_eq!(
        recorded["semcore_project"],
        &json!("semantics/semcore/example-shop")
    );
    assert_eq!(recorded["semcore_cluster_count"], &json!(42));
    assert_eq!(
        recorded["semcore_landing_mapping"],
        &json!("38 of 42 clusters mapped to a landing page")
    );
}

#[test]
fn semcore_link_is_a_current_value_when_updated() {
    let (_dir, root) = testkit::project("semcore", "update");
    project_facts(&root, Some(&semcore_facts()), false, true, None).expect("project_facts");
    let updated = json!([{
        "name": "semcore_cluster_count",
        "value": 45,
        "provenance": "operator: semcore report summary",
        "observed_at": null,
    }]);

    let result = project_facts(&root, Some(&updated), false, true, None).expect("project_facts");

    let actions: Vec<&str> = result["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["action"].as_str().unwrap())
        .collect();
    assert_eq!(actions, ["updated"]);
    let saved = saved_facts(&root);
    let values: Vec<&Value> = saved
        .iter()
        .filter(|f| f["name"] == "semcore_cluster_count")
        .map(|f| &f["value"])
        .collect();
    assert_eq!(values, [&json!(45)]);
    assert_eq!(saved.len(), 3);
}

#[test]
fn semcore_link_refuses_to_claim_detection_provenance() {
    let (_dir, root) = testkit::project("semcore", "forged");
    let forged = json!([{
        "name": "semcore_project",
        "value": "semantics/semcore/example-shop",
        "provenance": "detected by tech-detect at https://example.test/",
        "observed_at": null,
    }]);

    let err = project_facts(&root, Some(&forged), false, true, None).expect_err("must refuse");
    assert!(
        err.to_string().contains("detection provenance"),
        "got: {err}"
    );

    assert!(saved_facts(&root).is_empty());
}

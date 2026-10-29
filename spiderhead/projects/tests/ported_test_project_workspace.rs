//! monkeypatch case and the cli-alias case stay deferred; the settings
//! overlay case writes a real `project-settings.json` instead of patching
//! `_settings`.

#[path = "common/testkit.rs"]
mod testkit;

use serde_json::{json, Value};
use spiderhead_projects::workspace::{create_project, open_project, open_tasks, project_status};

#[test]
fn create_open_status_and_move_preserve_relative_artifact_references() {
    let dir = testkit::scratch("workspace", "move");
    let project = dir.0.join("example");
    let created = create_project(
        &project,
        "https://example.test/",
        None,
        Some(&json!([{"provenance": "fixture", "observed_at": Value::Null, "value": "synthetic", "name": "cms"}])),
        Some(&json!(["ecommerce/product-card"])),
        Some(&json!(["ecommerce/basic"])),
    )
    .expect("create_project");
    assert_eq!(created["project"]["artifact_directories"]["scans"], "scans");
    assert!(created["project"]["project_uuid"].as_str().is_some());
    assert!(created["project"]["created_at"]
        .as_str()
        .unwrap_or_default()
        .ends_with('Z'));
    let status = project_status(&project, None, false).expect("status");
    assert_eq!(status["checklist"]["state"], "not_initialized");
    assert!(!status.to_string().contains("0/0"));
    assert_eq!(status["preparation"]["state"], "pending");

    let moved = dir.0.join("moved");
    std::fs::rename(&project, &moved).expect("move");
    assert_eq!(
        open_project(&moved, None).expect("open")["project"]["site"]["target"],
        "https://example.test/"
    );
    assert_eq!(
        project_status(&moved, None, false).expect("status")["scans"]["items"],
        json!([])
    );
}

#[test]
fn status_keeps_unmeasured_operational_fields_explicit() {
    let dir = testkit::scratch("workspace", "unmeasured");
    let project = dir.0.join("status");
    create_project(&project, "https://example.test/", None, None, None, None)
        .expect("create_project");
    let status = project_status(&project, None, false).expect("status");
    assert!(status["summary"]["running_scan"]["running"].is_null());
    assert!(status["summary"]["running_scan"]["reason"]
        .as_str()
        .is_some());
    assert!(status["summary"]["open_tasks"]["count"].is_null());
    assert!(status["summary"]["open_tasks"]["reason"].as_str().is_some());
    assert!(status["summary"]["latest_scan"]["uuid"].is_null());
    assert!(status["summary"]["latest_scan"]["reason"]
        .as_str()
        .is_some());
    assert_eq!(status["summary"]["folder_health"]["state"], "ok");
    assert!(status["agent_state"]["connected"].is_null());
    assert!(status["agent_state"]["last_seen_at"].is_null());
    assert!(status["agent_state"]["reason"].as_str().is_some());
    assert_eq!(status["storage"]["relative_path"], "scans");
    assert!(status["storage"]["total_bytes"].is_null());
    assert!(status["storage"]["scan_count"].is_null());
    assert!(status["storage"]["reason"].as_str().is_some());
}

#[test]
fn status_counts_only_current_applicable_custom_tasks() {
    let result = open_tasks(&json!({
        "state": "initialized",
        "items": [
            {"id": "custom:open", "complete": false, "stale": false},
            {"id": "custom:done", "complete": true, "stale": false},
            {"id": "custom:stale", "complete": false, "stale": true},
            {"id": "custom:excluded", "complete": false, "stale": false, "applicability": "excluded"},
            {"id": "check:missing-title", "complete": false, "stale": false},
        ],
    }));
    assert_eq!(result["count"], 1);
    assert_eq!(result["state"], "measured");
    assert_eq!(result["scope"], "primary_project_custom_checklist");
}

#[test]
fn settings_overlay_is_returned_without_rewriting_project_manifest() {
    let dir = testkit::scratch("workspace", "settings-overlay");
    let project = dir.0.join("settings");
    create_project(
        &project,
        "https://example.test/",
        Some("Manifest label"),
        None,
        None,
        None,
    )
    .expect("create_project");
    let before = std::fs::read(project.join("project.json")).expect("manifest");
    let uuid = serde_json::from_str::<Value>(
        &std::fs::read_to_string(project.join("project.json")).expect("manifest text"),
    )
    .expect("manifest json")["project_uuid"]
        .clone();
    std::fs::write(
        project.join("project-settings.json"),
        serde_json::to_string_pretty(&json!({
            "format": "spiderhead.project-settings.v1",
            "version": 1,
            "project_uuid": uuid,
            "revision": 7,
            "last_operation_id": Value::Null,
            "settings": {"label": "Current label"},
        }))
        .expect("settings json"),
    )
    .expect("settings write");
    let opened = open_project(&project, None).expect("open");
    let status = project_status(&project, None, false).expect("status");
    assert_eq!(opened["project"]["site"]["label"], "Current label");
    assert_eq!(opened["project"]["settings_revision"], 7);
    assert_eq!(status["project"]["site"]["label"], "Current label");
    assert_eq!(status["project"]["settings_revision"], 7);
    assert_eq!(
        std::fs::read(project.join("project.json")).expect("manifest"),
        before
    );
}

#[test]
fn creation_never_overwrites_and_conflicting_identity_refuses() {
    let dir = testkit::scratch("workspace", "identity");
    let project = dir.0.join("example");
    create_project(&project, "https://example.test/", None, None, None, None)
        .expect("create_project");
    let before = std::fs::read(project.join("project.json")).expect("manifest");
    let err = create_project(&project, "https://other.test/", None, None, None, None)
        .expect_err("existing dir refuses");
    assert!(err.to_string().contains("already exists"), "got: {err}");
    assert_eq!(
        std::fs::read(project.join("project.json")).expect("manifest"),
        before
    );
    let err = open_project(&project, Some("other.test")).expect_err("expected_site conflict");
    assert!(err.to_string().contains("conflicts"), "got: {err}");
}

#[test]
fn malformed_or_incomplete_projects_never_open_as_successful() {
    for mutate in ["rmdir", "bad-version"] {
        let dir = testkit::scratch("workspace", mutate);
        let project = dir.0.join("example");
        create_project(&project, "https://example.test/", None, None, None, None)
            .expect("create_project");
        match mutate {
            "rmdir" => std::fs::remove_dir(project.join("scans")).expect("rmdir scans"),
            _ => std::fs::write(project.join("project.json"), "{\"version\":999}")
                .expect("bad manifest"),
        }
        open_project(&project, None).expect_err("must refuse");
    }
}

#[test]
fn references_and_facts_refuse_traversal_and_secrets() {
    let dir = testkit::scratch("workspace", "unsafe-refs");
    let err = create_project(
        &dir.0.join("unsafe"),
        "https://example.test/",
        None,
        None,
        Some(&json!(["../run"])),
        None,
    )
    .expect_err("traversal refuses");
    assert!(err.to_string().contains("relative"), "got: {err}");
    let err = create_project(
        &dir.0.join("secrets"),
        "https://example.test/",
        None,
        Some(&json!([{"name": "api_token", "value": "x", "provenance": "fixture", "observed_at": Value::Null}])),
        None,
        None,
    )
    .expect_err("secret fact refuses");
    assert!(err.to_string().contains("credentials"), "got: {err}");
}

#[test]
fn project_path_rejects_symlink_escape() {
    let dir = testkit::scratch("workspace", "symlink-escape");
    let outside = dir.0.join("outside");
    std::fs::create_dir_all(&outside).expect("outside");
    #[cfg(unix)]
    {
        let link = dir.0.join("project-link");
        std::os::unix::fs::symlink(&outside, &link).expect("symlink");
        let err = create_project(&link, "https://example.test/", None, None, None, None)
            .expect_err("symlinked dir refuses");
        assert!(err.to_string().contains("already exists"), "got: {err}");
    }
    let err = create_project(
        &dir.0.join("nested").join("..").join("outside"),
        "https://example.test/",
        None,
        None,
        None,
        None,
    )
    .expect_err("traversal refuses");
    assert!(err.to_string().contains("traversal"), "got: {err}");
}

#[test]
fn facts_are_key_order_independent_and_preserve_scalar_values() {
    let dir = testkit::scratch("workspace", "facts-order");
    let created = create_project(
        &dir.0.join("facts"),
        "https://example.test/",
        None,
        Some(&json!([
            {"value": "text", "observed_at": Value::Null, "name": "string", "provenance": "fixture"},
            {"provenance": "fixture", "name": "integer", "value": 7, "observed_at": "2026-01-02T03:04:05Z"},
            {"name": "boolean", "value": true, "provenance": "fixture", "observed_at": Value::Null},
        ])),
        None,
        None,
    )
    .expect("create_project");
    assert_eq!(
        created["project"]["facts"],
        json!([
            {"name": "string", "value": "text", "provenance": "fixture", "observed_at": Value::Null},
            {"name": "integer", "value": 7, "provenance": "fixture", "observed_at": "2026-01-02T03:04:05Z"},
            {"name": "boolean", "value": true, "provenance": "fixture", "observed_at": Value::Null},
        ])
    );
}

#[test]
fn site_label_never_overrides_target_host_and_boolean_version_refuses() {
    let dir = testkit::scratch("workspace", "site-label");
    let project = dir.0.join("labeled");
    create_project(
        &project,
        "https://Example.test/",
        Some("Client label"),
        None,
        None,
        None,
    )
    .expect("create_project");
    assert_eq!(
        open_project(&project, Some("example.test")).expect("open")["project"]["site"],
        json!({"host": "example.test", "target": "https://example.test/", "label": "Client label"})
    );
    let mut document: Value = serde_json::from_str(
        &std::fs::read_to_string(project.join("project.json")).expect("manifest"),
    )
    .expect("manifest json");
    document["version"] = json!(true);
    std::fs::write(
        project.join("project.json"),
        serde_json::to_string(&document).expect("dump"),
    )
    .expect("write");
    let err = open_project(&project, None).expect_err("bool version refuses");
    assert!(err.to_string().contains("version"), "got: {err}");
}

#[test]
fn unknown_or_wrongly_typed_site_identity_refuses_cleanly() {
    for site in [
        json!({"id": 7, "target": "https://example.test/"}),
        json!({"host": "example.test", "label": 7, "target": "https://example.test/"}),
    ] {
        let dir = testkit::scratch("workspace", "site-identity");
        let project = dir.0.join("identity");
        create_project(&project, "https://example.test/", None, None, None, None)
            .expect("create_project");
        let mut document: Value = serde_json::from_str(
            &std::fs::read_to_string(project.join("project.json")).expect("manifest"),
        )
        .expect("manifest json");
        document["site"] = site;
        std::fs::write(
            project.join("project.json"),
            serde_json::to_string(&document).expect("dump"),
        )
        .expect("write");
        let err = open_project(&project, None).expect_err("bad identity refuses");
        assert!(err.to_string().contains("site identity"), "got: {err}");
    }
}

#[test]
fn credential_fact_names_refuse_before_project_creation() {
    for name in [
        "api_key",
        "service_api_key",
        "apikey",
        "private_key",
        "credentials",
    ] {
        let dir = testkit::scratch("workspace", "credential-names");
        let project = dir.0.join("secrets");
        let err = create_project(
            &project,
            "https://example.test/",
            None,
            Some(&json!([{"name": name, "value": "synthetic", "provenance": "operator", "observed_at": Value::Null}])),
            None,
            None,
        )
        .expect_err("credential name refuses");
        assert!(err.to_string().contains("credentials"), "{name}: {err}");
        assert!(!project.exists());
    }
}

#[test]
fn keyword_fact_names_are_not_mistaken_for_credentials() {
    let dir = testkit::scratch("workspace", "keyword-facts");
    let result = create_project(
        &dir.0.join("keywords"),
        "https://example.test/",
        None,
        Some(&json!([{"name": "keyword_count", "value": 5, "provenance": "operator", "observed_at": Value::Null}])),
        None,
        None,
    )
    .expect("create_project");
    assert_eq!(result["project"]["facts"][0]["value"], 5);
}

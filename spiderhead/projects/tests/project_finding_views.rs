//! parametrized expansions). The report-builder cases (`build_report`
//! md/csv/xlsx/docx/pdf), the scan-reader case (`findings_view` handler), and
//! the cli/mcp contract cases stay deferred — they exercise the reports
//! dispatcher, store `read_audit`, and cli/mcp surfaces, not the
//! finding_views store itself.

#[path = "common/testkit.rs"]
mod testkit;

use serde_json::{json, Value};
use spiderhead_projects::finding_views::{
    apply_view_to_audit, delete_view, list_views, rename_view, save_view, show_view,
};
use spiderhead_projects::workspace::create_project;

const SITE: &str = "https://example.test/";
const SCHEMA: &str = "spiderhead.site-audit/1";
const VIEWS_FILE: &str = "finding-views.json";

fn definition(values: Value) -> Value {
    let mut base = json!({
        "name": "triage",
        "filters": {},
        "sort": {"field": "severity", "direction": "desc"},
        "columns": ["severity", "check", "url", "text"],
        "page_size": 2,
    });
    if let Value::Object(extra) = values {
        for (k, v) in extra {
            base[k] = v;
        }
    }
    base
}

fn audit_document() -> Value {
    json!({
        "schema": SCHEMA,
        "domain": "example.test",
        "url": SITE,
        "generated_at": "2026-10-03T12:00:00Z",
        "run": {
            "scan_uuid": "scan-synthetic-757",
            "finding_exclusion_policy": [
                {"id": "synthetic-rule", "reason": "synthetic source exclusion"}
            ],
            "crawl_config": {
                "scope.segments": [{"name": "blog", "prefix": "/blog/"}],
                "scope.segments_only": [],
            },
        },
        "findings": [
            {
                "severity": "critical",
                "check": "BROKEN_PAGE_4XX",
                "url": "https://example.test/blog/a",
                "text": "Synthetic broken URL",
                "status_code": 404,
                "occurrences_count": 1,
                "fix_hint": "Repair the fixture link",
            },
            {
                "severity": "warning",
                "check": "MISSING_TITLE",
                "url": "https://example.test/blog/b",
                "text": "Synthetic missing title",
                "status_code": 200,
                "occurrences_count": 1,
                "fix_hint": "Add a title",
            },
            {
                "severity": "warning",
                "check": "MISSING_TITLE",
                "url": "https://example.test/blog/c",
                "text": "Synthetic second missing title",
                "status_code": 200,
                "occurrences_count": 1,
                "fix_hint": "Add a title",
            },
            {
                "severity": "warning",
                "check": "MISSING_TITLE",
                "text": "Audit-wide fixture without a target URL",
            },
        ],
        "pages": [
            {"url": "https://example.test/blog/a", "status": 404, "title": "A"},
            {"url": "https://example.test/blog/b", "status": 200, "title": "B"},
            {"url": "https://example.test/blog/c", "status": 200, "title": "C"},
        ],
        "summary": {
            "pages_checked": 3,
            "findings_total": 4,
            "findings_by_severity": {"critical": 1, "warning": 3, "notice": 0},
            "finding_exclusions": {
                "rules_configured": 1,
                "suppressed_total": 2,
                "by_rule": {"synthetic-rule": 2},
                "by_check": {"SYNTHETIC_SUPPRESSED": 2},
                "by_severity": {"notice": 2},
            },
        },
    })
}

fn project(dir: &testkit::Scratch) -> std::path::PathBuf {
    let root = dir.0.join("project");
    create_project(&root, SITE, None, None, None, None).expect("create");
    root
}

#[test]
fn view_round_trip_identity_revisions_and_expected_revision() {
    let dir = testkit::scratch("views", "round-trip");
    let root = project(&dir);
    let saved = save_view(&root, &definition(json!({})), 0).expect("save");
    let view_id = saved["view"]["id"].as_str().unwrap().to_string();
    assert_eq!(saved["config_revision"], 1);
    assert_eq!(saved["view"]["revision"], 1);

    assert_eq!(
        show_view(&root, "triage").expect("show")["view"]["id"],
        view_id
    );
    assert_eq!(
        list_views(&root).expect("list")["views"][0]["definition"],
        definition(json!({}))
    );

    let updated = save_view(
        &root,
        &definition(json!({"columns": ["check", "url"], "page_size": 10})),
        1,
    )
    .expect("update");
    assert_eq!(updated["view"]["id"], view_id);
    assert_eq!(updated["view"]["revision"], 2);
    assert_eq!(updated["config_revision"], 2);

    let before = std::fs::read(root.join(VIEWS_FILE)).expect("views file");
    let err =
        save_view(&root, &definition(json!({"name": "second"})), 1).expect_err("revision conflict");
    assert!(err.to_string().contains("revision conflict"), "got: {err}");
    assert_eq!(
        std::fs::read(root.join(VIEWS_FILE)).expect("reread"),
        before
    );
}

#[test]
fn delete_and_rename_are_revision_gated_and_keep_identity_rules() {
    let dir = testkit::scratch("views", "delete-rename");
    let root = project(&dir);
    save_view(&root, &definition(json!({})), 0).expect("save");
    save_view(&root, &definition(json!({"name": "other"})), 1).expect("save");

    let renamed = rename_view(&root, "triage", "inbox", 2).expect("rename");
    assert_eq!(renamed["config_revision"], 3);
    assert_eq!(renamed["view"]["name"], "inbox");
    assert_eq!(renamed["view"]["definition"]["name"], "inbox");
    assert_eq!(renamed["view"]["revision"], 2);
    let names: Vec<String> = list_views(&root).expect("list")["views"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["inbox", "other"]);

    let before = std::fs::read(root.join(VIEWS_FILE)).expect("views file");
    let err = delete_view(&root, "other", 2).expect_err("stale revision");
    assert!(err.to_string().contains("revision conflict"), "got: {err}");
    let err = rename_view(&root, "inbox", "other", 3).expect_err("existing name");
    assert!(err.to_string().contains("already exists"), "got: {err}");
    let err = rename_view(&root, "missing", "fresh", 3).expect_err("missing");
    assert!(err.to_string().contains("does not exist"), "got: {err}");
    let err = delete_view(&root, "missing", 3).expect_err("missing");
    assert!(err.to_string().contains("does not exist"), "got: {err}");
    assert_eq!(
        std::fs::read(root.join(VIEWS_FILE)).expect("reread"),
        before
    );

    let deleted = delete_view(&root, "other", 3).expect("delete");
    assert_eq!(deleted["ok"], true);
    assert!(!deleted["project_uuid"].is_null());
    assert_eq!(deleted["config_revision"], 4);
    assert_eq!(deleted["deleted"], "other");
    let names: Vec<String> = list_views(&root).expect("list")["views"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["inbox"]);
    let err = show_view(&root, "other").expect_err("gone");
    assert!(err.to_string().contains("does not exist"), "got: {err}");
}

#[test]
fn closed_view_schema_rejects_expressions_and_unsupported_values() {
    let dir = testkit::scratch("views", "schema-rejects");
    let root = project(&dir);
    let bad = [
        json!({"name": "x", "filters": {"expression": "1=1"}}),
        json!({"name": "x", "filters": {"url_regex": [".*"]}}),
        json!({"name": "x", "sort": {"field": "__import__", "direction": "asc"}}),
        json!({"name": "x", "sort": {"field": ["url"], "direction": "asc"}}),
        json!({"name": "x", "columns": ["severity", "sql"]}),
        json!({"name": "x", "filters": {"severity": []}}),
        json!({"name": "x", "page_size": 1001}),
    ];
    let path = root.join(VIEWS_FILE);
    for (i, view) in bad.iter().enumerate() {
        let before = if path.exists() {
            Some(std::fs::read(&path).expect("views file"))
        } else {
            None
        };
        save_view(&root, view, 0).expect_err(&format!("bad view {i}"));
        let after = if path.exists() {
            Some(std::fs::read(&path).expect("views file"))
        } else {
            None
        };
        assert_eq!(after, before, "view {i} mutated the store");
    }
}

#[test]
fn filter_sort_projection_and_pagination_are_repeatable() {
    let dir = testkit::scratch("views", "projection");
    let root = project(&dir);
    save_view(
        &root,
        &definition(json!({
            "filters": {
                "severity": ["warning"],
                "check": ["MISSING_TITLE"],
                "url": ["https://example.test/blog/b", "https://example.test/blog/c"],
                "segment": ["blog"],
            },
            "sort": {"field": "url", "direction": "desc"},
            "columns": ["check", "url", "segment", "text"],
            "page_size": 1,
        })),
        0,
    )
    .expect("save");

    let document = audit_document();
    let original = document.clone();
    let first = apply_view_to_audit(&root, "triage", &document, 0).expect("apply");
    let first_again = apply_view_to_audit(&root, "triage", &document, 0).expect("apply");
    let next_page = apply_view_to_audit(&root, "triage", &document, 1).expect("apply");

    assert_eq!(document, original);
    assert_eq!(first["state"], "partial");
    assert_eq!(
        first["counts"],
        json!({
            "source": 4,
            "matched": 2,
            "returned": 1,
            "filtered": 2,
            "missing_filter_fields": {"url": 1},
            "missing_projection_fields": {},
        })
    );
    assert_eq!(
        first["pagination"],
        json!({
            "offset": 0,
            "page_size": 1,
            "has_more": true,
            "truncated": true,
            "next_offset": 1,
        })
    );
    assert_eq!(first["items"], first_again["items"]);
    assert_eq!(
        first["items"][0]["fields"],
        json!({
            "check": "MISSING_TITLE",
            "url": "https://example.test/blog/c",
            "segment": "blog",
            "text": "Synthetic second missing title",
        })
    );
    assert_eq!(
        next_page["items"][0]["fields"]["url"],
        "https://example.test/blog/b"
    );
    assert_ne!(
        next_page["items"][0]["finding_id"],
        first["items"][0]["finding_id"]
    );
}

#[test]
fn severity_sort_directions_follow_severity_strength() {
    let dir = testkit::scratch("views", "severity-sort");
    let root = project(&dir);
    let mut document = audit_document();
    document["findings"].as_array_mut().unwrap().push(json!({
        "severity": "notice",
        "check": "OPTIONAL_FIXTURE",
        "url": "https://example.test/blog/notice",
        "text": "Synthetic notice",
    }));
    save_view(
        &root,
        &definition(json!({"columns": ["severity"], "page_size": 10})),
        0,
    )
    .expect("save");

    let severities = |doc: &Value| -> Vec<String> {
        apply_view_to_audit(&root, "triage", doc, 0).expect("apply")["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["fields"]["severity"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(
        severities(&document),
        ["critical", "warning", "warning", "warning", "notice"]
    );

    save_view(
        &root,
        &definition(json!({
            "columns": ["severity"],
            "page_size": 10,
            "sort": {"field": "severity", "direction": "asc"},
        })),
        1,
    )
    .expect("update");
    assert_eq!(
        severities(&document),
        ["notice", "warning", "warning", "warning", "critical"]
    );
}

#[test]
fn missing_filter_fields_are_counted_and_never_match() {
    let dir = testkit::scratch("views", "missing-fields");
    let root = project(&dir);
    save_view(
        &root,
        &definition(json!({
            "filters": {"url": ["https://example.test/blog/a"]},
            "columns": ["severity", "segment"],
        })),
        0,
    )
    .expect("save");
    let mut no_segments = audit_document();
    no_segments["run"] = json!({"scan_uuid": "scan-no-segments"});
    let result = apply_view_to_audit(&root, "triage", &no_segments, 0).expect("apply");
    assert_eq!(result["state"], "partial");
    assert_eq!(result["counts"]["matched"], 1);
    assert_eq!(result["counts"]["missing_filter_fields"], json!({"url": 1}));
    assert_eq!(
        result["counts"]["missing_projection_fields"],
        json!({"segment": 1})
    );
    assert_eq!(result["items"][0]["missing_fields"], json!(["segment"]));
}

#[test]
fn segment_filter_requires_and_uses_source_segment_definitions() {
    let dir = testkit::scratch("views", "segment-required");
    let root = project(&dir);
    save_view(
        &root,
        &definition(json!({
            "filters": {"segment": ["blog"]},
            "columns": ["url", "segment"],
        })),
        0,
    )
    .expect("save");
    let result = apply_view_to_audit(&root, "triage", &audit_document(), 0).expect("apply");
    assert_eq!(result["counts"]["matched"], 3);
    for item in result["items"].as_array().unwrap() {
        assert_eq!(item["fields"]["segment"], "blog");
    }

    let mut no_segments = audit_document();
    no_segments["run"] = json!({"scan_uuid": "scan-no-segments"});
    let err = apply_view_to_audit(&root, "triage", &no_segments, 0)
        .expect_err("segment filtering unavailable");
    assert!(
        err.to_string().contains("segment filtering is unavailable"),
        "got: {err}"
    );
}

#[test]
fn segment_view_uses_the_audits_declared_segment_rules() {
    let dir = testkit::scratch("views", "segment-pattern");
    let root = project(&dir);
    save_view(
        &root,
        &definition(json!({
            "filters": {"segment": ["blog"]},
            "columns": ["url", "segment"],
        })),
        0,
    )
    .expect("save");
    let mut document = audit_document();
    document["run"]["crawl_config"]["scope.segments"] =
        json!([{"name": "blog", "pattern": r"https://example\.test/blog/"}]);
    let result = apply_view_to_audit(&root, "triage", &document, 0).expect("apply");
    assert_eq!(result["counts"]["matched"], 3);
    for item in result["items"].as_array().unwrap() {
        assert_eq!(item["fields"]["segment"], "blog");
    }
}

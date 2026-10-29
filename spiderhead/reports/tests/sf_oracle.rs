//! Differential tests for the sf reporters and the tasks pipeline.
//!
//! the reference suite (run with the oracle venv) emits
//! `tests/fixtures/sf_oracle.json`; every assertion here rebuilds the same
//! `AuditResult` the reference generator built from the reference suite and
//! compares outputs — zero diffs.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use spiderhead_formats::models::{AuditResult, Group, Issue, Page, SkippedCheck};
use spiderhead_reports::sf_reporters;
use spiderhead_reports::sf_tasks;

fn fixtures() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sf_oracle.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture file"))
        .expect("fixture json")
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sf-oracle-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn text_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn object_list(value: Option<&Value>) -> Vec<Map<String, Value>> {
    value
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_object)
        .cloned()
        .collect()
}

/// `build_result` in the reference suite — the same dataclass, field by field.
fn build_result(spec: &Value) -> AuditResult {
    let issue = |row: &Value| Issue {
        check: row["check"].as_str().unwrap_or_default().to_owned(),
        severity: row["severity"].as_str().unwrap_or_default().to_owned(),
        source: row["source"].as_str().unwrap_or_default().to_owned(),
        message: row["message"].as_str().unwrap_or_default().to_owned(),
        id: row.get("id").and_then(Value::as_str).map(str::to_owned),
        fingerprint: row
            .get("fingerprint")
            .and_then(Value::as_str)
            .map(str::to_owned),
        target_url: row
            .get("target_url")
            .and_then(Value::as_str)
            .map(str::to_owned),
        status_code: row.get("status_code").and_then(Value::as_i64),
        occurrences_count: row
            .get("occurrences_count")
            .and_then(Value::as_i64)
            .unwrap_or(1),
        locations: object_list(row.get("locations")),
        details: row
            .get("details")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
        group_id: row
            .get("group_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        fix_hint: row
            .get("fix_hint")
            .and_then(Value::as_str)
            .map(str::to_owned),
        evidence: row
            .get("evidence")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
    };
    let page = |row: &Value| Page {
        url: row["url"].as_str().unwrap_or_default().to_owned(),
        status_code: row.get("status_code").and_then(Value::as_i64),
        status: row.get("status").and_then(Value::as_str).map(str::to_owned),
        content_type: row
            .get("content_type")
            .and_then(Value::as_str)
            .map(str::to_owned),
        indexability: row
            .get("indexability")
            .and_then(Value::as_str)
            .map(str::to_owned),
        indexability_status: row
            .get("indexability_status")
            .and_then(Value::as_str)
            .map(str::to_owned),
        metrics: row
            .get("metrics")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
        issues: text_list(row.get("issues")),
        issue_ids: text_list(row.get("issue_ids")),
        suppressed_issue_ids: text_list(row.get("suppressed_issue_ids")),
    };
    let group = |row: &Value| Group {
        group_id: row["group_id"].as_str().unwrap_or_default().to_owned(),
        check: row["check"].as_str().unwrap_or_default().to_owned(),
        value: row.get("value").and_then(Value::as_str).map(str::to_owned),
        urls: text_list(row.get("urls")),
        count: row.get("count").and_then(Value::as_i64).unwrap_or_default(),
    };
    let skipped = |row: &Value| SkippedCheck {
        id: row["id"].as_str().unwrap_or_default().to_owned(),
        reason: row["reason"].as_str().unwrap_or_default().to_owned(),
    };
    let rows = |key: &str| {
        spec.get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    AuditResult {
        run: spec
            .get("run")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
        summary: spec
            .get("summary")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
        issues: rows("issues").iter().map(issue).collect(),
        pages: rows("pages").iter().map(page).collect(),
        groups: rows("groups").iter().map(group).collect(),
        skipped: rows("skipped").iter().map(skipped).collect(),
        disabled: rows("disabled").iter().map(skipped).collect(),
        suppressed_issues: rows("suppressed_issues"),
    }
}

fn assert_json_eq(context: &str, expected: &Value, actual: &Value) {
    assert_eq!(
        expected,
        actual,
        "{context}\nEXPECTED: {}\nACTUAL: {}",
        serde_json::to_string_pretty(expected).unwrap_or_default(),
        serde_json::to_string_pretty(actual).unwrap_or_default()
    );
}

// md.write_markdown over every corpus document.
#[test]
fn reporters_parity() {
    let cases = fixtures()
        .get("results")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for (name, row) in &cases {
        let spec = row.get("spec").expect("spec");
        let result = build_result(spec);

        let actual_dict = Value::Object(sf_reporters::to_dict(&result));
        assert_json_eq(
            &format!("{name}: to_dict"),
            row.get("to_dict").unwrap_or(&Value::Null),
            &actual_dict,
        );

        let want_errors: Vec<String> = row
            .get("validate")
            .and_then(Value::as_array)
            .map(|list| text_list(Some(&Value::Array(list.clone()))))
            .unwrap_or_default();
        assert_eq!(
            want_errors,
            sf_reporters::validate(&result),
            "{name}: validate"
        );

        let dir = scratch(&format!("reporter-{name}"));
        let json_path = dir.join("audit.json");
        let returned = sf_reporters::write_json(&result, json_path.to_str().unwrap_or_default())
            .expect("write_json");
        assert!(
            returned.ends_with(
                row["write_json_path_suffix"]
                    .as_str()
                    .unwrap_or("audit.json")
            ),
            "{name}: write_json path"
        );
        let text = std::fs::read_to_string(&json_path).expect("audit.json text");
        assert_eq!(
            row["write_json_text"].as_str().unwrap_or_default(),
            text,
            "{name}: write_json text"
        );

        assert_eq!(
            row["md"].as_str().unwrap_or_default(),
            sf_reporters::render_markdown(&result),
            "{name}: markdown"
        );
        let md_path = dir.join("audit.md");
        let md_returned =
            sf_reporters::write_markdown(&result, md_path.to_str().unwrap_or_default())
                .expect("write_markdown");
        assert!(
            md_returned.ends_with(
                row["write_markdown_path_suffix"]
                    .as_str()
                    .unwrap_or("audit.md")
            ),
            "{name}: write_markdown path"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}

// filters, declared and segment-derived assignments, validation errors.
#[test]
fn tasks_parity() {
    let results = fixtures()
        .get("results")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let tasks = fixtures()
        .get("tasks")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let config = |pipeline: Value| json!({"tasks_pipeline": pipeline});
    let assignments = json!([
        {
            "name": "Shop template",
            "kind": "template",
            "rationale": "shared title block",
            "urls": ["https://example.com/a", "https://example.com/b"],
            "state": "confirmed",
            "confirmed_by": "review",
            "confirmation": "same template header",
        },
        {
            "name": "Legacy component",
            "kind": "component",
            "rationale": "inherited footer",
            "urls": ["https://example.com/gone"],
        },
    ]);
    let variants: Vec<(&str, Option<Value>)> = vec![
        ("default", None),
        ("issue", Some(config(json!({"group_by": "issue"})))),
        (
            "min_occurrences",
            Some(config(json!({"min_occurrences": 2}))),
        ),
        ("max_urls_1", Some(config(json!({"max_urls_per_task": 1})))),
        (
            "exclude_checks",
            Some(config(
                json!({"exclude_checks": ["TITLE_DUPLICATE", "BROKEN_INTERNAL_LINK"]}),
            )),
        ),
        (
            "include_checks",
            Some(config(json!({"include_checks": ["TITLE_DUPLICATE"]}))),
        ),
        (
            "severities",
            Some(config(json!({"include_severities": ["critical"]}))),
        ),
        (
            "check_assignment_declared",
            Some(config(
                json!({"group_by": "check_assignment", "assignments": assignments}),
            )),
        ),
        (
            "check_assignment_segments",
            Some(config(json!({"group_by": "check_assignment"}))),
        ),
        (
            "error_bad_group_by",
            Some(config(json!({"group_by": "bogus"}))),
        ),
        (
            "error_bad_assignments_type",
            Some(config(
                json!({"group_by": "check_assignment", "assignments": "nope"}),
            )),
        ),
        (
            "error_bad_assignment_item",
            Some(config(
                json!({"group_by": "check_assignment", "assignments": [{"name": "x"}]}),
            )),
        ),
        (
            "error_bad_assignment_kind",
            Some(config(json!({
                "group_by": "check_assignment",
                "assignments": [{"name": "n", "kind": "bogus", "rationale": "r",
                                 "urls": ["https://example.com/a"]}]
            }))),
        ),
        (
            "error_bad_assignment_state",
            Some(config(json!({
                "group_by": "check_assignment",
                "assignments": [{"name": "n", "kind": "template", "rationale": "r",
                                 "state": "bogus", "urls": ["https://example.com/a"]}]
            }))),
        ),
        (
            "error_confirmed_incomplete",
            Some(config(json!({
                "group_by": "check_assignment",
                "assignments": [{"name": "n", "kind": "template", "rationale": "r",
                                 "state": "confirmed", "urls": ["https://example.com/a"]}]
            }))),
        ),
        (
            "error_dup_urls",
            Some(config(json!({
                "group_by": "check_assignment",
                "assignments": [
                    {"name": "n", "kind": "template", "rationale": "r",
                     "urls": ["https://example.com/a"]},
                    {"name": "m", "kind": "component", "rationale": "r",
                     "urls": ["https://example.com/a"]}
                ]
            }))),
        ),
        (
            "error_empty_name",
            Some(config(json!({
                "group_by": "check_assignment",
                "assignments": [{"name": " ", "kind": "template", "rationale": "r",
                                 "urls": ["https://example.com/a"]}]
            }))),
        ),
        (
            "error_empty_rationale",
            Some(config(json!({
                "group_by": "check_assignment",
                "assignments": [{"name": "n", "kind": "template", "rationale": " ",
                                 "urls": ["https://example.com/a"]}]
            }))),
        ),
        (
            "error_empty_urls",
            Some(config(json!({
                "group_by": "check_assignment",
                "assignments": [{"name": "n", "kind": "template", "rationale": "r", "urls": []}]
            }))),
        ),
        (
            "error_non_object_item",
            Some(config(
                json!({"group_by": "check_assignment", "assignments": ["stray"]}),
            )),
        ),
    ];
    for (name, row) in &results {
        let audit = row.get("to_dict").expect("to_dict");
        let want_all = tasks.get(name).expect("tasks row");
        for (variant, cfg) in &variants {
            let want = want_all.get(variant).expect("variant");
            let ctx = format!("{name}:{variant}");
            match (want.get("backlog"), want.get("error")) {
                (Some(backlog), _) => {
                    let actual = sf_tasks::build_tasks(audit, cfg.as_ref())
                        .unwrap_or_else(|e| panic!("{ctx}: unexpected error {e}"));
                    assert_json_eq(&ctx, backlog, &actual);
                }
                (_, Some(error)) => {
                    let actual = sf_tasks::build_tasks(audit, cfg.as_ref())
                        .err()
                        .map(|e| e.to_string())
                        .unwrap_or_else(|| panic!("{ctx}: expected an error"));
                    assert_eq!(
                        error.as_str().unwrap_or_default(),
                        actual,
                        "{ctx}: error message"
                    );
                }
                _ => panic!("{ctx}: fixture has neither backlog nor error"),
            }
        }
        // tasks.md and the write_tasks files of the default backlog.
        if let Some(backlog) = want_all.get("default").and_then(|v| v.get("backlog")) {
            assert_eq!(
                want_all["tasks_md"].as_str().unwrap_or_default(),
                sf_tasks::render_tasks_md(backlog),
                "{name}: tasks_md"
            );
            let dir = scratch(&format!("tasks-{name}"));
            let json_target = dir.join("deep").join("tasks.json");
            let md_target = dir.join("deep").join("tasks.md");
            let (jp, mp) = sf_tasks::write_tasks(
                backlog,
                json_target.to_str().unwrap_or_default(),
                md_target.to_str().unwrap_or_default(),
            )
            .expect("write_tasks");
            let suffixes = want_all["write_tasks"]["paths_suffix"]
                .as_array()
                .map(|a| text_list(Some(&Value::Array(a.clone()))))
                .unwrap_or_default();
            assert!(jp.ends_with(suffixes.first().map(String::as_str).unwrap_or("tasks.json")));
            assert!(mp.ends_with(suffixes.get(1).map(String::as_str).unwrap_or("tasks.md")));
            assert_eq!(
                want_all["write_tasks"]["json_text"]
                    .as_str()
                    .unwrap_or_default(),
                std::fs::read_to_string(&json_target).unwrap_or_default(),
                "{name}: write_tasks json"
            );
            assert_eq!(
                want_all["write_tasks"]["md_text"]
                    .as_str()
                    .unwrap_or_default(),
                std::fs::read_to_string(&md_target).unwrap_or_default(),
                "{name}: write_tasks md"
            );
            std::fs::remove_dir_all(&dir).ok();
        }
    }
}

// companion file the reference writer produced — the Rust reader must agree.
#[test]
fn tasks_from_audit_v2_parity() {
    let fixture = fixtures();
    let streamed = fixture.get("streamed").expect("streamed");
    let dir = scratch("streamed");
    let scan = dir.join("scan.sqlite");
    let raw =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../oracle/raw-scans/basic/scan.sqlite");
    std::fs::copy(&raw, &scan).expect("copy raw scan");
    let companion = PathBuf::from(format!("{}.audit-v2.sqlite", scan.display()));
    let bytes = {
        // Base64-decode the reference-written companion without a new dependency.
        let b64 = streamed["companion_b64"].as_str().expect("companion_b64");
        let mut out = Vec::new();
        let mut acc: u32 = 0;
        let mut bits = 0u32;
        for ch in b64.bytes() {
            let v = match ch {
                b'A'..=b'Z' => ch - b'A',
                b'a'..=b'z' => ch - b'a' + 26,
                b'0'..=b'9' => ch - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => continue,
            };
            acc = (acc << 6) | u32::from(v);
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
                acc &= (1 << bits) - 1;
            }
        }
        out
    };
    std::fs::write(&companion, &bytes).expect("write companion");

    let cases: Vec<(&str, Option<Value>)> = vec![
        ("check", None),
        (
            "check_assignment",
            Some(json!({
                "tasks_pipeline": {
                    "group_by": "check_assignment",
                    "assignments": [{
                        "name": "Shop template",
                        "kind": "template",
                        "rationale": "shared title block",
                        "urls": ["https://example.com/a", "https://example.com/b"],
                    }],
                }
            })),
        ),
        (
            "bad_group_by",
            Some(json!({"tasks_pipeline": {"group_by": "issue"}})),
        ),
        (
            "check_assignment_no_declared",
            Some(json!({"tasks_pipeline": {"group_by": "check_assignment"}})),
        ),
    ];
    for (name, config) in &cases {
        let want = streamed.get(name).expect("streamed case");
        let actual = sf_tasks::build_tasks_from_audit_v2(&scan, config.as_ref());
        match (want.get("backlog"), want.get("error")) {
            (Some(backlog), _) => {
                let actual = actual.unwrap_or_else(|e| panic!("streamed:{name}: error {e}"));
                assert_json_eq(&format!("streamed:{name}"), backlog, &actual);
            }
            (_, Some(error)) => {
                let actual = actual
                    .err()
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| panic!("streamed:{name}: expected an error"));
                assert_eq!(
                    error.as_str().unwrap_or_default(),
                    actual,
                    "streamed:{name}: error message"
                );
            }
            _ => panic!("streamed:{name}: fixture has neither backlog nor error"),
        }
    }
    std::fs::remove_dir_all(&dir).ok();
}

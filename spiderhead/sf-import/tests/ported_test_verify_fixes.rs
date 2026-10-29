//! Ports of the reference suite cases that exercise only
//! `spiderhead.core.verification`. Handler, CLI and live-recrawl tests stay
//! deferred: they need the `mcp` handler, the `cli` surface, the `crawl`
//! collector, or `projects`/`store` remediation ledger.

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use spiderhead_core::common::python_json;
use spiderhead_sf_import::verification::{
    classify, offline_observation_gap, results_policy_fingerprint, select, source_identity,
    AuditSource,
};

const A: &str = "https://example.test/a";
const B: &str = "https://example.test/b";
const C: &str = "https://example.test/c";

fn issue(id: &str, check: &str, url: &str, extra: Map<String, Value>) -> Map<String, Value> {
    let mut row = Map::new();
    row.insert("id".into(), json!(id));
    row.insert("check".into(), json!(check));
    row.insert("target_url".into(), json!(url));
    row.insert("status_code".into(), json!(200));
    row.extend(extra);
    row
}

fn audit(
    urls: &[&str],
    issues: Vec<Map<String, Value>>,
    representation: &str,
    scan_uuid: Option<&str>,
    generated_at: Option<&str>,
    run_extra: Map<String, Value>,
) -> Map<String, Value> {
    let crawl_config = json!({"rendering": false, "limits.max_urls": 100, "depth": 3});
    let mut run = json!({
        "generated_at": generated_at.map_or(Value::Null, Value::from),
        "source": "https://example.test/",
        "crawl_config": crawl_config,
        "checks_skipped": [],
        "checks_disabled": [],
        "crawl_partial": false,
    });
    run.as_object_mut().unwrap().extend(run_extra);
    let mut summary = json!({
        "check_coverage": {"checks_silent_ids": ["TITLE_MISSING", "DESC_MISSING", "CANONICAL_MISSING"]},
    });
    if let Some(uuid) = scan_uuid {
        summary
            .as_object_mut()
            .unwrap()
            .insert("evidence_contract".into(), json!({"scan_uuid": uuid}));
    }
    let pages: Vec<Value> = urls
        .iter()
        .map(|u| {
            json!({"url": u, "status_code": 200, "content_type": "text/html",
                   "metrics": {"representation": representation}})
        })
        .collect();
    let mut doc = Map::new();
    doc.insert("schema_version".into(), json!("2.0"));
    doc.insert("run".into(), run);
    doc.insert("summary".into(), summary);
    doc.insert("pages".into(), Value::Array(pages));
    doc.insert(
        "issues".into(),
        Value::Array(issues.into_iter().map(Value::Object).collect()),
    );
    doc
}

#[test]
fn selection_accepts_ids_saved_view_and_supplied_urls() {
    let baseline = audit(
        &[A, B, C],
        vec![
            issue("ISSUE-000001", "TITLE_MISSING", A, Map::new()),
            issue("ISSUE-000002", "DESC_MISSING", B, Map::new()),
            issue("ISSUE-000003", "CANONICAL_MISSING", C, Map::new()),
        ],
        "static",
        None,
        Some("2026-10-01T00:00:00Z"),
        Map::new(),
    );
    let view = json!({"schema_version": "verification_view.v1", "checks": ["CANONICAL_MISSING"]});
    let (chosen, urls) = select(
        &baseline,
        Some(&json!(["ISSUE-000001"])),
        Some(&json!([B])),
        Some(&view),
    )
    .unwrap();
    let ids: Vec<&str> = chosen.iter().map(|i| i["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["ISSUE-000001", "ISSUE-000002", "ISSUE-000003"]);
    assert_eq!(urls, [A, B, C]);
    assert!(
        select(&baseline, Some(&json!(["ISSUE-999999"]),), None, None)
            .unwrap_err()
            .0
            .contains("absent from baseline")
    );
    assert!(
        select(&baseline, None, Some(&json!(["https://other.test/"])), None)
            .unwrap_err()
            .0
            .contains("URLs are absent")
    );
}

#[test]
fn source_identity_includes_results_affecting_policy_fingerprint() {
    let mut baseline = audit(
        &[A, B, C],
        vec![],
        "static",
        None,
        Some("2026-10-01T00:00:00Z"),
        Map::new(),
    );
    let first = source_identity(&AuditSource::Document(&baseline)).unwrap();
    let expected = {
        let raw = python_json(&baseline["run"]["crawl_config"]);
        hex(&Sha256::digest(raw.as_bytes()))[..16].to_owned()
    };
    assert_eq!(first["results_policy_fingerprint"], json!(expected));
    baseline["run"]["crawl_config"]["robots.policy"] = json!("ignore");
    let second = source_identity(&AuditSource::Document(&baseline)).unwrap();
    assert_ne!(
        second["results_policy_fingerprint"],
        first["results_policy_fingerprint"]
    );
    let empty = json!({"run": {}});
    assert!(results_policy_fingerprint(empty.as_object().unwrap()).is_none());
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn four_outcomes_use_exact_page_and_finding_evidence() {
    let baseline = audit(
        &[A, B, C],
        vec![
            issue("ISSUE-000001", "TITLE_MISSING", A, Map::new()),
            issue(
                "ISSUE-000002",
                "DESC_MISSING",
                B,
                json!({"details": {"value": "old"}})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
            issue("ISSUE-000003", "CANONICAL_MISSING", C, Map::new()),
        ],
        "static",
        None,
        Some("2026-10-01T00:00:00Z"),
        Map::new(),
    );
    let after = audit(
        &[A, B, C],
        vec![
            issue(
                "ISSUE-000005",
                "DESC_MISSING",
                B,
                json!({"details": {"value": "old"}})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
            issue(
                "ISSUE-000006",
                "CANONICAL_MISSING",
                C,
                json!({"details": {"value": "new"}})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        ],
        "static",
        None,
        Some("2026-10-01T00:00:00Z"),
        Map::new(),
    );
    let selected: Vec<Map<String, Value>> = baseline["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i.as_object().unwrap().clone())
        .collect();
    let mut observations = Map::new();
    for u in [A, B, C] {
        observations.insert(u.to_owned(), Value::Object(after.clone()));
    }
    let items = classify(
        &baseline,
        &selected,
        &observations,
        "selected URL was not recrawled",
    )
    .unwrap();
    let statuses: Vec<&str> = items
        .iter()
        .map(|i| i["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["resolved", "persisting", "changed"]);
    assert_eq!(items[0]["before"]["target_url"], json!(A));
    assert_eq!(items[0]["after_page"]["url"], json!(A));
    assert_eq!(items[2]["after"]["details"], json!({"value": "new"}));
}

#[test]
fn missing_page_graph_check_changed_config_and_js_gap_are_not_fixes() {
    let graph = issue("ISSUE-000004", "LOW_LINK_SCORE", B, Map::new());
    let baseline = audit(
        &[A, B, C],
        vec![
            issue("ISSUE-000001", "TITLE_MISSING", A, Map::new()),
            graph.clone(),
        ],
        "static",
        None,
        Some("2026-10-01T00:00:00Z"),
        Map::new(),
    );
    let partial = audit(
        &[B],
        vec![],
        "static",
        None,
        Some("2026-10-01T00:00:00Z"),
        json!({"crawl_partial": true}).as_object().unwrap().clone(),
    );
    let selected: Vec<Map<String, Value>> = baseline["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i.as_object().unwrap().clone())
        .collect();
    let observations = json!({A: partial, B: partial}).as_object().unwrap().clone();
    let statuses = classify(&baseline, &selected, &observations, "missing").unwrap();
    assert_eq!(statuses[0]["status"], json!("not_verifiable"));
    assert_eq!(statuses[1]["status"], json!("not_verifiable"));
    assert!(statuses[0]["reason"]
        .as_str()
        .unwrap()
        .contains("no page observation"));
    assert!(statuses[1]["reason"]
        .as_str()
        .unwrap()
        .contains("site-wide"));

    let mut changed_config = audit(
        &[A],
        vec![],
        "static",
        None,
        Some("2026-10-01T00:00:00Z"),
        Map::new(),
    );
    changed_config["run"]["crawl_config"]["robots.policy"] = json!("ignore");
    let observations = json!({A: changed_config}).as_object().unwrap().clone();
    let result = &classify(&baseline, &selected[..1], &observations, "missing").unwrap()[0];
    assert_eq!(result["status"], json!("not_verifiable"));
    assert!(result["reason"].as_str().unwrap().contains("robots.policy"));

    let rendered = audit(
        &[A],
        vec![issue("ISSUE-000001", "TITLE_MISSING", A, Map::new())],
        "rendered",
        None,
        Some("2026-10-01T00:00:00Z"),
        Map::new(),
    );
    let rendered_selected: Vec<Map<String, Value>> = rendered["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i.as_object().unwrap().clone())
        .collect();
    let plain = audit(
        &[A],
        vec![],
        "static",
        None,
        Some("2026-10-01T00:00:00Z"),
        Map::new(),
    );
    let observations = json!({A: plain}).as_object().unwrap().clone();
    let result = &classify(&rendered, &rendered_selected, &observations, "missing").unwrap()[0];
    assert_eq!(result["status"], json!("not_verifiable"));
    assert!(result["reason"]
        .as_str()
        .unwrap()
        .contains("representation"));
}

#[test]
fn skipped_check_is_unverifiable_but_a_measured_page_in_partial_run_can_resolve() {
    let baseline = audit(
        &[A],
        vec![issue("ISSUE-000001", "TITLE_MISSING", A, Map::new())],
        "static",
        None,
        Some("2026-10-01T00:00:00Z"),
        Map::new(),
    );
    let mut after = audit(
        &[A],
        vec![],
        "static",
        None,
        Some("2026-10-01T00:00:00Z"),
        json!({"crawl_partial": true}).as_object().unwrap().clone(),
    );
    after["run"]["checks_skipped"] =
        json!([{"id": "TITLE_MISSING", "reason": "HTML was not parsed"}]);
    let selected: Vec<Map<String, Value>> = baseline["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i.as_object().unwrap().clone())
        .collect();
    let observations = json!({A: after.clone()}).as_object().unwrap().clone();
    let row = &classify(&baseline, &selected, &observations, "missing").unwrap()[0];
    assert_eq!(row["status"], json!("not_verifiable"));
    assert!(row["reason"]
        .as_str()
        .unwrap()
        .contains("HTML was not parsed"));
    after["run"]["checks_skipped"] = json!([]);
    let observations = json!({A: after}).as_object().unwrap().clone();
    let row = &classify(&baseline, &selected, &observations, "missing").unwrap()[0];
    assert_eq!(row["status"], json!("resolved"));
}

#[test]
fn redirected_html_page_is_changed_not_resolved() {
    let baseline = audit(
        &[A],
        vec![issue("ISSUE-000001", "TITLE_MISSING", A, Map::new())],
        "static",
        None,
        Some("2026-10-01T00:00:00Z"),
        Map::new(),
    );
    let mut after = audit(
        &[A],
        vec![],
        "static",
        None,
        Some("2026-10-01T00:00:00Z"),
        Map::new(),
    );
    after["pages"][0]["status_code"] = json!(301);
    let selected: Vec<Map<String, Value>> = baseline["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i.as_object().unwrap().clone())
        .collect();
    let observations = json!({A: after}).as_object().unwrap().clone();
    let item = &classify(&baseline, &selected, &observations, "missing").unwrap()[0];
    assert_eq!(item["status"], json!("changed"));
    assert!(item["reason"].as_str().unwrap().contains("successful HTML"));
}

#[test]
fn missing_response_and_wrong_source_are_not_fixes() {
    let baseline = audit(
        &[A],
        vec![issue("ISSUE-000001", "TITLE_MISSING", A, Map::new())],
        "static",
        None,
        Some("2026-10-01T00:00:00Z"),
        Map::new(),
    );
    let mut after = audit(
        &[A],
        vec![],
        "static",
        None,
        Some("2026-10-01T00:00:00Z"),
        Map::new(),
    );
    after["pages"][0]["status_code"] = Value::Null;
    let selected: Vec<Map<String, Value>> = baseline["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i.as_object().unwrap().clone())
        .collect();
    let observations = json!({A: after.clone()}).as_object().unwrap().clone();
    let item = &classify(&baseline, &selected, &observations, "missing").unwrap()[0];
    assert_eq!(item["status"], json!("not_verifiable"));
    assert!(item["reason"]
        .as_str()
        .unwrap()
        .contains("no measured HTTP"));

    after["pages"][0]["status_code"] = json!(200);
    after["run"]["source"] = json!("https://other.test/");
    let observations = json!({A: after.clone()}).as_object().unwrap().clone();
    let item = &classify(&baseline, &selected, &observations, "missing").unwrap()[0];
    assert_eq!(item["status"], json!("not_verifiable"));
    assert!(item["reason"].as_str().unwrap().contains("source origin"));

    after.remove("issues");
    let observations = json!({A: after}).as_object().unwrap().clone();
    let item = &classify(&baseline, &selected, &observations, "missing").unwrap()[0];
    assert_eq!(item["status"], json!("not_verifiable"));
    assert!(item["reason"].as_str().unwrap().contains("complete audit"));
}

#[test]
fn status_check_needs_a_clean_measured_response() {
    let baseline = audit(
        &[A],
        vec![issue("ISSUE-000001", "BROKEN_PAGE_4XX", A, Map::new())],
        "static",
        None,
        Some("2026-10-01T00:00:00Z"),
        Map::new(),
    );
    let mut after = audit(
        &[A],
        vec![],
        "static",
        None,
        Some("2026-10-01T00:00:00Z"),
        Map::new(),
    );
    after["summary"]["check_coverage"]["checks_silent_ids"]
        .as_array_mut()
        .unwrap()
        .push(json!("BROKEN_PAGE_4XX"));
    after["pages"][0]["status_code"] = json!(500);
    let selected: Vec<Map<String, Value>> = baseline["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i.as_object().unwrap().clone())
        .collect();
    let observations = json!({A: after.clone()}).as_object().unwrap().clone();
    let item = &classify(&baseline, &selected, &observations, "missing").unwrap()[0];
    assert_eq!(item["status"], json!("changed"));
    after["pages"][0]["status_code"] = json!(200);
    let observations = json!({A: after}).as_object().unwrap().clone();
    let item = &classify(&baseline, &selected, &observations, "missing").unwrap()[0];
    assert_eq!(item["status"], json!("resolved"));
}

// (the parametrize rows; the handler-level artifact asserts stay deferred to mcp)
#[test]
fn offline_after_requires_distinct_later_observation() {
    type GapRow = (
        Option<&'static str>,
        Option<&'static str>,
        Option<&'static str>,
        Option<&'static str>,
        &'static str,
    );
    let cases: [GapRow; 8] = [
        (
            Some("same-scan"),
            Some("same-scan"),
            Some("2026-10-01T00:00:00Z"),
            Some("2026-10-02T00:00:00Z"),
            "same scan UUID",
        ),
        (
            Some("before-scan"),
            Some("after-scan"),
            Some("2026-10-02T00:00:00Z"),
            Some("2026-10-01T00:00:00Z"),
            "not later",
        ),
        (
            Some("before-scan"),
            Some("after-scan"),
            Some("2026-10-01T00:00:00Z"),
            Some("2026-10-01T00:00:00Z"),
            "not later",
        ),
        (
            None,
            Some("after-scan"),
            Some("2026-10-01T00:00:00Z"),
            Some("2026-10-02T00:00:00Z"),
            "lacks a recorded scan UUID",
        ),
        (
            Some("before-scan"),
            None,
            Some("2026-10-01T00:00:00Z"),
            Some("2026-10-02T00:00:00Z"),
            "lacks a recorded scan UUID",
        ),
        (
            Some("before-scan"),
            Some("after-scan"),
            None,
            Some("2026-10-02T00:00:00Z"),
            "lacks a timezone-aware",
        ),
        (
            Some("before-scan"),
            Some("after-scan"),
            Some("2026-10-01T00:00:00Z"),
            None,
            "lacks a timezone-aware",
        ),
        (
            Some("before-scan"),
            Some("after-scan"),
            Some("2026-10-01T00:00:00"),
            Some("2026-10-02T00:00:00Z"),
            "lacks a timezone-aware",
        ),
    ];
    for (before_id, after_id, before_time, after_time, reason) in cases {
        let before = audit(
            &[A],
            vec![issue("ISSUE-000001", "TITLE_MISSING", A, Map::new())],
            "static",
            before_id,
            before_time,
            Map::new(),
        );
        let after = audit(&[A], vec![], "static", after_id, after_time, Map::new());
        let gap = offline_observation_gap(&before, &after).unwrap_or("");
        assert!(
            gap.contains(reason),
            "{before_id:?}/{after_id:?} {before_time:?}/{after_time:?}: {gap}"
        );
    }
}

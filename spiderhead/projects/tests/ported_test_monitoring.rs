//! `match=` becomes an error-message substring assertion; the `_now`
//! monkeypatch becomes the injected `run_at` clock.

#[path = "common/testkit.rs"]
mod testkit;

use serde_json::json;
use spiderhead_projects::monitoring::{configure, run, run_at, schedule, status};
use spiderhead_projects::workspace::create_project;

fn project(dir: &testkit::Scratch) -> std::path::PathBuf {
    let root = dir.0.join("project");
    create_project(&root, "https://example.test/", None, None, None, None).expect("create");
    root
}

#[test]
fn disabled_monitor_keeps_quiet_and_actionable_runs_distinct() {
    let dir = testkit::scratch("monitoring", "quiet-actionable");
    let root = project(&dir);
    let configured = configure(
        &root,
        &json!({
            "enabled": false,
            "urls": ["https://example.test/a"],
            "max_urls": 1,
            "max_requests": 2,
            "full_refresh_every": 7,
        }),
        0,
    )
    .expect("configure");
    let quiet = run(
        &root,
        "scan:one",
        &json!([{"url": "https://example.test/a", "changes": [{"severity": "notice"}]}]),
        configured["revision"].as_i64().unwrap(),
    )
    .expect("quiet run");
    assert_eq!(quiet["run"]["state"], "quiet");
    assert_eq!(quiet["notification"], "none");
    let active = run(
        &root,
        "scan:two",
        &json!([{"url": "https://example.test/a", "changes": [{"severity": "warning"}]}]),
        quiet["revision"].as_i64().unwrap(),
    )
    .expect("active run");
    assert_eq!(active["run"]["state"], "actionable");
    assert_eq!(
        status(&root).expect("status")["last_run"]["scan_id"],
        "scan:two"
    );
}

#[test]
fn monitor_claims_are_disabled_by_default_and_recover_without_a_timer() {
    let dir = testkit::scratch("monitoring", "claims");
    let root = project(&dir);
    let mut policy = json!({
        "enabled": false,
        "urls": ["https://example.test/a", "https://example.test/b"],
        "max_urls": 1,
        "max_requests": 2,
        "max_render_requests": 1,
        "full_refresh_every": 2,
        "interval_seconds": 60,
        "suppression_runs": 2,
        "severity_threshold": "warning",
    });
    let configured = configure(&root, &policy, 0).expect("configure");
    assert_eq!(configured["due"]["state"], "disabled");
    let err = schedule(
        &root,
        "start",
        configured["revision"].as_i64().unwrap(),
        None,
    )
    .expect_err("disabled claim");
    assert!(err.to_string().contains("disabled"), "got: {err}");

    policy["enabled"] = json!(true);
    let configured =
        configure(&root, &policy, configured["revision"].as_i64().unwrap()).expect("enable");
    let claimed = schedule(
        &root,
        "start",
        configured["revision"].as_i64().unwrap(),
        None,
    )
    .expect("claim");
    assert_eq!(claimed["scheduled"], false);
    assert_eq!(claimed["runner"]["state"], "running");
    let err = schedule(&root, "start", claimed["revision"].as_i64().unwrap(), None)
        .expect_err("overlapping claim");
    assert!(err.to_string().contains("overlapping"), "got: {err}");
    let recovered = schedule(
        &root,
        "recover",
        claimed["revision"].as_i64().unwrap(),
        None,
    )
    .expect("recover");
    assert_eq!(recovered["runner"]["state"], "interrupted");
}

#[test]
fn full_refresh_keeps_unavailable_evidence_without_inventing_resolutions() {
    let dir = testkit::scratch("monitoring", "full-refresh");
    let root = project(&dir);
    let configured = configure(
        &root,
        &json!({
            "enabled": true,
            "urls": ["https://example.test/a", "https://example.test/b"],
            "max_urls": 1,
            "max_requests": 2,
            "full_refresh_every": 2,
        }),
        0,
    )
    .expect("configure");
    let retained = run(
        &root,
        "scan:full",
        &json!([
            {"url": "https://example.test/a", "changes": [], "qualifier": "fresh"},
            {"url": "https://example.test/b", "changes": [], "qualifier": "unavailable"},
        ]),
        configured["revision"].as_i64().unwrap(),
    )
    .expect("retained run");
    assert_eq!(retained["run"]["state"], "partial");
    assert_eq!(retained["run"]["recoveries"], json!([]));
    assert_eq!(retained["run"]["baseline"]["scan_id"], "scan:full");
    assert_eq!(retained["due"]["mode"], "full");
}

#[test]
fn claimed_full_refresh_keeps_its_plan_and_enforces_request_budgets() {
    let dir = testkit::scratch("monitoring", "claimed-full");
    let root = project(&dir);
    let configured = configure(
        &root,
        &json!({
            "enabled": true,
            "urls": ["https://example.test/a", "https://example.test/b"],
            "max_urls": 1,
            "max_requests": 2,
            "max_render_requests": 1,
            "full_refresh_every": 2,
        }),
        0,
    )
    .expect("configure");
    let claimed = schedule(
        &root,
        "start",
        configured["revision"].as_i64().unwrap(),
        None,
    )
    .expect("claim");
    let retained = run(
        &root,
        "scan:full-revalidated",
        &json!([
            {
                "url": "https://example.test/a",
                "changes": [],
                "qualifier": "revalidated",
                "cache_state": "revalidated",
            },
            {
                "url": "https://example.test/b",
                "changes": [],
                "qualifier": "stale",
                "cache_state": "cached",
            },
        ]),
        claimed["revision"].as_i64().unwrap(),
    )
    .expect("retained run");
    assert_eq!(retained["run"]["mode"], "full");
    assert_eq!(
        retained["run"]["observations"][0]["cache_state"],
        "revalidated"
    );
    assert_eq!(status(&root).expect("status")["runner"]["state"], "idle");
}

#[test]
fn alert_suppression_window_expires_after_the_declared_number_of_runs() {
    let dir = testkit::scratch("monitoring", "suppression");
    let root = project(&dir);
    let configured = configure(
        &root,
        &json!({
            "enabled": false,
            "urls": ["https://example.test/a"],
            "max_urls": 1,
            "max_requests": 1,
            "full_refresh_every": 7,
            "suppression_runs": 1,
        }),
        0,
    )
    .expect("configure");
    let change = json!({"kind": "canonical_changed", "severity": "warning"});
    let first = run(
        &root,
        "scan:one",
        &json!([{"url": "https://example.test/a", "changes": [change.clone()]}]),
        configured["revision"].as_i64().unwrap(),
    )
    .expect("first");
    let suppressed = run(
        &root,
        "scan:two",
        &json!([{"url": "https://example.test/a", "changes": [change.clone()]}]),
        first["revision"].as_i64().unwrap(),
    )
    .expect("suppressed");
    let quiet = run(
        &root,
        "scan:three",
        &json!([{"url": "https://example.test/a", "changes": []}]),
        suppressed["revision"].as_i64().unwrap(),
    )
    .expect("quiet");
    let repeated = run(
        &root,
        "scan:four",
        &json!([{"url": "https://example.test/a", "changes": [change]}]),
        quiet["revision"].as_i64().unwrap(),
    )
    .expect("repeated");
    assert!(!first["run"]["alerts"].as_array().unwrap().is_empty());
    assert_eq!(suppressed["run"]["alerts"], json!([]));
    assert!(!repeated["run"]["alerts"].as_array().unwrap().is_empty());
}

#[test]
fn per_url_snapshots_compare_all_seo_monitoring_fields_with_provenance() {
    let dir = testkit::scratch("monitoring", "snapshots");
    let root = project(&dir);
    let configured = configure(
        &root,
        &json!({
            "enabled": false,
            "urls": ["https://example.test/a"],
            "max_urls": 1,
            "max_requests": 1,
            "full_refresh_every": 7,
        }),
        0,
    )
    .expect("configure");
    let baseline = json!({
        "status": 200,
        "indexability": "indexable",
        "canonical": "https://example.test/a",
        "robots": "index,follow",
        "metadata": {"title": "Before"},
        "content": "before-sha256",
        "links": {"internal": 2},
    });
    let first = run(
        &root,
        "scan:baseline",
        &json!([{"url": "https://example.test/a", "changes": [], "measurement": baseline}]),
        configured["revision"].as_i64().unwrap(),
    )
    .expect("baseline");
    let changed = run(
        &root,
        "scan:after",
        &json!([{
            "url": "https://example.test/a",
            "changes": [],
            "qualifier": "revalidated",
            "measurement": {
                "status": 301,
                "indexability": "noindex",
                "canonical": "https://example.test/b",
                "robots": "noindex,nofollow",
                "metadata": {"title": "After"},
                "content": "after-sha256",
                "links": {"internal": 1},
            },
        }]),
        first["revision"].as_i64().unwrap(),
    )
    .expect("changed");
    let changes = changed["run"]["observations"][0]["changes"]
        .as_array()
        .expect("changes");
    let fields: std::collections::HashSet<&str> = changes
        .iter()
        .map(|c| c["field"].as_str().unwrap())
        .collect();
    assert_eq!(
        fields,
        [
            "status",
            "indexability",
            "canonical",
            "robots",
            "metadata",
            "content",
            "links"
        ]
        .into_iter()
        .collect()
    );
    let baselines: std::collections::HashSet<&str> = changes
        .iter()
        .map(|c| c["baseline_scan_id"].as_str().unwrap())
        .collect();
    assert_eq!(baselines, ["scan:baseline"].into_iter().collect());
    assert!(changed["run"]["observations"][0]["measured_at"]
        .as_str()
        .unwrap()
        .ends_with('Z'));
}

#[test]
fn cancelled_claim_rejects_observations_until_an_explicit_restart() {
    let dir = testkit::scratch("monitoring", "cancelled");
    let root = project(&dir);
    let configured = configure(
        &root,
        &json!({
            "enabled": true,
            "urls": ["https://example.test/a"],
            "max_urls": 1,
            "max_requests": 1,
            "full_refresh_every": 2,
        }),
        0,
    )
    .expect("configure");
    let claimed = schedule(
        &root,
        "start",
        configured["revision"].as_i64().unwrap(),
        None,
    )
    .expect("claim");
    let cancelled =
        schedule(&root, "cancel", claimed["revision"].as_i64().unwrap(), None).expect("cancel");
    let err = run(
        &root,
        "cancelled-pass",
        &json!([{"url": "https://example.test/a", "changes": []}]),
        cancelled["revision"].as_i64().unwrap(),
    )
    .expect_err("cancelled claim");
    assert!(err.to_string().contains("restart"), "got: {err}");
    let restarted = schedule(
        &root,
        "start",
        cancelled["revision"].as_i64().unwrap(),
        None,
    )
    .expect("restart");
    let retained = run(
        &root,
        "restarted-pass",
        &json!([{"url": "https://example.test/a", "changes": []}]),
        restarted["revision"].as_i64().unwrap(),
    )
    .expect("retained");
    assert_eq!(retained["run"]["coverage"]["complete"], true);
}

#[test]
fn incremental_cursor_retries_missing_urls_then_advances_and_records_next_due() {
    let dir = testkit::scratch("monitoring", "cursor");
    let root = project(&dir);
    let now = || "2026-10-04T00:00:00Z".to_string();
    let configured = configure(
        &root,
        &json!({
            "enabled": true,
            "urls": [
                "https://example.test/a",
                "https://example.test/b",
                "https://example.test/c",
            ],
            "max_urls": 2,
            "max_requests": 3,
            "full_refresh_every": 3,
            "interval_seconds": 60,
        }),
        0,
    )
    .expect("configure");
    let urls = configured["policy"]["urls"].as_array().unwrap().clone();
    let full = run_at(
        &root,
        "full",
        &json!(urls
            .iter()
            .map(|url| json!({"url": url, "changes": []}))
            .collect::<Vec<_>>()),
        configured["revision"].as_i64().unwrap(),
        Some(&now),
    )
    .expect("full");
    assert_eq!(
        full["due"]["planned_urls"],
        json!(["https://example.test/a", "https://example.test/b"])
    );
    let incomplete = run_at(
        &root,
        "incomplete",
        &json!([{"url": "https://example.test/a", "changes": []}]),
        full["revision"].as_i64().unwrap(),
        Some(&now),
    )
    .expect("incomplete");
    assert_eq!(
        incomplete["run"]["coverage"],
        json!({
            "planned_url_count": 2,
            "observed_url_count": 1,
            "unobserved_url_count": 1,
            "unobserved_urls": ["https://example.test/b"],
            "fresh_count": 1,
            "revalidated_count": 0,
            "stale_count": 0,
            "unavailable_count": 0,
            "partial_count": 0,
            "failed_count": 0,
            "failures": [],
            "complete": false,
        })
    );
    assert_eq!(incomplete["run"]["state"], "partial");
    assert_eq!(
        incomplete["due"]["planned_urls"],
        json!(["https://example.test/a", "https://example.test/b"])
    );
    let complete = run_at(
        &root,
        "incremental",
        &json!([
            {"url": "https://example.test/a", "changes": []},
            {"url": "https://example.test/b", "changes": []},
        ]),
        incomplete["revision"].as_i64().unwrap(),
        Some(&now),
    )
    .expect("complete");
    assert_eq!(
        complete["due"]["planned_urls"],
        json!(["https://example.test/c", "https://example.test/a"])
    );
    assert_eq!(
        status(&root).expect("status")["runner"]["next_due_at"],
        "2026-10-04T00:01:00Z"
    );
}

#[test]
fn cached_data_cannot_be_declared_fresh_and_failed_coverage_stays_explicit() {
    let dir = testkit::scratch("monitoring", "cache-failed");
    let root = project(&dir);
    let configured = configure(
        &root,
        &json!({
            "enabled": false,
            "urls": ["https://example.test/a"],
            "max_urls": 1,
            "max_requests": 1,
            "full_refresh_every": 2,
        }),
        0,
    )
    .expect("configure");
    let err = run(
        &root,
        "bad-cache",
        &json!([{
            "url": "https://example.test/a",
            "changes": [],
            "qualifier": "fresh",
            "cache_state": "cached",
        }]),
        configured["revision"].as_i64().unwrap(),
    )
    .expect_err("inconsistent cache");
    assert!(err.to_string().contains("inconsistent"), "got: {err}");
    let failed = run(
        &root,
        "failed",
        &json!([{
            "url": "https://example.test/a",
            "changes": [],
            "qualifier": "failed",
            "failure_reason": "connection reset",
        }]),
        configured["revision"].as_i64().unwrap(),
    )
    .expect("failed run");
    assert_eq!(failed["run"]["state"], "failed");
    assert_eq!(
        failed["coverage"]["failures"],
        json!([{"url": "https://example.test/a", "reason": "connection reset"}])
    );
    assert_eq!(failed["run"]["alerts"], json!([]));
    assert_eq!(failed["run"]["recoveries"], json!([]));
}

#[test]
fn recovery_is_derived_only_from_a_fresh_measured_return() {
    let dir = testkit::scratch("monitoring", "recovery");
    let root = project(&dir);
    let configured = configure(
        &root,
        &json!({
            "enabled": false,
            "urls": ["https://example.test/a"],
            "max_urls": 1,
            "max_requests": 1,
            "full_refresh_every": 2,
        }),
        0,
    )
    .expect("configure");
    let baseline = run(
        &root,
        "baseline",
        &json!([{"url": "https://example.test/a", "changes": [], "measurement": {"status": 200}}]),
        configured["revision"].as_i64().unwrap(),
    )
    .expect("baseline");
    let broken = run(
        &root,
        "broken",
        &json!([{"url": "https://example.test/a", "changes": [], "measurement": {"status": 500}}]),
        baseline["revision"].as_i64().unwrap(),
    )
    .expect("broken");
    let recovered = run(
        &root,
        "recovered",
        &json!([{"url": "https://example.test/a", "changes": [], "measurement": {"status": 200}}]),
        broken["revision"].as_i64().unwrap(),
    )
    .expect("recovered");
    assert_eq!(
        broken["run"]["alerts"][0]["changes"][0]["kind"],
        "status_changed"
    );
    assert_eq!(recovered["run"]["alerts"], json!([]));
    assert_eq!(
        recovered["run"]["recoveries"][0]["change"]["kind"],
        "recovered"
    );
}

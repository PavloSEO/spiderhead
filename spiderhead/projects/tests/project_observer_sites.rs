//! need the reference cache/monkeypatch seams or the mcp tool surface:
//! the per-site observe projection, and active-collection evidence on a
//! still-running scan. The mcp, `_EVIDENCE_CACHE` and `_FINDINGS_CACHE`
//! invalidation cases stay deferred.
//!
//! `prepare_project` is unported; competitor workspaces are declared through
//! `runtime::add_competitors`, the same write path prepare uses.

#[path = "common/testkit.rs"]
mod testkit;

use serde_json::{json, Value};
use spiderhead_projects::coverage::initialize_coverage;
use spiderhead_projects::observer::observe;
use spiderhead_projects::runtime::add_competitors;
use spiderhead_projects::workspace::create_project;

fn prepared(dir: &testkit::Scratch) -> (std::path::PathBuf, Vec<std::path::PathBuf>) {
    let root = dir.0.join("owner");
    create_project(
        &root,
        "https://owner.example.test/",
        Some("Owner"),
        None,
        Some(&json!(["ecommerce/product-card"])),
        Some(&json!(["ecommerce/basic"])),
    )
    .expect("create_project");
    initialize_coverage(&root, None).expect("coverage");
    let candidates = json!([
        {"url": "https://competitor-one.example.test/", "source": "synthetic shortlist", "observed_at": Value::Null},
        {"url": "https://competitor-two.example.test/", "source": "synthetic shortlist", "observed_at": Value::Null},
    ]);
    let rows = add_competitors(&root, &candidates).expect("add_competitors");
    let children = rows["competitors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| root.join(c["directory"].as_str().unwrap()))
        .collect();
    (root, children)
}

fn finished_scan(dir: &std::path::Path, name: &str) {
    std::fs::create_dir_all(dir.join("scans")).expect("scans dir");
    std::fs::copy(
        testkit::fixture("audited-cap500.sqlite"),
        dir.join("scans").join(name),
    )
    .expect("scan");
}

#[test]
fn observer_projects_scans_coverage_and_methods_per_declared_site() {
    let dir = testkit::scratch("observer-sites", "projection");
    let (root, children) = prepared(&dir);
    finished_scan(&root, "owner.sqlite");
    finished_scan(&children[0], "competitor-one.sqlite");
    finished_scan(&children[1], "competitor-two.sqlite");
    let owner_before = std::fs::read(root.join("scans").join("owner.sqlite")).expect("scan");
    let competitor_before =
        std::fs::read(children[0].join("scans").join("competitor-one.sqlite")).expect("scan");

    let snapshot = observe(&root, None, 1, 0, 20, None).expect("observe");

    let policy = &snapshot["policy"];
    assert_eq!(policy["ok"], true);
    assert_eq!(policy["applied"], false);
    assert_eq!(policy["revision"], 0);
    assert_eq!(
        policy["policy"]["approval_thresholds"],
        json!({"pages": 1000, "requests": 3000, "seconds": 600})
    );
    assert_eq!(
        policy["policy"]["quick_crawl"],
        json!({"pages": 50, "requests": 150, "seconds": 60})
    );
    assert!(policy["policy"]["crawl_profile"].is_null());
    assert_eq!(policy["policy"]["crawl_overrides"], json!({}));
    assert_eq!(policy["policy"]["competitor_limit"], 5);
    assert_eq!(
        policy["policy"]["evidence_hash"],
        json!({"max_bytes": 1073741824, "max_seconds": 5})
    );
    assert!(policy["operation_id"].is_null());

    assert_eq!(snapshot["sites"]["total"], 3);
    assert_eq!(snapshot["sites"]["scan_limit_per_site"], 1);
    let items = snapshot["sites"]["items"].as_array().unwrap();
    let (owner, competitor_one, competitor_two) = (&items[0], &items[1], &items[2]);
    assert_eq!(owner["role"], "primary");
    assert_eq!(owner["site"]["target"], "https://owner.example.test/");
    assert!(owner["candidate"].is_null());
    assert_eq!(
        owner["scans"]["items"][0]["artifact"],
        json!({"state": "available", "path": "scans/owner.sqlite"})
    );
    assert_eq!(owner["scans"]["items"][0]["evidence"]["state"], "available");
    assert_eq!(owner["coverage"]["state"], "initialized");
    assert!(owner["methods"]["kinds"]["scenario"]["expected"].is_null());
    assert!(
        owner["methods"]["kinds"]["scenario"]["applicable"]
            .as_i64()
            .unwrap()
            > 0
    );
    assert_eq!(owner["methods"]["kinds"]["scenario"]["completed"], 0);
    assert_eq!(competitor_one["role"], "competitor");
    assert_eq!(competitor_two["role"], "competitor");
    assert_eq!(
        competitor_one["candidate"]["state"],
        "candidate; audit not run"
    );
    assert_eq!(competitor_one["candidate"]["source"], "synthetic shortlist");
    assert!(competitor_one["scans"]["items"][0]["artifact"]["path"]
        .as_str()
        .unwrap()
        .starts_with("scans/"));
    assert_eq!(
        competitor_one["scans"]["items"][0]["evidence"]["state"],
        "available"
    );
    assert_eq!(
        competitor_two["site"]["target"],
        "https://competitor-two.example.test/"
    );
    // Legacy root-only projection remains compatible.
    assert_eq!(snapshot["scans"]["total"], 1);
    assert!(snapshot["methods"]
        .as_array()
        .unwrap()
        .iter()
        .all(|m| m["site"]["target"].as_str().is_some()));
    assert_eq!(
        std::fs::read(root.join("scans").join("owner.sqlite")).expect("scan"),
        owner_before
    );
    assert_eq!(
        std::fs::read(children[0].join("scans").join("competitor-one.sqlite")).expect("scan"),
        competitor_before
    );
}

#[test]
fn observer_keeps_active_collection_counters_when_audit_is_not_ready() {
    let dir = testkit::scratch("observer-sites", "active-counters");
    let root = dir.0.join("owner");
    create_project(&root, "https://owner.example.test/", None, None, None, None)
        .expect("create_project");
    std::fs::create_dir_all(root.join("scans")).expect("scans dir");
    std::fs::copy(
        testkit::fixture("running-scan.sqlite"),
        root.join("scans").join("active.sqlite"),
    )
    .expect("scan");

    let evidence = observe(&root, None, 1, 0, 20, None).expect("observe")["scans"]["items"][0]
        ["evidence"]
        .clone();

    assert_eq!(evidence["state"], "available");
    let counts = &evidence["frontier"]["counts"];
    for key in ["queued", "inflight", "done", "excluded"] {
        assert!(counts[key].is_number(), "{key} missing: {counts}");
    }
    assert_eq!(evidence["findings"]["state"], "unavailable");
    assert!(evidence["findings"]["reason"].as_str().is_some());
    assert!(evidence["findings"]["total"].is_null());
    assert_eq!(evidence["findings"]["by_severity"], json!({}));
    assert_eq!(evidence["findings"]["items"], json!([]));
    assert_eq!(evidence["findings"]["truncated"], false);
    assert!(evidence["skipped_checks"].is_null());
}

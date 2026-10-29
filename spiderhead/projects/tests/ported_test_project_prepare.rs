//! Ported the reference suite and the reference suite.
//!
//! `test_default_report_of_streamed_latest_scan` stays deferred: it exercises the
//! mcp `audit_workflow` report handler, which is outside the projects crate.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

type Calls = RefCell<Vec<(Value, bool, Option<String>)>>;

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("prep-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn project(dir: &Path) -> PathBuf {
    let proj = dir.join("project");
    spiderhead_projects::workspace::create_project(
        &proj,
        "https://example.test/",
        None,
        None,
        None,
        None,
    )
    .unwrap();
    std::fs::create_dir_all(proj.join("scans")).unwrap();
    proj
}

fn refusing<'a>(
    calls: &'a Calls,
    error: &'a str,
) -> impl Fn(&str, &Value, bool, Option<&str>) -> Result<Value, String> + 'a {
    move |_p, overrides, approve, build| {
        calls
            .borrow_mut()
            .push((overrides.clone(), approve, build.map(str::to_owned)));
        Ok(json!({"ok": false, "error": error}))
    }
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn copy_scan(proj: &Path, fixture_name: &str, dest: &str) -> PathBuf {
    let path = proj.join("scans").join(dest);
    std::fs::copy(fixture(fixture_name), &path).unwrap();
    path
}

#[test]
fn injected_crawl_failure_is_saved_as_bounded_not_run_preparation() {
    let dir = tmp("crawl-fail");
    let proj = project(&dir);
    let calls: Calls = RefCell::new(Vec::new());
    let tool = |_: &str, overrides: &Value, approve: bool, build: Option<&str>| {
        calls
            .borrow_mut()
            .push((overrides.clone(), approve, build.map(str::to_owned)));
        Ok(json!({"ok": false, "error": "synthetic crawler unavailable"}))
    };
    let result =
        spiderhead_projects::runtime::prepare_project(&proj, &tool, None, None, false, None, None)
            .unwrap();
    let preparation = &result["preparation"];
    assert_eq!(calls.borrow().len(), 1);
    assert_eq!(
        calls.borrow()[0].0,
        json!({
            "limits.max_urls": 50,
            "limits.max_requests": 150,
            "limits.max_crawl_seconds": 60,
            "sitemaps.auto_discover": true,
        })
    );
    assert_eq!(preparation["state"], "partial");
    assert_eq!(
        preparation["steps"]["crawl"],
        json!({"state": "not_run", "reason": "synthetic crawler unavailable"})
    );
    assert_eq!(
        preparation["steps"]["sitemap"],
        json!({
            "state": "not_run",
            "reason": "crawl source unavailable; sitemap coverage not established",
        })
    );
    assert_eq!(
        spiderhead_projects::runtime::preparation_status(&proj).unwrap()["steps"]["crawl"],
        preparation["steps"]["crawl"]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rerun_preserves_prior_operator_supplied_competitors_when_not_replaced() {
    let dir = tmp("competitors");
    let proj = project(&dir);
    let candidates = json!([
        {"url": "https://competitor-one.test/", "source": "operator shortlist", "observed_at": null},
        {"url": "https://competitor-two.test/", "source": "operator shortlist", "observed_at": null},
    ]);
    let calls: Calls = RefCell::new(Vec::new());
    let tool = refusing(&calls, "synthetic crawler unavailable");
    let first = spiderhead_projects::runtime::prepare_project(
        &proj,
        &tool,
        None,
        Some(&candidates),
        false,
        None,
        None,
    )
    .unwrap()["preparation"]
        .clone();
    let second =
        spiderhead_projects::runtime::prepare_project(&proj, &tool, None, None, false, None, None)
            .unwrap()["preparation"]
            .clone();
    let fold = |rows: &Value| -> Value {
        Value::Array(
            rows.as_array()
                .unwrap()
                .iter()
                .map(|row| {
                    let mut m = serde_json::Map::new();
                    for k in ["url", "source", "observed_at", "directory", "project_uuid"] {
                        m.insert(k.to_owned(), row[k].clone());
                    }
                    Value::Object(m)
                })
                .collect(),
        )
    };
    assert_eq!(fold(&second["competitors"]), fold(&first["competitors"]));
    assert_eq!(
        second["steps"]["competitors"],
        json!({
            "state": "run",
            "reason": "operator-supplied candidate shortlist; competitiveness is not verified",
            "count": 2,
        })
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preparation_reuses_latest_complete_scan_without_crawling() {
    let dir = tmp("reuse");
    let proj = project(&dir);
    copy_scan(&proj, "audited-cap500.sqlite", "full.sqlite");
    let calls: Calls = RefCell::new(Vec::new());
    let tool = refusing(&calls, "crawl must not run");
    let result =
        spiderhead_projects::runtime::prepare_project(&proj, &tool, None, None, false, None, None)
            .unwrap();
    assert!(calls.borrow().is_empty());
    let crawl = &result["preparation"]["steps"]["crawl"];
    assert_eq!(crawl["state"], "run");
    assert_eq!(crawl["reused"], true);
    assert_eq!(crawl["artifact"], "scans/full.sqlite");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn requested_cap_is_passed_to_the_crawl_and_scales_other_budgets() {
    let dir = tmp("cap-pass");
    let proj = project(&dir);
    let calls: Calls = RefCell::new(Vec::new());
    let tool = refusing(&calls, "crawl must not run");
    let _ = spiderhead_projects::runtime::prepare_project(
        &proj,
        &tool,
        None,
        None,
        false,
        None,
        Some(120),
    );
    assert_eq!(
        calls.borrow()[0].0,
        json!({
            "limits.max_urls": 120,
            "limits.max_requests": 360,
            "limits.max_crawl_seconds": 240,
            "sitemaps.auto_discover": true,
        })
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cap_stop_is_reported_as_reaching_the_owner_limit_not_partial() {
    let dir = tmp("cap-stop");
    let proj = project(&dir);
    copy_scan(&proj, "audited-cap120-stopped.sqlite", "capped.sqlite");
    let calls: Calls = RefCell::new(Vec::new());
    let tool = refusing(&calls, "crawl must not run");
    let result = spiderhead_projects::runtime::prepare_project(
        &proj,
        &tool,
        None,
        None,
        false,
        None,
        Some(120),
    )
    .unwrap();
    assert!(calls.borrow().is_empty());
    let crawl = &result["preparation"]["steps"]["crawl"];
    assert_eq!(crawl["state"], "run");
    assert_eq!(crawl["partial"], false);
    assert_eq!(crawl["reason"], "reached your URL limit (120)");
    assert_eq!(crawl["reused"], true);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn saved_scan_with_another_cap_is_not_reused_for_a_requested_cap() {
    let dir = tmp("cap-mismatch");
    let proj = project(&dir);
    copy_scan(&proj, "audited-cap500.sqlite", "other.sqlite");
    let calls: Calls = RefCell::new(Vec::new());
    let tool = refusing(&calls, "crawl must not run");
    let _ = spiderhead_projects::runtime::prepare_project(
        &proj,
        &tool,
        None,
        None,
        false,
        None,
        Some(120),
    );
    assert_eq!(calls.borrow().len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

// (the True/"50" inputs are reference dynamic typing; the Rust signature takes Option<i64>)
#[test]
fn non_integer_or_out_of_range_cap_is_refused() {
    let dir = tmp("cap-refused");
    let proj = project(&dir);
    for cap in [0i64, -1, 1_000_001] {
        let calls: Calls = RefCell::new(Vec::new());
        let tool = refusing(&calls, "crawl must not run");
        let err = spiderhead_projects::runtime::prepare_project(
            &proj,
            &tool,
            None,
            None,
            false,
            None,
            Some(cap),
        )
        .unwrap_err();
        assert!(err.to_string().contains("max_urls"), "{cap}: {err}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn latest_project_scan_is_the_newest_complete_audited_scan() {
    let dir = tmp("latest");
    let proj = project(&dir);
    copy_scan(&proj, "audited-cap50-stopped.sqlite", "partial.sqlite");
    copy_scan(&proj, "audited-cap500.sqlite", "full.sqlite");
    let latest = spiderhead_projects::runtime::latest_project_scan(&proj).unwrap();
    assert_eq!(latest.file_name().unwrap(), "full.sqlite");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn latest_project_scan_refuses_when_no_complete_scan_exists() {
    let dir = tmp("latest-none");
    let proj = project(&dir);
    let err = spiderhead_projects::runtime::latest_project_scan(&proj).unwrap_err();
    assert!(
        err.to_string().contains("no finished, audited scan"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

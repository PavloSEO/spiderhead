//! `tools={}` missing-crawler refusal has no Rust equivalent: `CompetitorTools`
//! makes `crawl_site` structurally required, so it stays a reference-seam note.
//! `KeyboardInterrupt` becomes `CrawlFault::Interrupted`, and `_read_scan`
//! monkeypatches become real retained-scan fixtures the injected crawl copies.

#[path = "common/testkit.rs"]
mod testkit;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};
use spiderhead_projects::competitor_scans::{
    cancel_competitor_scan_run, cancel_path, get_competitor_scan_run, read_manifest,
    scan_task_competitors, write_manifest, CompetitorTools, CrawlFault, SiteProgress, RUN_LOCK,
};
use spiderhead_projects::coverage;
use spiderhead_projects::observer;
use spiderhead_projects::runtime::add_competitors;
use spiderhead_projects::schedule;
use spiderhead_projects::workspace::create_project;

fn project(
    dir: &testkit::Scratch,
    count: usize,
    max_urls: i64,
) -> (std::path::PathBuf, Vec<Value>, i64) {
    let root = dir.0.join("owner");
    create_project(&root, "https://owner.example.test/", None, None, None, None).expect("create");
    coverage::initialize_coverage(&root, None).expect("coverage");
    let candidates: Vec<Value> = (0..count)
        .map(|i| json!(format!("https://rival-{i}.example.test/")))
        .collect();
    let rows = add_competitors(&root, &json!(candidates)).expect("add_competitors")["competitors"]
        .as_array()
        .unwrap()
        .clone();
    for row in &rows {
        let child = root.join(row["directory"].as_str().unwrap());
        schedule::configure(
            &child,
            0,
            None,
            &Map::from_iter([(
                "crawl".into(),
                json!({"max_urls": max_urls, "max_requests": 30_000}),
            )]),
        )
        .expect("schedule");
    }
    let definition = json!({
        "id": "custom:competitor-audit",
        "title": "Audit selected competitors",
        "competitors": rows.iter().map(|r| json!({"url": r["url"], "max_urls": max_urls})).collect::<Vec<_>>(),
    });
    let rev = coverage::coverage_status(&root, false).expect("status")["revision"]
        .as_i64()
        .unwrap();
    coverage::update_item(&root, &definition, rev).expect("update_item");
    let final_rev = coverage::coverage_status(&root, false).expect("status")["revision"]
        .as_i64()
        .unwrap();
    (root, rows, final_rev)
}

/// `_write_artifact` in the oracle wrote junk bytes; `history::read_scan` needs
/// a real retained scan, so the injected crawl copies a finished fixture.
fn write_scan_artifact(kwargs: &Map<String, Value>, fixture: &str) -> std::path::PathBuf {
    let path = std::path::PathBuf::from(
        kwargs["scan_out"]
            .as_str()
            .or_else(|| kwargs["resume"].as_str())
            .expect("artifact kwarg"),
    );
    std::fs::create_dir_all(path.parent().unwrap()).expect("artifact dir");
    if !path.exists() {
        std::fs::copy(testkit::fixture(fixture), &path).expect("fixture scan");
    }
    path
}

fn tools<'a>(
    crawl: &'a spiderhead_projects::competitor_scans::CrawlSite<'a>,
) -> CompetitorTools<'a> {
    CompetitorTools {
        crawl_site: crawl,
        progress: None,
        cancel_requested: None,
    }
}

#[test]
fn aggregate_request_budget_refuses_before_crawl_or_lock() {
    let dir = testkit::scratch("competitor", "budget");
    let (root, _rows, revision) = project(&dir, 5, 10_000);
    let calls = Mutex::new(Vec::new());
    let crawl =
        |kwargs: &Map<String, Value>, _progress: &SiteProgress| -> Result<Value, CrawlFault> {
            calls.lock().unwrap().push(kwargs.clone());
            Ok(json!({"ok": true}))
        };
    let err = scan_task_competitors(
        &root,
        "custom:competitor-audit",
        revision,
        true,
        1,
        false,
        None,
        &tools(&crawl),
        None,
    )
    .expect_err("budget refuses");
    assert!(
        err.to_string().contains("aggregate competitor"),
        "got: {err}"
    );
    assert!(calls.lock().unwrap().is_empty());
    assert!(!root.join(RUN_LOCK).exists());
}

#[test]
fn successful_run_records_per_site_and_fixed_budgets() {
    let dir = testkit::scratch("competitor", "successful");
    let (root, rows, revision) = project(&dir, 1, 500);
    let calls = Mutex::new(Vec::new());
    let crawl =
        |kwargs: &Map<String, Value>, progress: &SiteProgress| -> Result<Value, CrawlFault> {
            calls.lock().unwrap().push(kwargs.clone());
            let artifact = write_scan_artifact(kwargs, "audited-cap500.sqlite");
            progress(4, 2).map_err(|(n, m)| CrawlFault::Interrupted(n, m))?;
            Ok(json!({"ok": true, "scan": artifact.to_str().unwrap()}))
        };
    let result = scan_task_competitors(
        &root,
        "custom:competitor-audit",
        revision,
        true,
        1,
        false,
        None,
        &tools(&crawl),
        None,
    )
    .expect("run");

    assert_eq!(result["state"], "completed");
    assert_eq!(result["ok"], true);
    assert_eq!(
        result["budgets"],
        json!({
            "urls": 500,
            "requests": 1_500,
            "seconds": 1_000,
            "max_urls": 50_000,
            "max_requests": 60_000,
            "max_seconds": 28_800,
        })
    );
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["max_urls"], 500);
    assert_eq!(calls[0]["overrides"]["limits.max_requests"], 1_500);
    assert_eq!(calls[0]["overrides"]["limits.max_crawl_seconds"], 1_000);
    assert_eq!(
        result["results"][0]["project_uuid"],
        rows[0]["project_uuid"]
    );
    assert_eq!(result["results"][0]["fetched"], 4);
}

#[test]
fn recovery_skips_completed_child_and_resumes_only_interrupted_child() {
    let dir = testkit::scratch("competitor", "recovery");
    let (root, rows, revision) = project(&dir, 2, 100);
    let run_id = "00000000-0000-4000-8000-000000000042";
    let second_url = rows[1]["url"].as_str().unwrap().to_string();
    let calls = Mutex::new(Vec::new());

    let interrupted_crawl =
        |kwargs: &Map<String, Value>, _progress: &SiteProgress| -> Result<Value, CrawlFault> {
            calls.lock().unwrap().push(kwargs.clone());
            // The interrupted child retains a partial scan (crawl_partial=1),
            // like the monkeypatched `_read_scan` returning `partial=True`.
            let fixture = if kwargs["url"].as_str() == Some(second_url.as_str()) {
                "audited-cap50-stopped.sqlite"
            } else {
                "audited-cap500.sqlite"
            };
            let artifact = write_scan_artifact(kwargs, fixture);
            if kwargs["url"].as_str() == Some(second_url.as_str()) {
                return Err(CrawlFault::Interrupted(
                    "KeyboardInterrupt",
                    "synthetic interruption".into(),
                ));
            }
            Ok(json!({"ok": true, "scan": artifact.to_str().unwrap()}))
        };
    scan_task_competitors(
        &root,
        "custom:competitor-audit",
        revision,
        true,
        1,
        false,
        None,
        &tools(&interrupted_crawl),
        Some(run_id),
    )
    .expect_err("interrupted");

    let status = get_competitor_scan_run(&root, run_id).expect("status");
    assert_eq!(status["state"], "interrupted");
    assert_eq!(status["results"][0]["state"], "completed");
    assert_eq!(status["results"][1]["state"], "interrupted");
    let detail = observer::task_detail(&root, "custom:competitor-audit").expect("detail");
    let reference = &detail["item"]["task"]["competitor_run"];
    assert_eq!(reference["run_id"], run_id);
    assert_eq!(reference["state"], "interrupted");
    assert_eq!(reference["resumable"], true);
    assert_eq!(reference["coverage_revision"], revision);

    coverage::append_task_comment(
        &root,
        "custom:competitor-audit",
        "Unrelated task note after the scan was interrupted.",
        "agent/test",
        revision,
        None,
    )
    .expect("comment");
    let updated_revision = coverage::coverage_status(&root, false).expect("status")["revision"]
        .as_i64()
        .unwrap();
    let refreshed = observer::task_detail(&root, "custom:competitor-audit").expect("detail");
    assert_eq!(refreshed["revision"], updated_revision);
    assert_eq!(
        refreshed["item"]["task"]["competitor_run"]["run_id"],
        run_id
    );
    assert_eq!(
        refreshed["item"]["task"]["competitor_run"]["resumable"],
        true
    );

    let interrupted_path = root.join(status["results"][1]["scan"].as_str().unwrap());
    let second_calls = Mutex::new(Vec::new());
    let resume_crawl =
        |kwargs: &Map<String, Value>, _progress: &SiteProgress| -> Result<Value, CrawlFault> {
            second_calls.lock().unwrap().push(kwargs.clone());
            assert!(kwargs.get("resume").is_some() && kwargs.get("scan_out").is_none());
            // The resumed crawl finishes the retained partial scan in place;
            // a complete fixture stands in for the rewritten artifact.
            let path = std::path::PathBuf::from(kwargs["resume"].as_str().unwrap());
            std::fs::copy(testkit::fixture("audited-cap500.sqlite"), &path)
                .expect("finish retained scan");
            Ok(json!({"ok": true, "scan": kwargs["resume"].clone()}))
        };
    let recovered = scan_task_competitors(
        &root,
        "custom:competitor-audit",
        updated_revision,
        true,
        1,
        false,
        None,
        &tools(&resume_crawl),
        Some(run_id),
    )
    .expect("recovery");

    assert_eq!(recovered["state"], "completed");
    assert_eq!(recovered["resumed"], true);
    assert_eq!(recovered["coverage_revision"], revision);
    let second_calls = second_calls.lock().unwrap();
    assert_eq!(second_calls.len(), 1);
    assert_eq!(
        second_calls[0]["project"],
        root.join(rows[1]["directory"].as_str().unwrap())
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap()
    );
    assert_eq!(calls.lock().unwrap().len(), 2);
    assert!(interrupted_path.exists());
}

#[test]
fn run_id_status_and_cancel_marker_are_durable() {
    let dir = testkit::scratch("competitor", "cancel");
    let (root, _rows, revision) = project(&dir, 1, 500);
    let run_id = "00000000-0000-4000-8000-000000000043";
    let started = Arc::new(std::sync::Barrier::new(2));
    let errors = Arc::new(Mutex::new(Vec::new()));

    let started2 = Arc::clone(&started);
    let errors2 = Arc::clone(&errors);
    let root2 = root.clone();
    let thread = std::thread::spawn(move || {
        let waiting_crawl =
            |_kwargs: &Map<String, Value>, progress: &SiteProgress| -> Result<Value, CrawlFault> {
                started2.wait();
                loop {
                    match progress(1, 1) {
                        Err((name, msg)) => return Err(CrawlFault::Interrupted(name, msg)),
                        Ok(()) => std::thread::sleep(std::time::Duration::from_millis(10)),
                    }
                }
            };
        // A partial scan exists behind the running manifest.
        if let Err(e) = scan_task_competitors(
            &root2,
            "custom:competitor-audit",
            revision,
            true,
            1,
            false,
            None,
            &tools(&waiting_crawl),
            Some(run_id),
        ) {
            errors2.lock().unwrap().push(e);
        }
    });
    started.wait();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut active;
    loop {
        active = get_competitor_scan_run(&root, run_id).expect("status");
        if active["state"] == "running" && active["runner_active"] == true {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "run never went live");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let requested = cancel_competitor_scan_run(&root, run_id).expect("cancel");
    assert_eq!(requested["cancelled"], true);
    thread.join().expect("join");

    assert!(
        !errors.lock().unwrap().is_empty(),
        "interrupted run must surface an error"
    );
    let final_status = get_competitor_scan_run(&root, run_id).expect("status");
    assert_eq!(final_status["state"], "interrupted");
    assert_eq!(final_status["results"][0]["state"], "interrupted");
}

#[test]
fn status_and_cancel_bind_the_live_lock_to_its_exact_run() {
    let dir = testkit::scratch("competitor", "lock-binding");
    let (root, _rows, revision) = project(&dir, 1, 500);
    let stale_id = "00000000-0000-4000-8000-000000000051";
    let live_id = "00000000-0000-4000-8000-000000000052";

    let interrupted_crawl =
        |_kwargs: &Map<String, Value>, _progress: &SiteProgress| -> Result<Value, CrawlFault> {
            Err(CrawlFault::Interrupted(
                "KeyboardInterrupt",
                "leave a stale running manifest".into(),
            ))
        };
    scan_task_competitors(
        &root,
        "custom:competitor-audit",
        revision,
        true,
        1,
        false,
        None,
        &tools(&interrupted_crawl),
        Some(stale_id),
    )
    .expect_err("interrupted");
    let project_uuid = spiderhead_projects::workspace::load(&root).expect("load").1["project_uuid"]
        .as_str()
        .unwrap()
        .to_string();
    let mut stale = read_manifest(&root, stale_id, &project_uuid).expect("manifest");
    stale["state"] = json!("running");
    stale["runner_token"] = json!("a".repeat(32));
    for site in stale["sites"].as_array_mut().unwrap() {
        site["state"] = json!("running");
    }
    write_manifest(&root, &stale).expect("write");

    let started = Arc::new(AtomicBool::new(false));
    let started2 = Arc::clone(&started);
    let root2 = root.clone();
    let thread = std::thread::spawn(move || {
        let waiting_crawl =
            |_kwargs: &Map<String, Value>, progress: &SiteProgress| -> Result<Value, CrawlFault> {
                started2.store(true, Ordering::SeqCst);
                loop {
                    match progress(1, 1) {
                        Err((name, msg)) => return Err(CrawlFault::Interrupted(name, msg)),
                        Ok(()) => std::thread::sleep(std::time::Duration::from_millis(10)),
                    }
                }
            };
        let _ = scan_task_competitors(
            &root2,
            "custom:competitor-audit",
            revision,
            true,
            1,
            false,
            None,
            &tools(&waiting_crawl),
            Some(live_id),
        );
    });
    while !started.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    let status = get_competitor_scan_run(&root, stale_id).expect("status");
    assert_eq!(status["state"], "interrupted");
    assert_eq!(status["runner_active"], false);
    let cancelled = cancel_competitor_scan_run(&root, stale_id).expect("cancel");
    assert_eq!(cancelled["cancelled"], false);
    assert!(!cancel_path(&root, stale_id, false)
        .expect("cancel_path")
        .exists());

    assert_eq!(
        cancel_competitor_scan_run(&root, live_id).expect("cancel")["cancelled"],
        true
    );
    thread.join().expect("join");
}

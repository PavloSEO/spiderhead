//! `sf_cli.main` export case belongs to the sf/cli surface and stays deferred.
//!
//! The os.kill and multiprocess-spawn seams become real child processes or
//! dead-controller doctoring, which the same module already uses elsewhere.

#[path = "common/testkit.rs"]
mod testkit;

use serde_json::{json, Map, Value};
use spiderhead_projects::run_observation::{self, NAME};
use spiderhead_projects::workspace::create_project;

fn project(dir: &testkit::Scratch) -> std::path::PathBuf {
    let root = dir.0.join("project");
    create_project(&root, "https://example.test/", None, None, None, None).expect("create");
    root
}

fn start(root: &std::path::Path) -> Value {
    try_start(root).expect("start")
}

fn try_start(root: &std::path::Path) -> spiderhead_projects::ProjectsResult<Value> {
    run_observation::start(
        root,
        "native",
        "spider",
        100,
        0,
        0,
        None,
        "synthetic",
        Some(&format!("{}/scans/live.sqlite", root.display())),
        false,
        None,
        None,
        None,
        None,
    )
}

fn spawn_dead() -> i64 {
    let mut child = std::process::Command::new("true").spawn().expect("spawn");
    child.wait().expect("wait");
    child.id() as i64
}

fn child_identity(pid: i64) -> String {
    let output = std::process::Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .expect("ps");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[test]
fn project_run_records_real_counters_and_no_false_site_percentage() {
    let dir = testkit::scratch("runobs", "counters");
    let root = project(&dir);
    let run = run_observation::start(
        &root,
        "native",
        "spider",
        100,
        0,
        0,
        None,
        "synthetic",
        Some(&format!("{}/scans/scan.sqlite", root.display())),
        false,
        None,
        None,
        None,
        None,
    )
    .expect("start");
    let run_id = run["id"].as_str().unwrap();
    run_observation::progress(
        &root,
        run_id,
        Some(9),
        Some(91),
        Some(1),
        Some(3),
        None,
        None,
        "separate",
        "collector",
    )
    .expect("progress");
    run_observation::phase(&root, run_id, "analysis", "analysis").expect("phase");

    let live = run_observation::status(&root, 20).expect("status");
    let item = &live["items"][0];
    assert_eq!(item["pid_state"], "live");
    assert_eq!(
        item["collector"],
        json!({
            "mode": "spider",
            "max_urls": 100,
            "max_requests": 0,
            "max_crawl_seconds": 0,
            "max_requests_per_second": Value::Null,
            "config_fingerprint": "synthetic",
            "resumed": false,
            "origin": Value::Null,
            "aggregate_max_requests_per_second": Value::Null,
        })
    );
    assert_eq!(item["artifact"], "scans/scan.sqlite");
    assert_eq!(
        item["counters"],
        json!({"fetched": 9, "queued": 91, "inflight": 1, "excluded": 3, "rate_per_second": Value::Null})
    );
    assert!(item.get("percent").is_none() || item["percent"].is_null());
    assert!(item.get("site_total").is_none() || item["site_total"].is_null());
    let phases: Vec<_> = item["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["phase"].as_str().unwrap())
        .collect();
    assert_eq!(phases, ["admission", "collection", "analysis"]);

    run_observation::finish(
        &root,
        run_id,
        "partial",
        "url_limit",
        Some(&Map::from_iter([
            ("fetched".into(), json!(100)),
            ("queued".into(), json!(17)),
            ("inflight".into(), json!(0)),
            ("excluded".into(), json!(3)),
            ("rate_per_second".into(), json!(2.0)),
        ])),
    )
    .expect("finish");
    let retained = run_observation::status(&root, 20).expect("status")["items"][0].clone();
    assert_eq!(retained["state"], "partial");
    assert_eq!(retained["pid_state"], "retained");
    assert_eq!(retained["finish_reason"], "url_limit");
    assert_eq!(retained["counters"]["rate_per_second"], 2.0);
}

#[test]
fn observer_read_marks_dead_launcher_abandoned_without_mutating_document() {
    let dir = testkit::scratch("runobs", "abandoned");
    let root = project(&dir);
    let run = start(&root);
    let dead = spawn_dead();
    testkit::doctor_runs(&root, |doc| {
        doc["runs"][0]["pid"] = json!(dead);
        doc["runs"][0]["controller_pid"] = json!(dead);
        doc["runs"][0]["controller_start_identity"] = json!("dead-pid");
    });
    let document = root.join(NAME);
    let before = std::fs::read(&document).expect("document");

    let result = run_observation::status(&root, 20).expect("status");

    assert_eq!(result["items"][0]["id"], run["id"]);
    assert_eq!(result["items"][0]["state"], "running");
    assert_eq!(result["items"][0]["pid_state"], "abandoned");
    assert_eq!(std::fs::read(&document).expect("document"), before);
}

#[test]
fn second_process_can_read_a_live_attempt_then_reconnect_to_retained_history() {
    let dir = testkit::scratch("runobs", "live-then-abandoned");
    let root = project(&dir);
    start(&root);
    // A real live child stands in for the spawned controller process: its pid
    // and recorded identity make the record "live" for an outside reader.
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("sleep");
    let pid = child.id() as i64;
    let identity = child_identity(pid);
    testkit::doctor_runs(&root, |doc| {
        doc["runs"][0]["pid"] = json!(pid);
        doc["runs"][0]["controller_pid"] = json!(pid);
        doc["runs"][0]["controller_start_identity"] = json!(identity);
    });
    let running = run_observation::status(&root, 20).expect("status");
    assert_eq!(running["items"][0]["pid_state"], "live");
    assert_eq!(running["items"][0]["state"], "running");

    // The child ends without a terminal update; a replacement observer names
    // that rather than changing the retained record.
    child.kill().expect("kill");
    child.wait().expect("wait");
    let after = run_observation::status(&root, 20).expect("status");
    assert_eq!(after["items"][0]["pid_state"], "abandoned");
}

#[test]
fn actual_collector_child_is_distinct_from_its_live_controller() {
    let dir = testkit::scratch("runobs", "collector-child");
    let root = project(&dir);
    let run = run_observation::start(
        &root,
        "screaming_frog",
        "sf_live",
        0,
        0,
        0,
        None,
        "synthetic",
        Some(&format!("{}/reports/sf", root.display())),
        false,
        None,
        None,
        None,
        Some(&Map::from_iter([
            ("fetched".into(), Value::Null),
            ("queued".into(), Value::Null),
            ("inflight".into(), Value::Null),
            ("excluded".into(), Value::Null),
        ])),
    )
    .expect("start");
    let run_id = run["id"].as_str().unwrap().to_string();
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("sleep");
    let child_pid = child.id() as i64;
    run_observation::collector_started(&root, &run_id, child_pid).expect("collector_started");
    let active = run_observation::status(&root, 20).expect("status")["items"][0].clone();
    assert_eq!(
        active["controller"]["pid"].as_i64().unwrap() as u32,
        std::process::id()
    );
    assert_eq!(active["collector_runtime"]["pid"], child_pid);
    assert_ne!(
        active["controller"]["pid"].as_i64().unwrap(),
        active["collector_runtime"]["pid"].as_i64().unwrap()
    );
    assert_eq!(active["collector_runtime"]["state"], "live");
    assert!(active["collector_runtime"]["observed_at"]
        .as_str()
        .is_some());

    child.kill().expect("kill");
    child.wait().expect("wait");
    let finished_child = run_observation::status(&root, 20).expect("status")["items"][0].clone();
    assert_eq!(finished_child["state"], "running");
    assert_eq!(finished_child["collector_runtime"]["state"], "abandoned");
}

#[test]
fn two_processes_keep_both_terminal_run_updates_after_revision_contention() {
    // Two spawn-context children in the oracle; the port keeps the concurrent
    // terminal-write pressure with threads on two doctored run records.
    let dir = testkit::scratch("runobs", "two-terminal");
    let root = project(&dir);
    let mut ids = Vec::new();
    for _ in 0..2 {
        let run = start(&root);
        let dead = spawn_dead();
        let run_id = run["id"].as_str().unwrap().to_string();
        testkit::doctor_runs(&root, |doc| {
            let row = doc["runs"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|r| r["id"] == run_id)
                .unwrap();
            row["pid"] = json!(dead);
            row["controller_pid"] = json!(dead);
            row["controller_start_identity"] = json!("dead-pid");
        });
        ids.push(run["id"].as_str().unwrap().to_string());
    }
    let root = std::sync::Arc::new(root);
    let mut threads = Vec::new();
    for id in &ids {
        let root = std::sync::Arc::clone(&root);
        let id = id.clone();
        threads.push(std::thread::spawn(move || {
            run_observation::finish(&root, &id, "finished", "finished", None)
        }));
    }
    for thread in threads {
        thread.join().expect("finish").expect("finish");
    }
    let rows = run_observation::status(&root, 20).expect("status")["items"]
        .as_array()
        .unwrap()
        .clone();
    let got: std::collections::HashSet<_> = rows
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(got, ids.into_iter().collect());
    assert!(rows.iter().all(|r| r["state"] == "finished"));
}

#[test]
fn out_of_range_collector_pid_is_unknown_not_an_observer_crash() {
    let dir = testkit::scratch("runobs", "pid-range");
    let root = project(&dir);
    let run = run_observation::start(
        &root,
        "screaming_frog",
        "sf_live",
        0,
        0,
        0,
        None,
        "synthetic",
        Some(&format!("{}/reports/sf", root.display())),
        false,
        None,
        None,
        None,
        None,
    )
    .expect("start");
    run_observation::collector_started(&root, run["id"].as_str().unwrap(), i64::MAX)
        .expect("collector_started");
    assert_eq!(
        run_observation::status(&root, 20).expect("status")["items"][0]["collector_runtime"]
            ["state"],
        "unknown"
    );
}

#[test]
fn second_start_is_refused_while_a_live_scan_runs() {
    let dir = testkit::scratch("runobs", "busy");
    let root = project(&dir);
    let first = start(&root);
    let err = try_start(&root).expect_err("second start must refuse");
    assert_eq!(
        err.to_string(),
        format!(
            "project busy; owner={} pid={}",
            first["id"].as_str().unwrap(),
            std::process::id()
        )
    );
}

#[test]
fn abandoned_scan_does_not_block_a_new_start() {
    let dir = testkit::scratch("runobs", "abandoned-start");
    let root = project(&dir);
    start(&root);
    let dead = spawn_dead();
    testkit::doctor_runs(&root, |doc| {
        doc["runs"][0]["pid"] = json!(dead);
        doc["runs"][0]["controller_pid"] = json!(dead);
        doc["runs"][0]["controller_start_identity"] = json!("dead-pid");
    });
    assert_eq!(
        run_observation::status(&root, 20).expect("status")["items"][0]["pid_state"],
        "abandoned"
    );
    assert_eq!(start(&root)["state"], "running");
}

#[test]
fn simultaneous_starts_admit_exactly_one_scan() {
    let dir = testkit::scratch("runobs", "one-scan");
    let root = std::sync::Arc::new(project(&dir));
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut threads = Vec::new();
    for _ in 0..2 {
        let root = std::sync::Arc::clone(&root);
        let barrier = std::sync::Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            match run_observation::start(
                &root,
                "native",
                "spider",
                100,
                0,
                0,
                None,
                "synthetic",
                Some(&format!("{}/scans/live.sqlite", root.display())),
                false,
                None,
                None,
                None,
                None,
            ) {
                Ok(_) => "started".to_string(),
                Err(e) => e.to_string(),
            }
        }));
    }
    let results: Vec<_> = threads
        .into_iter()
        .map(|t| t.join().expect("join"))
        .collect();
    assert_eq!(results.iter().filter(|r| *r == "started").count(), 1);
    let busy = results.iter().find(|r| *r != "started").expect("busy");
    assert!(busy.starts_with("project busy; owner="), "got: {busy}");
    let status = run_observation::status(&root, 20).expect("status");
    let running: Vec<_> = status["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["state"] == "running")
        .collect();
    assert_eq!(running.len(), 1);
}

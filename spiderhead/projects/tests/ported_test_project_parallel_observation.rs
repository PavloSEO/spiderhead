//! cases that do not need reference seams. The shared-schedule and cross-process
//! pacing tests monkeypatch `origin_pacing.time.time` (no Rust clock seam) and
//! `_SharedDispatchGate` lives in the crawl slice; those stay deferred.
//!
//! The three-simultaneous-runs case patches `run_observation._owner_busy` in
//! the oracle so several live records can coexist in one process. The port
//! instead doctors each run's controller PID to a dead process between starts
//! (the same mechanism the run-control ports use), then performs the progress
//! writes concurrently.

#[path = "common/testkit.rs"]
mod testkit;

use serde_json::json;
use spiderhead_projects::origin_pacing::ProjectOriginPacer;
use spiderhead_projects::run_observation;
use spiderhead_projects::workspace::create_project;

const TARGET: &str = "https://example.test/";

fn project(dir: &testkit::Scratch) -> std::path::PathBuf {
    let root = dir.0.join("project");
    create_project(&root, TARGET, None, None, None, None).expect("create");
    root
}

fn dead_pid() -> i64 {
    let mut child = std::process::Command::new("true").spawn().expect("spawn");
    child.wait().expect("wait");
    child.id() as i64
}

#[test]
fn three_simultaneous_runs_keep_independent_uuid_artifact_and_progress() {
    let dir = testkit::scratch("parallel", "three-runs");
    let root = project(&dir);
    let mut runs = Vec::new();
    for index in 0..3 {
        let run = run_observation::start(
            &root,
            "native",
            "spider",
            10,
            20,
            30,
            Some(2.0),
            &format!("config-{index}"),
            Some(&format!("{}/scans/run-{index}.sqlite", root.display())),
            false,
            Some("example.test"),
            Some(2.0),
            None,
            None,
        )
        .expect("start");
        // The oracle's autouse fixture bypasses `_owner_busy`; doctoring the
        // just-created record to a dead controller is the equivalent.
        let dead = dead_pid();
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
        runs.push(run);
    }

    // Concurrent progress writes — the part of the test that exercises
    // simultaneous writers on the shared document.
    let root = std::sync::Arc::new(root);
    let mut threads = Vec::new();
    for (index, run) in runs.iter().enumerate() {
        let root = std::sync::Arc::clone(&root);
        let run_id = run["id"].as_str().unwrap().to_string();
        threads.push(std::thread::spawn(move || {
            run_observation::progress(
                &root,
                &run_id,
                Some(index as i64 + 1),
                Some(10 - index as i64),
                Some(1),
                Some(0),
                Some(1.0),
                None,
                "separate",
                "collector",
            )
        }));
    }
    for thread in threads {
        thread.join().expect("progress").expect("progress");
    }
    assert_eq!(
        runs.iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect::<std::collections::HashSet<_>>()
            .len(),
        3
    );

    let status = run_observation::status(&root, 3).expect("status");
    assert_eq!(status["active_total"], 3);
    let items = status["items"].as_array().unwrap();
    assert!(items.iter().all(|r| r["source_kind"] == "native"));
    assert_eq!(
        items
            .iter()
            .map(|r| r["artifact"].as_str().unwrap().to_string())
            .collect::<std::collections::HashSet<_>>(),
        (0..3)
            .map(|i| format!("scans/run-{i}.sqlite"))
            .collect::<std::collections::HashSet<_>>()
    );
    assert_eq!(
        items
            .iter()
            .map(|r| r["collector"]["config_fingerprint"]
                .as_str()
                .unwrap()
                .to_string())
            .collect::<std::collections::HashSet<_>>(),
        (0..3)
            .map(|i| format!("config-{i}"))
            .collect::<std::collections::HashSet<_>>()
    );
    assert!(items
        .iter()
        .all(|r| r["collector"]["aggregate_max_requests_per_second"] == 2.0));
    assert!(items.iter().all(|r| r["telemetry"]["sampled_at"]
        .as_str()
        .is_some_and(|v| !v.is_empty())));
}

#[test]
fn caller_uuid_is_retained_as_the_observer_handshake() {
    let dir = testkit::scratch("parallel", "caller-uuid");
    let root = project(&dir);
    let requested = uuid::Uuid::new_v4().to_string();
    let run = run_observation::start(
        &root,
        "native",
        "spider",
        1,
        0,
        0,
        None,
        "config",
        Some(&format!("{}/scans/run.sqlite", root.display())),
        false,
        None,
        None,
        Some(&requested),
        None,
    )
    .expect("start");
    assert_eq!(run["id"], requested);
    let err = run_observation::start(
        &root,
        "native",
        "spider",
        1,
        0,
        0,
        None,
        "other",
        Some(&format!("{}/scans/other.sqlite", root.display())),
        false,
        None,
        None,
        Some(&requested),
        None,
    )
    .expect_err("duplicate run id refuses");
    assert!(err.to_string().contains("already exists"), "got: {err}");
}

#[test]
fn explicit_unlimited_project_pacer_does_not_reserve_a_hidden_two_per_second_slot() {
    let dir = testkit::scratch("parallel", "unlimited-pacer");
    let root = project(&dir);
    let pacer = ProjectOriginPacer::new(Some(&root), TARGET, 0.0, 0.0).expect("pacer");
    assert_eq!(
        (0..10)
            .map(|_| pacer.reserve().unwrap())
            .collect::<Vec<_>>(),
        vec![0.0; 10]
    );
    assert!(!pacer.path().exists());
}

#[test]
fn pacer_refuses_unbounded_intervals_and_future_store_waits() {
    let dir = testkit::scratch("parallel", "pacer-refusals");
    let root = project(&dir);
    let slow = ProjectOriginPacer::new(Some(&root), TARGET, 120.0, 2.0).expect("pacer");
    assert!((slow.interval_seconds() - 0.5).abs() < 1e-9);
    let pacer = ProjectOriginPacer::new(Some(&root), TARGET, 0.0, 2.0).expect("pacer");
    pacer.reserve().expect("reserve");
    let con = rusqlite::Connection::open(pacer.path()).expect("store");
    con.execute(
        "UPDATE origin_turns SET next_at=?1",
        rusqlite::params![now_seconds() + 61.0],
    )
    .expect("doctor");
    drop(con);
    let err = pacer.reserve().expect_err("future wait refuses");
    assert!(err.to_string().contains("excessive wait"), "got: {err}");
}

fn now_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs_f64()
}

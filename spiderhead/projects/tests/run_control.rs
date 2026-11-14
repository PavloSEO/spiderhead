//! active, live native scans in the selected project.
//!
//! The reference suite fakes `storage.open_scan` and `run_observation.status`;
//! the port instead retains a real running-scan SQLite fixture and real
//! `run-observation.json` runs, doctoring the persisted document where the
//! reference tests inject a status row. The two post-insert recheck cases need
//! interposition *between* `request`'s two `status` reads — a reference-only
//! monkeypatch seam, deferred in `oracle/deferred-tests.toml`.

#[path = "common/testkit.rs"]
mod testkit;

use spiderhead_projects::run_control;

fn request_err(root: &std::path::Path, run_id: &str, action: &str) -> String {
    run_control::request(root, run_id, action)
        .err()
        .map(|e| e.to_string())
        .unwrap_or_else(|| panic!("request {action} unexpectedly succeeded"))
}

#[test]
fn pause_request_is_consumed_once_and_checkpoints_by_run_id() {
    let (_dir, root, run_id) = testkit::live_project("runctl", "pause-once");
    let requested = run_control::request(&root, &run_id, "pause").expect("request pause");
    assert_eq!(
        requested,
        serde_json::json!({"ok": true, "run_id": run_id, "state": "pause_requested"})
    );
    assert_eq!(
        run_control::consume(&root, &run_id).expect("consume"),
        Some("pause".to_owned())
    );
    assert_eq!(run_control::consume(&root, &run_id).expect("consume"), None);
    let con = rusqlite::Connection::open(root.join(".run-control.sqlite")).expect("control db");
    let state: String = con
        .query_row(
            "SELECT state FROM requests WHERE run_id=?1",
            rusqlite::params![run_id],
            |row| row.get(0),
        )
        .expect("request row");
    assert_eq!(state, "applied");
}

// (four parametrised rows: no run, partial state, abandoned pid, sitemap kind)
#[test]
fn control_rejects_stale_or_non_live_runs() {
    // No run at all.
    {
        let (_dir, root) = testkit::project("runctl", "no-run");
        assert!(
            request_err(&root, &"a".repeat(32), "stop").contains("active native crawl"),
            "missing run should be rejected"
        );
    }
    // Persisted run left `running` state.
    {
        let (_dir, root, run_id) = testkit::live_project("runctl", "partial");
        testkit::doctor_runs(&root, |doc| {
            doc["runs"][0]["state"] = serde_json::json!("partial");
        });
        assert!(
            request_err(&root, &run_id, "stop").contains("active native crawl"),
            "partial run should be rejected"
        );
    }
    // Controller pid abandoned: a spawned-and-reaped pid is reliably dead.
    {
        let (_dir, root, run_id) = testkit::live_project("runctl", "abandoned");
        let mut child = std::process::Command::new("/usr/bin/true")
            .spawn()
            .expect("spawn");
        child.wait().expect("wait");
        let dead = child.id() as i64;
        testkit::doctor_runs(&root, |doc| {
            doc["runs"][0]["pid"] = serde_json::json!(dead);
            doc["runs"][0]["controller_pid"] = serde_json::json!(dead);
            doc["runs"][0]["controller_start_identity"] = serde_json::json!("dead-pid");
        });
        let err = request_err(&root, &run_id, "stop");
        assert!(
            err.contains("active native crawl"),
            "abandoned run should be rejected, got: {err}"
        );
    }
    // Non-native source kind.
    {
        let (_dir, root) = testkit::project("runctl", "sitemap");
        let run = spiderhead_projects::run_observation::start(
            &root,
            "sitemap",
            "sitemap",
            200,
            20_000,
            0,
            None,
            "fingerprint",
            None,
            false,
            None,
            None,
            None,
            None,
        )
        .expect("sitemap run");
        let run_id = run["id"].as_str().expect("run id").to_owned();
        assert!(
            request_err(&root, &run_id, "stop").contains("active native crawl"),
            "sitemap run should be rejected"
        );
    }
}

#[test]
fn control_rejects_an_existing_pending_request() {
    let (_dir, root, run_id) = testkit::live_project("runctl", "pending");
    run_control::request(&root, &run_id, "pause").expect("request pause");
    assert!(
        request_err(&root, &run_id, "stop").contains("already pending"),
        "second request should be rejected"
    );
}

// The reference test monkeypatches `consume`; the port leaves a real persisted
// pending request for `consume` to find.
#[test]
fn controlled_reporter_turns_progress_boundary_into_checkpoint_interrupt() {
    let (_dir, root, run_id) = testkit::live_project("runctl", "reporter");
    run_control::request(&root, &run_id, "pause").expect("request pause");
    let mut reporter = run_control::ControlledRunReporter::new(&root, &run_id, None, None, None);
    let err = reporter.call(0, 1).expect_err("interrupt expected");
    assert_eq!(err.action, "pause");
    assert_eq!(reporter.control_action.as_deref(), Some("pause"));
}

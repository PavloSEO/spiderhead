//! Ported the reference suite cases that need no live fetch pipeline.
//!
//! The loopback fetch cases stay deferred: the reference `collect_once(apply=True)`
//! embeds the `fetch_one` + `ResponseCache` + `Throttle` pipeline, and no canonical
//! Rust crate exposes an equivalent `MonitorFetch` bridge yet.
//! `test_non_boolean_apply_is_rejected_before_http` is reference dynamic typing
//! (the Rust signature takes `bool`).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{json, Value};

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mon-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn project(dir: &Path, urls: &[&str]) -> (PathBuf, i64) {
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
    let configured = spiderhead_projects::monitoring::configure(
        &proj,
        &json!({
            "enabled": true,
            "urls": urls,
            "max_urls": urls.len() as i64,
            "max_requests": urls.len() as i64,
            "max_render_requests": 0,
            "full_refresh_every": 2,
        }),
        0,
    )
    .unwrap();
    (proj, configured["revision"].as_i64().unwrap())
}

fn monitor_doc(proj: &Path) -> Value {
    spiderhead_projects::runtime::read_document(proj, "monitor.json")
        .unwrap()
        .unwrap()
}

fn save_monitor_doc(proj: &Path, document: &Value, expected_revision: i64) -> i64 {
    spiderhead_projects::runtime::write_document(
        proj,
        "monitor.json",
        &document.clone(),
        Some(expected_revision),
    )
    .unwrap();
    monitor_doc(proj)["revision"].as_i64().unwrap()
}

#[test]
fn expired_or_dispatched_claim_never_replays_automatically() {
    let dir = tmp("expired");
    let (proj, revision) = project(&dir, &["https://example.test/a"]);
    let claimed =
        spiderhead_projects::monitoring::schedule(&proj, "start", revision, Some(2)).unwrap();
    // reference monkeypatches `_now`; the Rust seam writes the lease expiry directly.
    let mut document = monitor_doc(&proj);
    document["runner"]["lease_expires_at"] = json!("2026-10-06T00:00:02Z");
    document["revision"] = json!(document["revision"].as_i64().unwrap() + 1);
    let revision = save_monitor_doc(&proj, &document, claimed["revision"].as_i64().unwrap());
    eprintln!(
        "saved rev => {revision}; doc rev => {}; claimed => {}",
        monitor_doc(&proj)["revision"],
        claimed["revision"]
    );
    let err = spiderhead_projects::monitoring::collect_once(
        &proj,
        revision,
        true,
        Some(&|| "2026-10-06T00:00:03Z".to_owned()),
        None,
    )
    .unwrap_err();
    assert!(err.to_string().contains("lease expired"), "{err}");
    let interrupted = monitor_doc(&proj);
    assert_eq!(interrupted["runner"]["state"], "interrupted");

    let restarted = spiderhead_projects::monitoring::schedule(
        &proj,
        "start",
        interrupted["revision"].as_i64().unwrap(),
        None,
    )
    .unwrap();
    let mut document = monitor_doc(&proj);
    document["runner"]["dispatched_urls"] = json!(["https://example.test/a"]);
    document["revision"] = json!(document["revision"].as_i64().unwrap() + 1);
    let revision = save_monitor_doc(&proj, &document, restarted["revision"].as_i64().unwrap());
    let err = spiderhead_projects::monitoring::collect_once(&proj, revision, true, None, None)
        .unwrap_err();
    assert!(
        err.to_string().contains("interrupted after dispatch"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tampered_claim_plan_is_rejected_before_http() {
    let dir = tmp("tampered");
    let (proj, revision) = project(&dir, &["https://example.test/a"]);
    let claimed =
        spiderhead_projects::monitoring::schedule(&proj, "start", revision, None).unwrap();
    let mut document = monitor_doc(&proj);
    document["runner"]["claim_id"] = json!("monitor:../escape");
    document["runner"]["plan"]["planned_urls"] = json!(["https://outside.test/"]);
    document["revision"] = json!(document["revision"].as_i64().unwrap() + 1);
    let revision = save_monitor_doc(&proj, &document, claimed["revision"].as_i64().unwrap());
    eprintln!(
        "saved rev => {revision}; doc rev => {}; claimed => {}",
        monitor_doc(&proj)["revision"],
        claimed["revision"]
    );
    let err = spiderhead_projects::monitoring::collect_once(&proj, revision, true, None, None)
        .unwrap_err();
    assert!(err.to_string().contains("claim id is invalid"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

// (the lease-expiry half; the no-store half needs the live fetch pipeline)
#[test]
fn mid_run_lease_expiry_interrupts_between_dispatches() {
    let dir = tmp("mid-run");
    let (proj, revision) = project(&dir, &["https://example.test/a", "https://example.test/b"]);
    let claimed =
        spiderhead_projects::monitoring::schedule(&proj, "start", revision, Some(2)).unwrap();
    let mut document = monitor_doc(&proj);
    document["runner"]["lease_expires_at"] = json!("2026-10-06T00:00:02Z");
    document["revision"] = json!(document["revision"].as_i64().unwrap() + 1);
    let revision = save_monitor_doc(&proj, &document, claimed["revision"].as_i64().unwrap());
    let ticks: Mutex<std::vec::IntoIter<&str>> = Mutex::new(
        vec![
            "2026-10-06T00:00:00Z",
            "2026-10-06T00:00:00Z",
            "2026-10-06T00:00:03Z",
            "2026-10-06T00:00:03Z",
        ]
        .into_iter(),
    );
    let now = || {
        ticks
            .lock()
            .unwrap()
            .next()
            .unwrap_or("2026-10-06T00:00:03Z")
            .to_owned()
    };
    let mut fetch = |url: &str| -> spiderhead_projects::error::ProjectsResult<(
        Value,
        Option<Value>,
        Value,
        u64,
    )> {
        Ok((
            json!({
                "url": url,
                "status_code": 200,
                "content_type": "text/html",
                "cache_status": "miss",
                "error": "",
                "meta_robots": "",
                "x_robots": "",
                "outlinks": 0,
                "external_outlinks": 0,
            }),
            Some(json!({"_raw_html": "<html>body</html>", "links": []})),
            json!({"status_code": 200, "cache_control": "max-age=0"}),
            1,
        ))
    };
    let err = spiderhead_projects::monitoring::collect_once(
        &proj,
        revision,
        true,
        Some(&now),
        Some(&mut fetch),
    )
    .unwrap_err();
    assert!(err.to_string().contains("lease expired"), "{err}");
    assert_eq!(monitor_doc(&proj)["runner"]["state"], "interrupted");
    let _ = std::fs::remove_dir_all(&dir);
}

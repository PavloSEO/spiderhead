//! Ported tests from the reference suite — all eight tests are
//! portable (pure functions over JSON values).

use serde_json::{json, Map, Value};
use spiderhead_sf_import::recompute::{
    classify_trigger, needs_recompute, should_refresh, DEFAULT_REFRESH_INTERVAL_SECONDS,
    RECOMPUTE_EVENTS,
};

#[test]
fn every_declared_event_is_decided_and_loading_never_recomputes() {
    let events: std::collections::BTreeSet<&str> = RECOMPUTE_EVENTS.iter().copied().collect();
    assert_eq!(
        events,
        [
            "crawl_started",
            "crawl_finished",
            "segments_changed",
            "config_changed",
            "scan_loaded"
        ]
        .into_iter()
        .collect()
    );
    let (recompute, reason) = needs_recompute("scan_loaded").expect("scan_loaded is declared");
    assert!(!recompute);
    assert_eq!(
        reason,
        "saved audit counters are shown as persisted; loading never recomputes"
    );
    for trigger in events.iter().filter(|t| **t != "scan_loaded") {
        let (recompute, reason) = needs_recompute(trigger).expect("declared event");
        assert!(recompute);
        assert!(!reason.is_empty());
    }
}

#[test]
fn unknown_trigger_is_rejected() {
    let err = needs_recompute("observer_redraw").expect_err("unknown trigger must fail");
    assert!(err.0.contains("unknown recompute trigger"));
}

fn state(overrides: &[(&str, Value)]) -> Value {
    let mut m = Map::new();
    m.insert("lifecycle".into(), json!("finished"));
    m.insert("config_fingerprint".into(), json!("fp-1"));
    m.insert("evidence_revision".into(), json!(1));
    m.insert("segments".into(), json!([]));
    for (k, v) in overrides {
        m.insert((*k).to_owned(), v.clone());
    }
    Value::Object(m)
}

#[test]
fn state_transitions_map_onto_the_five_events() {
    assert_eq!(
        classify_trigger(None, &state(&[])).expect("classified"),
        "scan_loaded"
    );
    assert_eq!(
        classify_trigger(
            Some(&state(&[("lifecycle", json!("interrupted"))])),
            &state(&[("lifecycle", json!("running"))])
        )
        .expect("classified"),
        "crawl_started"
    );
    assert_eq!(
        classify_trigger(
            Some(&state(&[("lifecycle", json!("running"))])),
            &state(&[("lifecycle", json!("finished"))])
        )
        .expect("classified"),
        "crawl_finished"
    );
    assert_eq!(
        classify_trigger(
            Some(&state(&[("lifecycle", json!("running"))])),
            &state(&[("lifecycle", json!("failed"))])
        )
        .expect("classified"),
        "crawl_finished"
    );
    assert_eq!(
        classify_trigger(Some(&state(&[])), &state(&[("segments", json!(["blog"]))]))
            .expect("classified"),
        "segments_changed"
    );
    assert_eq!(
        classify_trigger(
            Some(&state(&[])),
            &state(&[("config_fingerprint", json!("fp-2"))])
        )
        .expect("classified"),
        "config_changed"
    );
    assert_eq!(
        classify_trigger(
            Some(&state(&[])),
            &state(&[("evidence_revision", json!(2))])
        )
        .expect("classified"),
        "config_changed"
    );
    assert_eq!(
        classify_trigger(Some(&state(&[])), &state(&[])).expect("classified"),
        "scan_loaded"
    );
}

#[test]
fn classification_validates_state_shapes() {
    let err = classify_trigger(Some(&state(&[])), &json!("running"))
        .expect_err("non-mapping current state must fail");
    assert!(err.0.contains("current scan state"));
    let err = classify_trigger(Some(&json!("finished")), &state(&[]))
        .expect_err("non-mapping previous state must fail");
    assert!(err.0.contains("previous scan state"));
}

#[test]
fn first_tick_refreshes_and_unchanged_revisions_never_do() {
    assert!(
        should_refresh(None, Some("rev-1"), None, DEFAULT_REFRESH_INTERVAL_SECONDS)
            .expect("refresh")
    );
    let refreshed = json!({"at": 100.0, "revision": "rev-1"});
    assert!(!should_refresh(
        Some(&refreshed),
        Some("rev-1"),
        Some(100.0 + DEFAULT_REFRESH_INTERVAL_SECONDS),
        DEFAULT_REFRESH_INTERVAL_SECONDS
    )
    .expect("refresh"));
    assert!(!should_refresh(
        Some(&refreshed),
        Some("rev-1"),
        Some(10_000.0),
        DEFAULT_REFRESH_INTERVAL_SECONDS
    )
    .expect("refresh"));
}

#[test]
fn changed_revision_refreshes_at_the_next_tick_boundary() {
    let refreshed = json!({"at": 100.0, "revision": "rev-1"});
    let interval = DEFAULT_REFRESH_INTERVAL_SECONDS;
    assert!(!should_refresh(
        Some(&refreshed),
        Some("rev-2"),
        Some(100.0 + interval / 2.0),
        interval
    )
    .expect("refresh"));
    assert!(should_refresh(
        Some(&refreshed),
        Some("rev-2"),
        Some(100.0 + interval),
        interval
    )
    .expect("refresh"));
    assert!(
        should_refresh(Some(&refreshed), None, Some(100.0 + interval), interval).expect("refresh")
    );
    let no_revision = json!({"at": 100.0, "revision": null});
    assert!(should_refresh(
        Some(&no_revision),
        Some("rev-2"),
        Some(100.0 + interval),
        interval
    )
    .expect("refresh"));
}

#[test]
fn zero_interval_refreshes_on_every_changed_tick() {
    let refreshed = json!({"at": 100.0, "revision": "rev-1"});
    assert!(!should_refresh(Some(&refreshed), Some("rev-1"), Some(101.0), 0.0).expect("refresh"));
    assert!(should_refresh(Some(&refreshed), Some("rev-2"), Some(100.0), 0.0).expect("refresh"));
}

#[test]
fn tick_inputs_are_validated() {
    let err = should_refresh(None, Some("rev-1"), None, -1.0).expect_err("negative interval");
    assert!(err.0.contains("refresh interval"));
    let err = should_refresh(None, Some("rev-1"), None, f64::NAN).expect_err("nan interval");
    assert!(err.0.contains("refresh interval"));
    let err = should_refresh(
        Some(&json!({"revision": "rev-1"})),
        Some("rev-1"),
        Some(1.0),
        DEFAULT_REFRESH_INTERVAL_SECONDS,
    )
    .expect_err("missing 'at'");
    assert!(err.0.contains("'at' timestamp"));
    let err = should_refresh(
        Some(&json!({"at": 1.0, "revision": "rev-1"})),
        Some("rev-1"),
        Some(f64::INFINITY),
        DEFAULT_REFRESH_INTERVAL_SECONDS,
    )
    .expect_err("infinite now");
    assert!(err.0.contains("monotonic timestamp"));
}

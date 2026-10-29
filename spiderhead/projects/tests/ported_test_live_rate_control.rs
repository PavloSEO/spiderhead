//! rate requests and the `ControlledRunReporter` pacer wiring.
//!
//! The `Throttle`/`crawl_to_scan` cases exercise the crawl collector, and the
//! CLI/MCP routing cases exercise the cli/mcp crates — all owned by other
//! slices. Here, the reference `_live_project` monkeypatch is replaced by a real
//! project + running scan fixture + persisted native run.

#[path = "common/testkit.rs"]
mod testkit;

use std::sync::{Arc, Mutex};

use spiderhead_projects::run_control;

#[test]
fn rate_request_is_consumed_once_and_a_newer_one_replaces_it() {
    let (_dir, root, run_id) = testkit::live_project("ratectl", "replace");
    let requested = run_control::request_rate(&root, &run_id, 2.0, None).expect("rate 2/s");
    assert_eq!(requested["state"], "rate_requested");
    run_control::request_rate(&root, &run_id, 10.0, Some(3)).expect("rate 10/s");
    assert_eq!(
        run_control::consume_rate(&root, &run_id).expect("consume_rate"),
        Some((0.1, Some(3)))
    );
    assert_eq!(
        run_control::consume_rate(&root, &run_id).expect("consume_rate"),
        None
    );
    // A rate change does not use up the one-shot pause/stop request.
    let pause = run_control::request(&root, &run_id, "pause").expect("request pause");
    assert_eq!(pause["state"], "pause_requested");
}

// (the `(True, None)` parametrisation is the reference bool-is-int typing — a
// `bool` cannot reach `request_rate`'s `f64` signature in Rust)
#[test]
fn rate_request_rejects_unsafe_values() {
    let (_dir, root, run_id) = testkit::live_project("ratectl", "unsafe");
    for (rate, concurrency) in [
        (0.0, None),
        (-1.0, None),
        (51.0, None),
        (5.0, Some(0)),
        (5.0, Some(33)),
    ] {
        assert!(
            run_control::request_rate(&root, &run_id, rate, concurrency).is_err(),
            "rate={rate} concurrency={concurrency:?} should be rejected"
        );
    }
}

#[test]
fn controlled_reporter_hands_the_rate_to_the_crawl_and_the_shared_pacer() {
    struct TestPacer(Arc<Mutex<Option<f64>>>);
    impl run_control::OriginPacer for TestPacer {
        fn set_min_delay(&mut self, min_delay_seconds: f64) {
            *self.0.lock().expect("pacer lock") = Some(min_delay_seconds);
        }
    }

    let (_dir, root, run_id) = testkit::live_project("ratectl", "pacer");
    let delay = Arc::new(Mutex::new(None));
    let mut reporter = run_control::ControlledRunReporter::new(
        &root,
        &run_id,
        None,
        Some(Box::new(TestPacer(Arc::clone(&delay)))),
        Some((0.25, None)),
    );
    assert_eq!(reporter.pending_rate(), Some((0.25, None)));
    run_control::request_rate(&root, &run_id, 10.0, None).expect("rate 10/s");
    assert_eq!(reporter.pending_rate(), Some((0.1, None)));
    assert_eq!(reporter.pending_rate(), None);
    assert_eq!(*delay.lock().expect("pacer lock"), Some(0.1));
}

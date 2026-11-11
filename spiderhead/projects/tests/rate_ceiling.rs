//! The per-host pacing of project crawls follows the operator's configured rate.

#[path = "common/testkit.rs"]
mod testkit;

use spiderhead_projects::origin_pacing::{paced_rate, ProjectOriginPacer};

#[test]
fn the_configured_rate_stands_for_any_host() {
    assert_eq!(paced_rate(10.0), 10.0);
    assert_eq!(paced_rate(0.5), 0.5);
    assert_eq!(paced_rate(200.0), 200.0);
    // A zero delay asks for no pacing at all.
    assert_eq!(paced_rate(f64::INFINITY), 0.0);
}

#[test]
fn a_live_rate_change_takes_effect_as_given() {
    let dir = testkit::scratch("pacing", "ceiling");
    std::env::set_var(
        "SPIDERHEAD_CONFIG_DIR",
        dir.0.join("state").join("spiderhead"),
    );
    let mut pacer = ProjectOriginPacer::new(None, "https://example.com/", 0.5, 2.0).expect("pacer");
    pacer.set_min_delay(0.1);
    assert!((pacer.max_requests_per_second() - 10.0).abs() < 1e-9);
    pacer.set_min_delay(0.0);
    assert_eq!(pacer.max_requests_per_second(), 0.0);
}

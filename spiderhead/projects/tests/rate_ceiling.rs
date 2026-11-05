//! The per-host rate ceiling of project crawls: held for hosts nobody
//! confirmed, lifted for hosts the operator owns.

#[path = "common/testkit.rs"]
mod testkit;

use spiderhead_projects::origin_pacing::{
    project_rate_ceiling, ProjectOriginPacer, PUBLIC_PROJECT_MAX_REQUESTS_PER_SECOND as PUBLIC,
};

#[test]
fn an_unconfirmed_host_is_held_at_the_public_ceiling() {
    assert_eq!(project_rate_ceiling(10.0, false), PUBLIC);
    assert_eq!(project_rate_ceiling(f64::INFINITY, false), PUBLIC);
    assert_eq!(project_rate_ceiling(0.5, false), PUBLIC);
    assert_eq!(project_rate_ceiling(PUBLIC, false), PUBLIC);
}

#[test]
fn a_confirmed_host_keeps_the_requested_rate() {
    assert_eq!(project_rate_ceiling(10.0, true), 10.0);
    // An unbounded request means unpaced, which only a confirmed host gets.
    assert_eq!(project_rate_ceiling(f64::INFINITY, true), 0.0);
    assert_eq!(project_rate_ceiling(1.0, true), PUBLIC);
}

#[test]
fn a_live_rate_change_follows_the_ownership_rule() {
    let dir = testkit::scratch("pacing", "ceiling");
    std::env::set_var(
        "SPIDERHEAD_CONFIG_DIR",
        dir.0.join("state").join("spiderhead"),
    );
    let mut pacer =
        ProjectOriginPacer::new(None, "https://example.com/", 0.5, PUBLIC).expect("pacer");
    pacer.set_min_delay(0.1);
    assert_eq!(pacer.max_requests_per_second(), PUBLIC);
    pacer.set_owned_host(true);
    pacer.set_min_delay(0.1);
    assert!((pacer.max_requests_per_second() - 10.0).abs() < 1e-9);
    pacer.set_min_delay(0.0);
    assert_eq!(pacer.max_requests_per_second(), 0.0);
}

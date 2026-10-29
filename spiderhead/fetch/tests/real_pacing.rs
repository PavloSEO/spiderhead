//! The one pacing test that runs on real time: throttling must actually wait.

use std::time::{Duration, Instant};

use spiderhead_fetch::{clock, throttle::Throttle};

#[tokio::test]
async fn real_clock_makes_the_second_dispatch_wait_for_the_delay() {
    clock::set_virtual(false);
    let mut throttle = Throttle::new(0.3, 60.0, 1);
    let started = Instant::now();
    throttle.wait().await;
    throttle.wait().await;
    assert!(
        started.elapsed() >= Duration::from_millis(290),
        "second dispatch returned after {:?}",
        started.elapsed()
    );
    assert_eq!(clock::virtual_slept().1, 0);
}

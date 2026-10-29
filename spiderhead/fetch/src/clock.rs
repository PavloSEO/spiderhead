//! Process-wide pacing clock: real time in production, virtual time in tests.
//!
//! Virtual mode (`SPIDERHEAD_VIRTUAL_TIME=1` or [`set_virtual`]) never sleeps: a
//! requested delay is recorded and added to a clock offset, so elapsed-time
//! checks and dispatch spacing behave as if the time had really passed.

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::time::{Duration, Instant};

const UNSET: u8 = 0;
const REAL: u8 = 1;
const VIRTUAL: u8 = 2;

static MODE: AtomicU8 = AtomicU8::new(UNSET);
static OFFSET_NANOS: AtomicU64 = AtomicU64::new(0);
static SLEEP_NANOS: AtomicU64 = AtomicU64::new(0);
static SLEEP_COUNT: AtomicU64 = AtomicU64::new(0);

/// Force virtual (`true`) or real (`false`) time for this process.
pub fn set_virtual(on: bool) {
    MODE.store(if on { VIRTUAL } else { REAL }, Ordering::SeqCst);
}

fn is_virtual() -> bool {
    match MODE.load(Ordering::Relaxed) {
        UNSET => {
            let on = std::env::var("SPIDERHEAD_VIRTUAL_TIME").is_ok_and(|v| v == "1");
            set_virtual(on);
            on
        }
        mode => mode == VIRTUAL,
    }
}

/// The current instant, shifted by all virtually slept time.
pub fn now() -> Instant {
    Instant::now() + Duration::from_nanos(OFFSET_NANOS.load(Ordering::SeqCst))
}

/// Wait for `duration`; in virtual mode record it and advance the clock instead.
pub async fn sleep(duration: Duration) {
    if is_virtual() {
        let nanos = u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX);
        OFFSET_NANOS.fetch_add(nanos, Ordering::SeqCst);
        SLEEP_NANOS.fetch_add(nanos, Ordering::SeqCst);
        SLEEP_COUNT.fetch_add(1, Ordering::SeqCst);
        tokio::task::yield_now().await;
    } else {
        tokio::time::sleep(duration).await;
    }
}

/// Total delay requested through [`sleep`] in virtual mode, and how many calls made it.
pub fn virtual_slept() -> (Duration, u64) {
    (
        Duration::from_nanos(SLEEP_NANOS.load(Ordering::SeqCst)),
        SLEEP_COUNT.load(Ordering::SeqCst),
    )
}

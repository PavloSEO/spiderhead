//! Per-origin pacing and the adaptive delay and concurrency controller.
//! One shared dispatch clock: `start_at = max(now, last_at + delay)`, with no jitter.
//! Delay halves toward the observed latency; timeouts, 429 and 5xx add penalties; failure streaks stop the crawl.

use std::time::{Duration, Instant};

/// Delay multiplier applied after a timeout or a 429 response.
const TIMEOUT_PENALTY: f64 = 4.0;
/// Consecutive 2xx responses after which concurrency widens by one.
const WIDEN_AFTER_CONSECUTIVE_OK: u32 = 3;
/// Consecutive timeouts or server errors that stop the crawl.
const STOP_AFTER_STREAK: u32 = 5;

/// Outcome class of one response, as the throttle consumes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// HTTP 2xx: widens concurrency after enough consecutive successes.
    Ok,
    /// Transport timeout: the delay is multiplied by the timeout penalty.
    Timeout,
    /// HTTP 429: the overload signal, quadruple penalty.
    RateLimited,
    /// HTTP 5xx: doubles the delay.
    ServerError,
    /// Any other non-2xx (4xx without 429): no penalty, no widening.
    ClientError,
}

/// The shared dispatch clock + adaptive state for one origin.
pub struct Throttle {
    min_delay: f64,
    max_delay: f64,
    delay: f64,
    concurrency: u32,
    max_concurrency: u32,
    consecutive_ok: u32,
    consecutive_timeouts: u32,
    consecutive_server_errors: u32,
    requests_used: u64,
    last_dispatch: Option<Instant>,
    started: Instant,
    elapsed_offset: f64,
}

impl Throttle {
    /// `min_delay`/`max_delay`/`concurrency` come from `speed.*`; the clock starts now.
    pub fn new(min_delay: f64, max_delay: f64, concurrency: u32) -> Self {
        let concurrency = concurrency.max(1);
        Throttle {
            min_delay,
            max_delay,
            delay: min_delay,
            concurrency: 1,
            max_concurrency: concurrency,
            consecutive_ok: 0,
            consecutive_timeouts: 0,
            consecutive_server_errors: 0,
            requests_used: 0,
            last_dispatch: None,
            started: crate::clock::now(),
            elapsed_offset: 0.0,
        }
    }

    /// An operator's live politeness change: the delay starts at the new floor
    /// and adaptive widening still applies afterwards when the origin struggles.
    pub fn set_rate(&mut self, min_delay: f64, concurrency: Option<u32>) {
        self.min_delay = min_delay;
        self.delay = min_delay;
        if let Some(concurrency) = concurrency {
            self.max_concurrency = concurrency.max(1);
            self.concurrency = self.concurrency.min(self.max_concurrency);
        }
    }

    /// The robots-derived floor: raising `min_delay` also raises `max_delay`, so
    /// a site asking for slow crawling may exceed the configured ceiling.
    pub fn apply_robots_floor(&mut self, delay: f64) {
        if delay > self.min_delay {
            self.min_delay = delay;
        }
        if self.max_delay < self.min_delay {
            self.max_delay = self.min_delay;
        }
        if self.delay < self.min_delay {
            self.delay = self.min_delay;
        }
    }

    /// Reserve the next dispatch slot: sleep until `last_at + delay`, then take it.
    pub async fn wait(&mut self) {
        let now = crate::clock::now();
        let start_at = match self.last_dispatch {
            Some(last) => {
                let earliest = last + Duration::from_secs_f64(self.delay);
                if earliest > now {
                    earliest
                } else {
                    now
                }
            }
            None => now,
        };
        if start_at > now {
            crate::clock::sleep(start_at - now).await;
        }
        let dispatched = crate::clock::now();
        self.last_dispatch = Some(dispatched);
        self.requests_used += 1;
    }

    /// Feed one response outcome back into the controller.
    pub fn record(&mut self, outcome: Outcome, latency_seconds: f64, retry_after: Option<f64>) {
        match outcome {
            Outcome::Ok => {
                self.consecutive_timeouts = 0;
                self.consecutive_server_errors = 0;
                self.consecutive_ok += 1;
                let candidate = (self.delay + latency_seconds / self.concurrency as f64) / 2.0;
                self.delay = candidate.clamp(self.min_delay, self.max_delay);
                // The streak only resets when concurrency actually widens;
                // at the configured ceiling the live count keeps growing and
                // the persisted state clamps it (`min(live, WIDEN-1)`).
                if self.consecutive_ok >= WIDEN_AFTER_CONSECUTIVE_OK
                    && self.concurrency < self.max_concurrency
                {
                    self.concurrency += 1;
                    self.consecutive_ok = 0;
                }
            }
            Outcome::Timeout => {
                self.consecutive_ok = 0;
                self.consecutive_timeouts += 1;
                self.concurrency = 1;
                let base = self.delay.max(self.min_delay).max(0.5);
                self.delay = (base * TIMEOUT_PENALTY).min(self.max_delay);
            }
            Outcome::RateLimited | Outcome::ServerError => {
                self.consecutive_ok = 0;
                self.consecutive_server_errors += 1;
                self.concurrency = 1;
                let base = self.delay.max(self.min_delay).max(0.5);
                let penalty = if outcome == Outcome::RateLimited {
                    TIMEOUT_PENALTY
                } else {
                    2.0
                };
                let mut widened = base * penalty;
                if let Some(seconds) = retry_after {
                    widened = widened.max(seconds);
                }
                self.delay = widened.min(self.max_delay);
            }
            Outcome::ClientError => {
                self.consecutive_ok = 0;
            }
        }
    }

    /// Five consecutive timeouts stop the crawl (`finish_reason="errors"`).
    pub fn should_stop_timeouts(&self) -> bool {
        self.consecutive_timeouts >= STOP_AFTER_STREAK
    }

    /// Five consecutive 429/5xx stop the crawl as well.
    pub fn should_stop_server_errors(&self) -> bool {
        self.consecutive_server_errors >= STOP_AFTER_STREAK
    }

    /// The adaptive delay, as stored in `throttle_state_json`.
    pub fn delay_seconds(&self) -> f64 {
        self.delay
    }

    /// The configured politeness floor (config default, robots floor or the
    /// operator's last rate change).
    pub fn min_delay_seconds(&self) -> f64 {
        self.min_delay
    }

    /// The concurrency ceiling (the live count may be lower while it widens).
    pub fn max_concurrency(&self) -> u32 {
        self.max_concurrency
    }

    /// Current number of in-flight requests allowed for this origin.
    pub fn concurrency(&self) -> u32 {
        self.concurrency
    }

    /// Consecutive 2xx responses since the last widening or penalty.
    pub fn consecutive_ok(&self) -> u32 {
        self.consecutive_ok
    }

    /// Number of dispatches taken from the clock so far.
    pub fn requests_used(&self) -> u64 {
        self.requests_used
    }

    /// Consecutive timeouts since the last successful response.
    pub fn timeout_streak(&self) -> u32 {
        self.consecutive_timeouts
    }

    /// Consecutive 429 or 5xx responses since the last successful response.
    pub fn server_error_streak(&self) -> u32 {
        self.consecutive_server_errors
    }

    /// `elapsed_seconds` of the run so far, rounded to milliseconds.
    pub fn elapsed_seconds(&self) -> f64 {
        let live = (crate::clock::now()
            .saturating_duration_since(self.started)
            .as_secs_f64()
            * 1000.0)
            .round()
            / 1000.0;
        (self.elapsed_offset + live).max(0.0)
    }

    /// Restore a persisted throttle state on resume (`scan_throttle.v2`):
    /// delay, concurrency, consecutive-ok streak, the shared request count and
    /// the elapsed-time baseline, so nothing regresses.
    pub fn restore(
        &mut self,
        delay_seconds: f64,
        concurrency: u32,
        consecutive_ok: u32,
        requests_used: u64,
        elapsed_seconds: f64,
    ) {
        self.delay = delay_seconds.clamp(self.min_delay, self.max_delay);
        self.concurrency = concurrency.clamp(1, self.max_concurrency);
        self.consecutive_ok = consecutive_ok;
        self.requests_used = requests_used;
        self.elapsed_offset = elapsed_seconds;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn first_dispatch_is_immediate_and_counts() {
        let mut throttle = Throttle::new(0.0, 60.0, 1);
        throttle.wait().await;
        assert_eq!(throttle.requests_used(), 1);
    }

    #[test]
    fn virtual_clock_records_the_delay_without_sleeping() {
        crate::clock::set_virtual(true);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("rt");
        let started = std::time::Instant::now();
        let mut throttle = Throttle::new(30.0, 60.0, 1);
        runtime.block_on(async {
            throttle.wait().await;
            throttle.wait().await;
        });
        assert!(
            started.elapsed().as_secs_f64() < 5.0,
            "virtual pacing must not sleep"
        );
        let (slept, calls) = crate::clock::virtual_slept();
        assert!(
            calls >= 1 && slept.as_secs_f64() >= 29.0,
            "{slept:?} in {calls}"
        );
        assert!(throttle.elapsed_seconds() >= 29.0);
    }

    #[test]
    fn adaptive_delay_halves_toward_latency_and_never_below_min() {
        let mut throttle = Throttle::new(0.02, 60.0, 1);
        throttle.record(Outcome::Ok, 0.001, None);
        assert!(throttle.delay_seconds() >= 0.02);
        assert_eq!(throttle.consecutive_ok(), 1);
    }

    #[test]
    fn timeout_quadruples_the_delay_with_half_second_floor() {
        let mut throttle = Throttle::new(0.02, 60.0, 1);
        throttle.record(Outcome::Timeout, 0.0, None);
        assert_eq!(throttle.delay_seconds(), 2.0);
        assert_eq!(throttle.concurrency(), 1);
        assert_eq!(throttle.timeout_streak(), 1);
    }

    #[test]
    fn server_error_doubles_and_streak_stops_at_five() {
        let mut throttle = Throttle::new(0.02, 60.0, 1);
        for _ in 0..5 {
            throttle.record(Outcome::ServerError, 0.0, None);
        }
        assert!(throttle.should_stop_server_errors());
        assert_eq!(throttle.server_error_streak(), 5);
    }

    #[test]
    fn robots_floor_raises_min_and_current_delay() {
        let mut throttle = Throttle::new(0.02, 60.0, 1);
        throttle.apply_robots_floor(1.0);
        assert_eq!(throttle.delay_seconds(), 1.0);
    }

    #[test]
    fn operator_set_rate_resets_delay_and_moves_concurrency() {
        let mut throttle = Throttle::new(0.02, 60.0, 4);
        throttle.record(Outcome::Ok, 0.001, None);
        throttle.record(Outcome::Ok, 0.001, None);
        throttle.record(Outcome::Ok, 0.001, None); // widened to 2
        throttle.set_rate(0.5, Some(1));
        assert_eq!(throttle.delay_seconds(), 0.5);
        assert_eq!(throttle.concurrency(), 1);
        // Ceiling raised again: the next widening starts from the clamped live count.
        throttle.set_rate(0.1, Some(4));
        assert_eq!(throttle.delay_seconds(), 0.1);
        // Without a concurrency argument the ceiling stays where it was.
        throttle.set_rate(0.3, None);
        assert_eq!(throttle.delay_seconds(), 0.3);
    }

    #[test]
    fn set_rate_moves_the_floor_both_ways_and_caps_concurrency() {
        let mut throttle = Throttle::new(0.5, 10.0, 4);
        throttle.restore(3.0, 4, 0, 0, 0.0);
        throttle.set_rate(0.1, Some(2));
        assert_eq!(throttle.delay_seconds(), 0.1);
        assert!(throttle.concurrency() <= 2);
        throttle.set_rate(2.0, None);
        assert_eq!(throttle.delay_seconds(), 2.0);
        assert!(throttle.concurrency() <= 2);
    }

    #[test]
    fn restore_after_a_live_rate_change_returns_to_configured_bounds() {
        let mut throttle = Throttle::new(0.5, 10.0, 2);
        throttle.restore(0.1, 8, 0, 0, 0.0);
        assert_eq!(throttle.delay_seconds(), 0.5);
        assert_eq!(throttle.concurrency(), 2);
    }
}

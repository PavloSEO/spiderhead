//! Live recomputation policy for audit statistics (issue #1511).
//!
//! severity and per-check splits, check coverage) are expensive to rebuild and
//! must not be recomputed on a whim. This module owns the single decision of
//! when a recomputation is required and when saved numbers must be shown as
//! persisted:
//!
//! - `crawl_started`, `crawl_finished`, `segments_changed` and
//!   `config_changed` all make the previously projected statistics stale.
//! - `scan_loaded` never does: opening a completed crawl from disk shows the
//!   saved audit's own counters, it does not derive new ones.
//!
//! The periodic visible-aggregate refresh is a consumer-side concern. The core
//! owns no timers and no threads; a consumer asks [`should_refresh`] with the
//! revision it last refreshed at and the revision it sees now.

use serde_json::Value;
use thiserror::Error;

/// The five policy events.
pub const RECOMPUTE_EVENTS: [&str; 5] = [
    "crawl_started",
    "crawl_finished",
    "segments_changed",
    "config_changed",
    "scan_loaded",
];

/// The default refresh interval, in seconds.
pub const DEFAULT_REFRESH_INTERVAL_SECONDS: f64 = 2.0;

const TERMINAL_LIFECYCLES: [&str; 3] = ["finished", "failed", "interrupted"];

/// Policy validation failure (`ValueError` in the oracle).
#[derive(Debug, Error)]
#[error("{0}")]
pub struct PolicyError(pub String);

fn recompute_reason(trigger: &str) -> &'static str {
    match trigger {
        "crawl_started" => "a started crawl replaces the previously projected statistics",
        "crawl_finished" => "a finished crawl finalizes the audit evidence and its counters",
        "segments_changed" => "the declared segment scope changed, so segment aggregates are stale",
        "config_changed" => {
            "the configuration changed, so prior aggregates describe other settings"
        }
        _ => "saved audit counters are shown as persisted; loading never recomputes",
    }
}

/// `needs_recompute`: whether audit statistics must be recomputed for one
/// trigger event. Returns the decision and its reason, so a projection can
/// state why its numbers were rebuilt or why the saved values were shown
/// unchanged.
pub fn needs_recompute(trigger: &str) -> Result<(bool, &'static str), PolicyError> {
    if !RECOMPUTE_EVENTS.contains(&trigger) {
        return Err(PolicyError(format!(
            "unknown recompute trigger: {}",
            crate::pyvalue::py_repr_value(&Value::from(trigger))
        )));
    }
    Ok((trigger != "scan_loaded", recompute_reason(trigger)))
}

/// `classify_trigger`: map a retained-scan state transition onto one recompute
/// event name. Both states are plain objects; recognised keys are `lifecycle`,
/// `segments`, `config_fingerprint` and `evidence_revision`. A missing
/// previous state is an initial load, never a transition onto recomputation:
/// the first read of an unchanged artifact is how saved counters materialise.
pub fn classify_trigger(
    before: Option<&Value>,
    after: &Value,
) -> Result<&'static str, PolicyError> {
    let Some(after_map) = after.as_object() else {
        return Err(PolicyError(
            "the current scan state must be a mapping".to_owned(),
        ));
    };
    let Some(before) = before else {
        return Ok("scan_loaded");
    };
    let Some(before_map) = before.as_object() else {
        return Err(PolicyError(
            "the previous scan state must be a mapping or None".to_owned(),
        ));
    };
    let lifecycle = after_map.get("lifecycle");
    let running = |v: Option<&Value>| v.and_then(Value::as_str) == Some("running");
    if running(lifecycle) && !running(before_map.get("lifecycle")) {
        return Ok("crawl_started");
    }
    if running(before_map.get("lifecycle"))
        && lifecycle
            .and_then(Value::as_str)
            .is_some_and(|l| TERMINAL_LIFECYCLES.contains(&l))
    {
        return Ok("crawl_finished");
    }
    let segments = |m: &serde_json::Map<String, Value>| -> Value {
        m.get("segments")
            .filter(|v| crate::pyvalue::truthy(v))
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new()))
    };
    if !crate::pyvalue::py_eq(&segments(before_map), &segments(after_map)) {
        return Ok("segments_changed");
    }
    if !crate::pyvalue::py_eq(
        map_get(before_map, "config_fingerprint"),
        map_get(after_map, "config_fingerprint"),
    ) || !crate::pyvalue::py_eq(
        map_get(before_map, "evidence_revision"),
        map_get(after_map, "evidence_revision"),
    ) {
        return Ok("config_changed");
    }
    Ok("scan_loaded")
}

/// `dict.get(k)` with `Null` for a missing key.
fn map_get<'a>(m: &'a serde_json::Map<String, Value>, k: &str) -> &'a Value {
    static NULL: Value = Value::Null;
    m.get(k).unwrap_or(&NULL)
}

/// `should_refresh`: consumer-side tick gate for visible aggregates.
///
/// The tick lives with the consumer (a redraw loop, an MCP poller): the core
/// owns no timers and no threads. After each refresh the consumer records
/// `{"at": <monotonic timestamp>, "revision": <revision it projected>}` and
/// asks this function on every tick. A refresh is due only when the observed
/// revision differs from the refreshed one and at least `interval` seconds
/// passed since that refresh, so an unchanged scan never triggers work and a
/// changed scan is picked up at the next tick boundary. A revision that is
/// unavailable on either side counts as changed and refreshes conservatively.
pub fn should_refresh(
    last_refresh: Option<&Value>,
    data_revision: Option<&str>,
    now: Option<f64>,
    interval: f64,
) -> Result<bool, PolicyError> {
    if !interval.is_finite() || interval < 0.0 {
        return Err(PolicyError(
            "refresh interval must be a non-negative finite number of seconds".to_owned(),
        ));
    }
    let Some(last_refresh) = last_refresh else {
        return Ok(true);
    };
    let Some(map) = last_refresh.as_object() else {
        return Err(PolicyError(
            "last_refresh must be a mapping with an 'at' timestamp and a 'revision'".to_owned(),
        ));
    };
    let stamped_at = map.get("at");
    let numeric = stamped_at.is_some_and(|v| matches!(v, Value::Number(_)));
    if !numeric {
        return Err(PolicyError(
            "last_refresh must record a numeric monotonic 'at' timestamp".to_owned(),
        ));
    }
    let now = match now {
        Some(n) => n,
        None => monotonic_now(),
    };
    if !now.is_finite() {
        return Err(PolicyError(
            "now must be a finite monotonic timestamp".to_owned(),
        ));
    }
    let refreshed_revision = map.get("revision");
    if let (Some(data_rev), Some(Value::String(refreshed))) = (data_revision, refreshed_revision) {
        if data_rev == refreshed.as_str() {
            return Ok(false);
        }
    }
    let at = stamped_at.and_then(Value::as_f64).unwrap_or(0.0);
    Ok(now - at >= interval)
}

/// `time.monotonic()` seconds — an epoch-less, monotonically increasing float.
fn monotonic_now() -> f64 {
    static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    EPOCH
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs_f64()
}

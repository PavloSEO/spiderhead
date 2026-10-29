//! Durable, read-only-observable local collector attempts.
//!
//! A project-side operation log, not a crawler or a scheduler: a collector records its own
//! lifecycle around existing native or export calls; the observer reads the document and
//! checks whether the recorded local PID is still alive.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::error::{ProjectsError, ProjectsResult};
use crate::io::{load_document, save_revision};
use crate::pytime;

mod finish_sitemap;
mod load;
pub use finish_sitemap::*;
pub use load::*;

/// Document format.
pub const FORMAT: &str = "spiderhead.run-observation.v1";
/// Document name under the project root.
pub const NAME: &str = "run-observation.json";
/// Retained runs ceiling.
pub const MAX_RUNS: usize = 100;
/// Events per run ceiling.
pub const MAX_EVENTS_PER_RUN: usize = 200;
/// Event message bound.
pub const MAX_EVENT_MESSAGE: usize = 240;
/// Sample staleness threshold.
pub const STALE_SAMPLE_SECONDS: f64 = 5.0;
/// Rate window length.
pub const RATE_WINDOW_SECONDS: f64 = 5.0;
/// Optimistic-write attempts.
pub const WRITE_ATTEMPTS: usize = 20;

const KINDS: &[&str] = &["native", "screaming_frog", "sitemap"];
const MODES: &[&str] = &["spider", "list", "sf_live", "sf_exports", "sitemap"];
const STATES: &[&str] = &["running", "finished", "partial", "failed", "cancelled"];
const PHASES: &[&str] = &[
    "admission",
    "collection",
    "render",
    "external",
    "analysis",
    "finalizing",
];

fn text(value: &Value, name: &str, maximum: usize) -> ProjectsResult<String> {
    match value.as_str() {
        Some(s) if !s.is_empty() && s.len() <= maximum => Ok(s.to_owned()),
        _ => Err(crate::error::value(format!(
            "{name} must be nonempty text of at most {maximum} characters"
        ))),
    }
}

fn counter(value: &Value, name: &str) -> ProjectsResult<i64> {
    match value.as_i64() {
        Some(v) if !value.is_boolean() && v >= 0 => Ok(v),
        _ => Err(crate::error::value(format!(
            "{name} must be a nonnegative integer"
        ))),
    }
}

fn optional_counter(value: &Value, name: &str) -> ProjectsResult<Value> {
    if value.is_null() {
        Ok(Value::Null)
    } else {
        Ok(json!(counter(value, name)?))
    }
}

fn rate_ok(value: &Value) -> bool {
    value.is_null() || value.as_f64().is_some_and(|v| v.is_finite() && v >= 0.0)
}

/// Text form of a counter JSON scalar in journal entries (`None`, `2`).
fn py_counter(value: &Value) -> String {
    match value {
        Value::Null => "None".to_owned(),
        other => other.to_string(),
    }
}

/// Canonicalize the deepest existing ancestor
/// (which resolves symlinks) and append the missing tail lexically.
fn resolve_nonstrict(path: &Path) -> PathBuf {
    let mut missing = Vec::new();
    let mut cursor = path.to_path_buf();
    loop {
        match cursor.canonicalize() {
            Ok(resolved) => {
                let mut out = resolved;
                for component in missing.iter().rev() {
                    out.push(component);
                }
                return out;
            }
            Err(_) => match cursor.file_name() {
                Some(name) => {
                    missing.push(name.to_os_string());
                    cursor.pop();
                }
                None => return cursor,
            },
        }
    }
}

fn relative_artifact(root: &Path, artifact: Option<&str>) -> ProjectsResult<Value> {
    let Some(artifact) = artifact else {
        return Ok(Value::Null);
    };
    let path = Path::new(artifact);
    let relative = root
        .canonicalize()
        .ok()
        .and_then(|root| {
            resolve_nonstrict(path)
                .strip_prefix(&root)
                .ok()
                .map(|r| r.to_path_buf())
        })
        .ok_or_else(|| {
            crate::error::value("collector artifact must be inside the project workspace")
        })?;
    let first = relative
        .components()
        .next()
        .map(|c| c.as_os_str())
        .unwrap_or_default();
    if relative.components().count() == 0 || (first != "scans" && first != "reports") {
        return Err(crate::error::value(
            "collector artifact must be under scans/ or reports/",
        ));
    }
    Ok(json!(relative.to_string_lossy()))
}

fn empty(project_uuid: &str) -> Value {
    json!({"format": FORMAT, "revision": 0, "project_uuid": project_uuid, "runs": []})
}

/// A platform process-start identity, when the host exposes one.
fn process_identity(pid: i64) -> Option<String> {
    let output = std::process::Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Cheap existence check for a recorded process id.
pub(crate) fn pid_exists(pid: i32) -> bool {
    pid_alive(i64::from(pid)).is_ok()
}

/// Existence probe for a recorded process id.
#[cfg(unix)]
fn pid_alive(pid: i64) -> Result<(), &'static str> {
    // SAFETY: signal 0 only checks existence/permission.
    match unsafe { libc::kill(pid as i32, 0) } {
        0 => Ok(()),
        _ => match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ESRCH) => Err("abandoned"),
            _ => Err("unknown"),
        },
    }
}

/// Existence probe via `OpenProcess`: success or access denied means the pid
/// exists; `ERROR_INVALID_PARAMETER` means it is gone.
#[cfg(windows)]
fn pid_alive(pid: i64) -> Result<(), &'static str> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    const ERROR_INVALID_PARAMETER: i32 = 87;
    // SAFETY: plain handle query; the handle is closed on success.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid as u32) };
    if !handle.is_null() {
        unsafe {
            CloseHandle(handle);
        }
        return Ok(());
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(ERROR_INVALID_PARAMETER) => Err("abandoned"),
        _ => Err("unknown"),
    }
}

/// Runtime state for one recorded PID: retained, abandoned, stale, live or unknown.
fn runtime_state(pid: Option<i64>, identity: Option<&str>, running: bool) -> &'static str {
    if !running {
        return "retained";
    }
    let Some(pid) = pid else {
        return "unavailable";
    };
    if let Err(state) = pid_alive(pid) {
        return state;
    }
    let current = process_identity(pid);
    match (identity, current) {
        (None, _) | (_, None) => "unknown",
        (Some(recorded), Some(now)) if now == recorded => "live",
        (Some(_), Some(_)) => "stale",
    }
}

/// Read older live-run records without making an observer write a migration.
fn normalize(document: &mut Value) {
    let Some(runs) = document["runs"].as_array_mut() else {
        return;
    };
    for run in runs {
        let Some(object) = run.as_object_mut() else {
            continue;
        };
        object
            .entry("source_metadata".to_owned())
            .or_insert(Value::Null);
        object.entry("telemetry".to_owned()).or_insert_with(|| {
            json!({
                "sampled_at": null,
                "rate_window_seconds": null,
                "queue_semantics": "unknown",
                "source": "legacy",
            })
        });
        if let Some(collector) = object.get_mut("collector").and_then(Value::as_object_mut) {
            collector.entry("origin".to_owned()).or_insert(Value::Null);
            collector
                .entry("aggregate_max_requests_per_second".to_owned())
                .or_insert(Value::Null);
        }
        if !object.contains_key("controller_pid") && object.contains_key("pid") {
            let pid = object["pid"].clone();
            object.insert("controller_pid".into(), pid);
            object.insert("controller_start_identity".into(), Value::Null);
            object.insert("collector_pid".into(), Value::Null);
            object.insert("collector_start_identity".into(), Value::Null);
            object.insert("collector_started_at".into(), Value::Null);
        }
    }
}

const RUN_KEYS: &[&str] = &[
    "id",
    "kind",
    "state",
    "started_at",
    "finished_at",
    "pid",
    "controller_pid",
    "controller_start_identity",
    "collector_pid",
    "collector_start_identity",
    "collector_started_at",
    "collector",
    "artifact",
    "counters",
    "finish_reason",
    "events",
    "telemetry",
    "source_metadata",
];

const COLLECTOR_KEYS: &[&str] = &[
    "mode",
    "max_urls",
    "max_requests",
    "max_crawl_seconds",
    "max_requests_per_second",
    "config_fingerprint",
    "resumed",
    "origin",
    "aggregate_max_requests_per_second",
];

fn validate(document: &Value, project_uuid: &str) -> ProjectsResult<()> {
    let invalid = |m: &str| crate::error::value(m.to_owned());
    let object = document.as_object().filter(|o| {
        o.keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>()
            == ["format", "revision", "project_uuid", "runs"]
                .into_iter()
                .collect()
    });
    let Some(object) = object else {
        return Err(invalid("run observation document has an unsupported shape"));
    };
    let runs = object["runs"].as_array().filter(|r| r.len() <= MAX_RUNS);
    let Some(runs) = runs else {
        return Err(invalid("run observation document has an unsupported shape"));
    };
    if object["format"].as_str() != Some(FORMAT)
        || object["project_uuid"].as_str() != Some(project_uuid)
        || object["revision"].as_i64().is_none_or(|r| r < 0)
        || object["revision"].is_boolean()
    {
        return Err(invalid("run observation document has an unsupported shape"));
    }
    let mut ids = std::collections::BTreeSet::new();
    for run in runs {
        let object = run.as_object().filter(|o| {
            o.keys()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>()
                == RUN_KEYS.iter().copied().collect()
        });
        let Some(object) = object else {
            return Err(invalid("run observation contains an unsupported run"));
        };
        text(&object["id"], "run id", 64)?;
        if !ids.insert(object["id"].as_str().unwrap_or_default().to_owned()) {
            return Err(invalid("run observation contains duplicate run IDs"));
        }
        if !KINDS.contains(&object["kind"].as_str().unwrap_or_default())
            || !STATES.contains(&object["state"].as_str().unwrap_or_default())
        {
            return Err(invalid("run observation has an unsupported kind or state"));
        }
        match object["pid"].as_i64() {
            Some(pid) if !object["pid"].is_boolean() && pid > 0 => {}
            _ => return Err(invalid("run observation has an invalid PID")),
        }
        if object["controller_pid"] != object["pid"] {
            return Err(invalid(
                "run observation controller PID disagrees with its compatibility PID",
            ));
        }
        if !object["controller_start_identity"].is_null()
            && !object["controller_start_identity"].is_string()
        {
            return Err(invalid("run observation controller identity is invalid"));
        }
        if !object["collector_pid"].is_null() {
            match object["collector_pid"].as_i64() {
                Some(pid) if !object["collector_pid"].is_boolean() && pid > 0 => {}
                _ => return Err(invalid("run observation collector PID is invalid")),
            }
        }
        if !object["collector_start_identity"].is_null()
            && !object["collector_start_identity"].is_string()
        {
            return Err(invalid("run observation collector identity is invalid"));
        }
        if !object["collector_started_at"].is_null() && !object["collector_started_at"].is_string()
        {
            return Err(invalid("run observation collector start time is invalid"));
        }
        let collector = object["collector"].as_object().filter(|c| {
            c.keys()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>()
                == COLLECTOR_KEYS.iter().copied().collect()
        });
        let Some(collector) = collector else {
            return Err(invalid("run observation collector shape is invalid"));
        };
        if !MODES.contains(&collector["mode"].as_str().unwrap_or_default()) {
            return Err(invalid("run observation collector mode is invalid"));
        }
        counter(&collector["max_urls"], "collector max_urls")?;
        counter(&collector["max_requests"], "collector max_requests")?;
        counter(
            &collector["max_crawl_seconds"],
            "collector max_crawl_seconds",
        )?;
        if !rate_ok(&collector["max_requests_per_second"]) {
            return Err(invalid("run observation request-rate limit is invalid"));
        }
        if !collector["config_fingerprint"].is_string() {
            return Err(invalid("run observation config fingerprint is invalid"));
        }
        if !collector["resumed"].is_boolean() {
            return Err(invalid("run observation resumed flag is invalid"));
        }
        if !collector["origin"].is_null()
            && collector["origin"]
                .as_str()
                .is_none_or(|o| o.is_empty() || o.len() > 253)
        {
            return Err(invalid("run observation origin is invalid"));
        }
        if !rate_ok(&collector["aggregate_max_requests_per_second"]) {
            return Err(invalid("run observation aggregate request rate is invalid"));
        }
        if !object["artifact"].is_null() && !object["artifact"].is_string() {
            return Err(invalid("run observation artifact is invalid"));
        }
        let counters = object["counters"].as_object().filter(|c| {
            c.keys()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>()
                == [
                    "fetched",
                    "queued",
                    "inflight",
                    "excluded",
                    "rate_per_second",
                ]
                .into_iter()
                .collect()
        });
        let Some(counters) = counters else {
            return Err(invalid("run observation counters are invalid"));
        };
        for (name, value) in counters {
            if name == "rate_per_second" {
                if !rate_ok(value) {
                    return Err(invalid("run observation rate is invalid"));
                }
            } else {
                optional_counter(value, name)?;
            }
        }
        let source = &object["source_metadata"];
        if !source.is_null() {
            let object = source.as_object().filter(|s| {
                object["kind"].as_str() == Some("sitemap")
                    && s.keys()
                        .map(String::as_str)
                        .collect::<std::collections::BTreeSet<_>>()
                        == [
                            "root",
                            "declared_url_count",
                            "parsed_documents",
                            "error_count",
                            "truncated",
                        ]
                        .into_iter()
                        .collect()
            });
            let Some(object) = object else {
                return Err(invalid("run source metadata has an unsupported shape"));
            };
            text(&object["root"], "sitemap root", 8192)?;
            for name in ["declared_url_count", "parsed_documents", "error_count"] {
                counter(&object[name], name)?;
            }
            if !object["truncated"].is_boolean() {
                return Err(invalid("sitemap truncated state must be boolean"));
            }
        }
        let telemetry = object["telemetry"].as_object().filter(|t| {
            t.keys()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>()
                == [
                    "sampled_at",
                    "rate_window_seconds",
                    "queue_semantics",
                    "source",
                ]
                .into_iter()
                .collect()
        });
        let Some(telemetry) = telemetry else {
            return Err(invalid("run telemetry has an unsupported shape"));
        };
        if !telemetry["sampled_at"].is_null() {
            let stamp = telemetry["sampled_at"]
                .as_str()
                .ok_or_else(|| invalid("run sample timestamp must be text"))?;
            let parsed = pytime::fromisoformat(&stamp.replace('Z', "+00:00"))
                .map_err(|_| invalid("run sample timestamp must include a timezone"))?;
            if !parsed.is_aware() {
                return Err(invalid("run sample timestamp must include a timezone"));
            }
        }
        let window = &telemetry["rate_window_seconds"];
        if !window.is_null() {
            let ok = window.as_f64().is_some_and(|w| w.is_finite() && w >= 0.0);
            if !ok {
                return Err(invalid("run rate window is invalid"));
            }
        }
        if !["unknown", "separate", "outstanding"]
            .contains(&telemetry["queue_semantics"].as_str().unwrap_or_default())
        {
            return Err(invalid("run queue semantics is invalid"));
        }
        text(&telemetry["source"], "telemetry source", 64)?;
        if !object["finish_reason"].is_null() && !object["finish_reason"].is_string() {
            return Err(invalid("run observation finish reason is invalid"));
        }
        let events = object["events"]
            .as_array()
            .filter(|e| e.len() <= MAX_EVENTS_PER_RUN);
        if events.is_none() {
            return Err(invalid("run observation events are invalid"));
        }
    }
    Ok(())
}

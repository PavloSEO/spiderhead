//! Bounded observed navigation evidence; unknown causes are never inferred as JS.
//! A pure state machine fed with plain event structs. Browser events are bound to CDP
//! objects in the `render` crate, which owns the browser glue.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::py_url;
use crate::types::{Json, ParseError, Result};
use crate::urlutil;

mod expand_navigation;
pub use expand_navigation::*;

/// Keys of one event in the live form, besides the two URL keys.
const EVENT_KEYS: [&str; 5] = [
    "elapsed_ms",
    "kind",
    "observation",
    "user_click",
    "status_code",
];
/// Keys of the versioned adjunct in its stored form.
const STORED_KEYS: [&str; 7] = [
    "schema",
    "state",
    "reason",
    "events_omitted",
    "interaction_policy",
    "policy",
    "events",
];
/// Longest accepted navigation URL, in characters.
const MAX_URL_CHARS: usize = 16384;

/// A JSON integer (floats and booleans are not).
fn py_int(value: &Json) -> Option<i128> {
    match value {
        Json::Number(number) => number
            .as_i64()
            .map(i128::from)
            .or_else(|| number.as_u64().map(i128::from)),
        _ => None,
    }
}

/// Validation error with the contract message.
fn invalid<T>(message: &str) -> Result<T> {
    Err(ParseError::Invalid(message.to_owned()))
}

/// Safety test on one URL: scheme `http`/`https`, a host, and no userinfo. A malformed
/// netloc is an error.
fn url_is_unsafe(url: &str) -> Result<bool> {
    let parts = py_url::urlsplit(url).map_err(|e| ParseError::Invalid(e.to_string()))?;
    let host_present = urlutil::hostname(url)?.is_some_and(|host| !host.is_empty());
    let (username, password) = match parts.netloc.rsplit_once('@') {
        None => (String::new(), String::new()),
        Some((userinfo, _)) => match userinfo.split_once(':') {
            Some((user, pass)) => (user.to_owned(), pass.to_owned()),
            None => (userinfo.to_owned(), String::new()),
        },
    };
    Ok(!matches!(parts.scheme.as_str(), "http" | "https")
        || !host_present
        || !username.is_empty()
        || !password.is_empty())
}

/// Allowed observation sources for one event kind.
fn allowed_observations(kind: &str) -> &'static [&'static str] {
    match kind {
        "initial_http_navigation" => &["cdp_frame", "playwright_frame"],
        "document_navigation" => &["cdp_frame", "playwright_frame", "cdp_history"],
        "http_redirect" => &["http_response"],
        "spa_history_change" | "fragment_navigation" => &["cdp_history"],
        _ => &["cdp_requested"],
    }
}

/// Schema tag.
pub const SCHEMA: &str = "spiderhead.navigation.v1";
/// Maximum retained events.
pub const EVENT_CAP: usize = 32;
/// Allowed event kinds.
pub const KINDS: [&str; 9] = [
    "initial_http_navigation",
    "document_navigation",
    "script_navigation",
    "spa_history_change",
    "fragment_navigation",
    "http_redirect",
    "anchor_navigation",
    "form_navigation",
    "refresh_navigation",
];
/// Allowed observation sources.
pub const OBSERVATIONS: [&str; 5] = [
    "playwright_frame",
    "http_response",
    "cdp_frame",
    "cdp_history",
    "cdp_requested",
];

/// One observed navigation event (live form: `source`/`destination` are URLs).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NavigationEvent {
    /// URL navigated from.
    pub source: String,
    /// URL navigated to.
    pub destination: String,
    /// Milliseconds since capture start (non-decreasing).
    pub elapsed_ms: u64,
    /// One of [`KINDS`].
    pub kind: String,
    /// One of [`OBSERVATIONS`].
    pub observation: String,
    /// HTTP status; set only for `http_redirect` (3xx).
    pub status_code: Option<u16>,
    /// Always `None`: a user click is never observed.
    pub user_click: Option<bool>,
}

/// The versioned navigation adjunct (live form).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NavigationEvidence {
    /// `spiderhead.navigation.v1`.
    pub schema: String,
    /// Requested URL.
    pub requested_url: String,
    /// Final URL.
    pub final_url: String,
    /// Wait condition.
    pub wait_until: String,
    /// Timeout.
    pub timeout_seconds: f64,
    /// Always `no_clicks`.
    pub interaction_policy: String,
    /// Always `pinned_http`.
    pub policy: String,
    /// Up to [`EVENT_CAP`] events.
    pub events: Vec<NavigationEvent>,
    /// Events beyond the cap or with unsafe URLs.
    pub events_omitted: u64,
    /// `complete`, `partial` or `unavailable`.
    pub state: String,
    /// Reason (<= 128 chars), `""` when complete.
    pub reason: String,
}

/// Validate the versioned adjunct (`stored` = URL references instead of strings;
/// `resolve_url` checks retained references). Error messages are part of the contract.
pub fn validate_navigation(
    value: &Json,
    stored: bool,
    resolve_url: Option<&dyn Fn(u64) -> Option<String>>,
) -> Result<()> {
    let Some(map) = value.as_object() else {
        return invalid("navigation evidence schema is invalid");
    };
    if map.get("schema").and_then(Json::as_str) != Some(SCHEMA) {
        return invalid("navigation evidence schema is invalid");
    }
    if stored
        && !(map.len() == STORED_KEYS.len() && STORED_KEYS.iter().all(|k| map.contains_key(*k)))
    {
        return invalid("retained navigation fields are invalid");
    }
    let state = map.get("state").and_then(Json::as_str).unwrap_or_default();
    if !matches!(state, "complete" | "partial" | "unavailable") {
        return invalid("navigation evidence state is invalid");
    }
    if map.get("interaction_policy").and_then(Json::as_str) != Some("no_clicks")
        || map.get("policy").and_then(Json::as_str) != Some("pinned_http")
    {
        return invalid("navigation evidence policy is invalid");
    }
    let reason = match map.get("reason").and_then(Json::as_str) {
        Some(text) if text.chars().count() <= 128 => text,
        _ => return invalid("navigation evidence reason is invalid"),
    };
    let events = match map.get("events").and_then(Json::as_array) {
        Some(items) if items.len() <= EVENT_CAP => items,
        _ => return invalid("navigation evidence events exceed the bound"),
    };
    let omitted = match map.get("events_omitted").and_then(py_int) {
        Some(count) if count >= 0 => count,
        _ => return invalid("navigation omitted count is invalid"),
    };
    if state == "complete" && (omitted != 0 || !reason.is_empty()) {
        return invalid("complete navigation evidence has omissions");
    }
    if state != "complete" && reason.is_empty() {
        return invalid("incomplete navigation evidence needs a reason");
    }
    if state == "complete" && events.is_empty() {
        return invalid("complete navigation evidence requires an observed event");
    }
    let names = if stored {
        ["source_url_id", "destination_url_id"]
    } else {
        ["source", "destination"]
    };
    let mut previous: i128 = -1;
    for event in events {
        let Some(fields) = event.as_object() else {
            return invalid("navigation event fields are invalid");
        };
        let shape_ok = fields.len() == names.len() + EVENT_KEYS.len()
            && names
                .iter()
                .chain(EVENT_KEYS.iter())
                .all(|k| fields.contains_key(*k));
        if !shape_ok {
            return invalid("navigation event fields are invalid");
        }
        let elapsed = match py_int(&fields["elapsed_ms"]) {
            Some(ms) if ms >= previous => ms,
            _ => return invalid("navigation event timing is invalid"),
        };
        previous = elapsed;
        let kind = fields["kind"].as_str().unwrap_or_default();
        let observation = fields["observation"].as_str().unwrap_or_default();
        if !KINDS.contains(&kind) || !OBSERVATIONS.contains(&observation) {
            return invalid("navigation event cause is invalid");
        }
        if !allowed_observations(kind).contains(&observation) {
            return invalid("navigation cause disagrees with its observation");
        }
        if !fields["user_click"].is_null() {
            return invalid("navigation user-click cause was not observed");
        }
        let status = &fields["status_code"];
        let redirect = kind == "http_redirect";
        let status_ok = match py_int(status) {
            Some(code) => redirect && (300..400).contains(&code),
            None => !redirect && status.is_null(),
        };
        if !status_ok {
            return invalid("navigation redirect status is invalid");
        }
        for name in names {
            let url = if stored {
                let id = match py_int(&fields[name]) {
                    Some(id) if id >= 1 => id as u64,
                    _ => return invalid("navigation URL reference is invalid"),
                };
                let Some(resolve) = resolve_url else {
                    continue;
                };
                match resolve(id) {
                    Some(url) => url,
                    None => return invalid("navigation URL is invalid"),
                }
            } else {
                match fields[name].as_str() {
                    Some(url) => url.to_owned(),
                    None => return invalid("navigation URL is invalid"),
                }
            };
            if url.chars().count() > MAX_URL_CHARS {
                return invalid("navigation URL is invalid");
            }
            if url_is_unsafe(&url)? {
                return invalid("navigation URL is unsafe");
            }
        }
    }
    Ok(())
}

/// Storage-safe event URL identities without duplicating URL strings; pre-versioned input
/// becomes explicitly `unavailable`.
pub fn retain_navigation(raw: &Json, intern_url: &mut dyn FnMut(&str) -> u64) -> Result<Json> {
    let Some(source) = raw.as_object() else {
        return invalid("navigation evidence schema is invalid");
    };
    let schema = source.get("schema").filter(|v| !v.is_null());
    if schema.is_some_and(|v| v.as_str() != Some(SCHEMA)) {
        return invalid("navigation evidence schema is unsupported");
    }
    // Pre-versioned renderers are explicitly unavailable, never reinterpreted as observed.
    let raw_map: Map<String, Value> = if schema.is_none() {
        serde_json::json!({
            "schema": SCHEMA,
            "state": "unavailable",
            "reason": "legacy_navigation_not_captured",
            "events": [],
            "events_omitted": 0,
            "interaction_policy": "no_clicks",
            "policy": "pinned_http",
        })
        .as_object()
        .cloned()
        .unwrap_or_default()
    } else {
        source.clone()
    };
    validate_navigation(&Value::Object(raw_map.clone()), false, None)?;
    let mut result = Map::new();
    for name in [
        "schema",
        "state",
        "reason",
        "events_omitted",
        "interaction_policy",
        "policy",
    ] {
        result.insert(name.to_owned(), raw_map[name].clone());
    }
    let mut events = Vec::new();
    for event in raw_map["events"].as_array().into_iter().flatten() {
        let Some(fields) = event.as_object() else {
            continue;
        };
        let mut out: Map<String, Value> = fields
            .iter()
            .filter(|(k, _)| k.as_str() != "source" && k.as_str() != "destination")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let source_url = fields["source"].as_str().unwrap_or_default();
        out.insert(
            "source_url_id".to_owned(),
            Value::from(intern_url(source_url)),
        );
        let destination_url = fields["destination"].as_str().unwrap_or_default();
        out.insert(
            "destination_url_id".to_owned(),
            Value::from(intern_url(destination_url)),
        );
        events.push(Value::Object(out));
    }
    result.insert("events".to_owned(), Value::Array(events));
    Ok(Value::Object(result))
}

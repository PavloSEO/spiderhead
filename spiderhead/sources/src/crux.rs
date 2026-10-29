//! Chrome UX Report (CrUX): Core Web Vitals as real users experienced them.
//!
//! Field data at the 75th percentile, at URL or
//! origin level, plus the history endpoint and a bounded multi-URL sample with an explicit
//! on-disk cache. The API key travels in the `X-goog-api-key` header, never in the URL, so
//! it cannot leak through a logged request line.
//!
//! This is a credential-gated client: a missing key returns an
//! explicit `not_configured` envelope, never a fabricated result.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use spiderhead_core::host::Host;
use thiserror::Error;

use crate::credentials::{self, CredentialError};
use crate::cwv;
use crate::http::{Request, Response, Transport, TransportError};
use crate::{pyerr, pyjson, pystr};

mod cache;
use cache::*;
mod cache_write;
use cache_write::*;
mod history;
pub use history::*;

/// `HOST` — current-record endpoint.
pub const HOST: &str = "https://chromeuxreport.googleapis.com/v1/records:queryRecord";
/// `HISTORY_HOST` — timeseries endpoint.
pub const HISTORY_HOST: &str =
    "https://chromeuxreport.googleapis.com/v1/records:queryHistoryRecord";
/// `TIMEOUT` — whole-request budget in seconds.
pub const TIMEOUT: f64 = 30.0;
/// `MAX_SAMPLES` — the largest accepted bounded URL sample.
pub const MAX_SAMPLES: i64 = 25;
/// `MAX_RESPONSE_BYTES` — bounded provider input budget (2 MiB).
pub const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
/// `MAX_CACHE_BYTES` — largest accepted cache file (64 KiB).
pub const MAX_CACHE_BYTES: usize = 64 * 1024;

const MALFORMED: &str = "CrUX malformed response";
const HISTORY_MALFORMED: &str = "CrUX History malformed response";
const TOO_LARGE: &str = "CrUX response exceeded the 2 MiB limit";

/// Errors raised before or around the call: invalid input and credential failures.
#[derive(Debug, Error)]
pub enum CruxError {
    /// Rejected input: exactly one of `url` or `origin`, sample budget, or a symlinked
    /// cache root.
    #[error("{0}")]
    Invalid(String),
    /// Credential store failures other than a missing key propagate with their own class.
    #[error("{0}")]
    Credential(#[from] CredentialError),
}

impl CruxError {
    /// Error class name reported to callers.
    pub fn python_class(&self) -> &'static str {
        match self {
            CruxError::Invalid(_) => "ValueError",
            CruxError::Credential(e) => e.python_class(),
        }
    }
}

fn invalid<T>(message: &str) -> Result<T, CruxError> {
    Err(CruxError::Invalid(message.to_owned()))
}

/// Current UTC time in seconds precision, or a fixed stamp when one is injected.
fn retrieved_at(now: Option<&str>) -> String {
    match now {
        Some(stamp) => stamp.to_owned(),
        None => {
            let secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs_f64().floor())
                .unwrap_or(0.0);
            spiderhead_core::timeutil::stamp_epoch(secs, spiderhead_core::timeutil::Format::Iso)
        }
    }
}

/// Failure kinds of a POST that `query` and `history` tell apart.
enum FetchFail {
    /// `ResponseTooLarge` — the wire body exceeded [`MAX_RESPONSE_BYTES`].
    TooLarge,
    /// `UnicodeError` — the body was not valid UTF-8.
    Undecodable,
    /// `HTTPError` — a received error response (including a refused 3xx).
    Http(Response),
    /// `URLError`/`TimeoutError` — nothing came back.
    Connect(String),
}

/// POST the payload as JSON with the key in `X-goog-api-key`, with the response capped at
/// [`MAX_RESPONSE_BYTES`].
async fn post<T: Transport>(
    transport: &T,
    host: &str,
    payload: &Value,
    api_key: &str,
) -> Result<String, FetchFail> {
    let request = Request::new("POST", host)
        .header("Content-Type", "application/json")
        .header("X-goog-api-key", api_key)
        .body(pyjson::dumps(payload).into_bytes())
        .timeout(TIMEOUT);
    let response = match transport.send(&request).await {
        Ok(response) => response,
        Err(TransportError::Http(response)) => return Err(FetchFail::Http(response)),
        Err(TransportError::Connect(kind)) => return Err(FetchFail::Connect(kind)),
        Err(TransportError::InvalidRequest(message)) => {
            return Err(FetchFail::Connect(message));
        }
    };
    if response.body.len() > MAX_RESPONSE_BYTES {
        return Err(FetchFail::TooLarge);
    }
    pystr::decode_utf8_strict(&response.body).map_err(|_| FetchFail::Undecodable)
}

/// Text of a parsed-JSON `error.message`: strings stay raw, scalars print plainly, and
/// containers print in their debug form.
fn str_message(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(_) | Value::Object(_) => repr_json(value),
        other => pystr::repr_value(other),
    }
}

/// Debug form of a JSON value, with strings in single quotes.
fn repr_json(value: &Value) -> String {
    match value {
        Value::String(text) => pystr::repr_str(text),
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(repr_json).collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Object(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", repr_json(&Value::String(k.clone())), repr_json(v)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
        other => pystr::repr_value(other),
    }
}

/// The provider's `error.message` when the error body parses, else the HTTP reason phrase;
/// an oversized error body gets its own message.
fn api_error(response: &Response) -> String {
    if response.body.len() > MAX_RESPONSE_BYTES {
        return "CrUX error response exceeded the 2 MiB limit".to_owned();
    }
    let text = String::from_utf8_lossy(&response.body);
    let reason = pyerr::reason_phrase(response.status);
    match serde_json::from_str::<Value>(&text) {
        Ok(body) => {
            let message = body
                .get("error")
                .filter(|e| e.is_object())
                .and_then(|e| e.get("message"));
            match message {
                Some(m) if pystr::truthy(Some(m)) => str_message(m),
                _ => reason.to_owned(),
            }
        }
        Err(_) => reason.to_owned(),
    }
}

/// The body as a JSON object, or None.
fn response_object(raw: &str) -> Option<Map<String, Value>> {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|v| v.as_object().cloned())
}

/// Options for `query`.
#[derive(Debug, Default)]
pub struct Query<'a> {
    /// `url` — exactly one of `url`/`origin` must be a non-empty string.
    pub url: Option<&'a str>,
    /// `origin` — exactly one of `url`/`origin` must be a non-empty string.
    pub origin: Option<&'a str>,
    /// `form_factor` — `PHONE`/`DESKTOP`/`TABLET`; absent means all form factors.
    pub form_factor: Option<&'a str>,
    /// `metrics` — passed through into the payload verbatim when truthy.
    pub metrics: Option<&'a Value>,
    /// `api_key` — falls back to the `crux/api_key` credential when empty.
    pub api_key: Option<&'a str>,
    /// Fixed timestamp override; None uses the real UTC clock.
    pub now: Option<&'a str>,
}

fn present(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.is_empty())
}

/// The API key, or a `not_configured` outcome when none is available.
fn api_token<T>(
    host: &dyn Host,
    root: &Path,
    api_key: Option<&str>,
    not_configured: impl FnOnce(String) -> Result<T, CruxError>,
) -> Result<String, Result<T, CruxError>> {
    match api_key.filter(|k| !k.is_empty()) {
        Some(k) => Ok(k.to_owned()),
        None => match credentials::crux_api_key(host, root) {
            Ok(k) => Ok(k),
            Err(CredentialError::Missing(message)) => Err(not_configured(message)),
            Err(e) => Err(Err(CruxError::Credential(e))),
        },
    }
}

/// Current-window field Core Web Vitals for a URL or origin at the 75th percentile.
///
/// A target with too little real-user traffic is `ok` with `state: "no_field_data"` —
/// the provider answers 404 for it and that is data, not a failure.
pub async fn query<T: Transport>(
    host: &dyn Host,
    root: &Path,
    transport: &T,
    args: &Query<'_>,
) -> Result<Value, CruxError> {
    if present(args.url) == present(args.origin) {
        return invalid("exactly one of url or origin is required");
    }
    let url_mode = present(args.url);
    let mut context = Map::new();
    context.insert(
        "target".to_owned(),
        json!(if url_mode { args.url } else { args.origin }),
    );
    context.insert(
        "target_kind".to_owned(),
        json!(if url_mode { "url" } else { "origin" }),
    );
    context.insert(
        "form_factor".to_owned(),
        json!(args
            .form_factor
            .filter(|f| !f.is_empty())
            .unwrap_or("ALL_FORM_FACTORS")),
    );
    context.insert("metric_source".to_owned(), json!("CrUX current field data"));
    context.insert("retrieved_at".to_owned(), json!(retrieved_at(args.now)));
    let finish = |result: Value| -> Result<Value, CruxError> {
        let mut merged = context.clone();
        if let Value::Object(m) = result {
            merged.extend(m);
        }
        let mut result = Value::Object(merged);
        let assessment = cwv::assess(&result).map_err(|e| CruxError::Invalid(e.to_string()))?;
        if let Value::Object(m) = &mut result {
            m.insert("assessment".to_owned(), assessment);
            let downgrade = m.get("state").and_then(Value::as_str) == Some("complete")
                && matches!(
                    m.get("assessment")
                        .and_then(|a| a.get("overall"))
                        .and_then(Value::as_str),
                    Some("partial" | "unavailable")
                );
            if downgrade {
                m.insert("state".to_owned(), json!("partial"));
            }
        }
        Ok(result)
    };

    let api_token = match api_token(host, root, args.api_key, |message| {
        finish(json!({"ok": false, "state": "not_configured", "error": message}))
    }) {
        Ok(token) => token,
        Err(result) => return result,
    };

    let mut payload = Map::new();
    payload.insert(
        if url_mode { "url" } else { "origin" }.to_owned(),
        json!(if url_mode { args.url } else { args.origin }),
    );
    if let Some(ff) = args.form_factor.filter(|f| !f.is_empty()) {
        payload.insert("formFactor".to_owned(), json!(ff));
    }
    if let Some(metrics) = args.metrics.filter(|m| pystr::truthy(Some(m))) {
        payload.insert("metrics".to_owned(), metrics.clone());
    }

    let raw = match post(transport, HOST, &Value::Object(payload), &api_token).await {
        Ok(raw) => raw,
        Err(FetchFail::TooLarge) => {
            return finish(json!({"ok": false, "state": "response_too_large", "error": TOO_LARGE}));
        }
        Err(FetchFail::Undecodable) => {
            return finish(json!({"ok": false, "state": "failed", "error": MALFORMED}));
        }
        Err(FetchFail::Http(response)) => {
            if response.status == 404 {
                return finish(json!({"ok": true, "state": "no_field_data", "metrics": {},
                           "note": "no CrUX data"}));
            }
            return finish(json!({
                "ok": false, "state": "failed",
                "error": api_error(&response), "status": response.status,
            }));
        }
        Err(FetchFail::Connect(kind)) => {
            return finish(json!({
                "ok": false, "state": "failed",
                "error": format!("CrUX request failed: {}", pyerr::url_error(&kind)),
            }));
        }
    };

    if raw.len() > MAX_RESPONSE_BYTES {
        return finish(json!({"ok": false, "state": "response_too_large", "error": TOO_LARGE}));
    }
    let Some(body) = response_object(&raw) else {
        return finish(json!({"ok": false, "state": "failed", "error": MALFORMED}));
    };
    let Some(record) = body.get("record").filter(|r| r.is_object()) else {
        return finish(json!({"ok": false, "state": "failed", "error": MALFORMED}));
    };
    let (Some(Value::Object(record_key)), Some(Value::Object(metric_data))) =
        (record.get("key"), record.get("metrics"))
    else {
        return finish(json!({"ok": false, "state": "failed", "error": MALFORMED}));
    };
    let mut values_by_metric = Map::new();
    for (name, values) in metric_data {
        // A missing `percentiles` key is an empty object, not a malformed response.
        let Some(values) = values.as_object() else {
            return finish(json!({"ok": false, "state": "failed", "error": MALFORMED}));
        };
        let percentiles = values.get("percentiles").cloned().unwrap_or(json!({}));
        if !percentiles.is_object() {
            return finish(json!({"ok": false, "state": "failed", "error": MALFORMED}));
        }
        values_by_metric.insert(name.clone(), percentiles);
    }
    let returned_target = record_key.get(if url_mode { "url" } else { "origin" });
    if pystr::truthy(record_key.get(if url_mode { "origin" } else { "url" })) {
        return finish(
            json!({"ok": false, "state": "failed", "error": "CrUX target kind mismatch"}),
        );
    }
    let Some(returned_target) = returned_target
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
    else {
        return finish(json!({"ok": false, "state": "failed", "error": MALFORMED}));
    };
    let key_ff = record_key.get("formFactor").filter(|v| !v.is_null());
    let ff_matches = match (key_ff, args.form_factor) {
        (None, None) => true,
        (Some(Value::String(s)), Some(f)) => s == f,
        _ => false,
    };
    if !ff_matches {
        return finish(
            json!({"ok": false, "state": "failed", "error": "CrUX form factor mismatch"}),
        );
    }
    let out_ff = key_ff
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            args.form_factor
                .filter(|f| !f.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "ALL_FORM_FACTORS".to_owned());
    let metrics: Map<String, Value> = values_by_metric
        .iter()
        .map(|(name, percentiles)| {
            (
                name.clone(),
                json!({"p75": percentiles.get("p75").cloned().unwrap_or(Value::Null)}),
            )
        })
        .collect();
    finish(json!({
        "ok": true,
        "state": "complete",
        "record_target": returned_target,
        "form_factor": out_ff,
        "collection_period": record.get("collectionPeriod").cloned().unwrap_or(Value::Null),
        "metrics": Value::Object(metrics),
    }))
}

/// Options for `sample_urls`.
#[derive(Debug)]
pub struct Sample<'a> {
    /// `urls` — must be a JSON array of absolute HTTP(S) strings.
    pub urls: &'a Value,
    /// `form_factor` — optional CrUX form factor.
    pub form_factor: Option<&'a str>,
    /// Sample size from 1 to 25; None uses [`MAX_SAMPLES`].
    pub max_samples: Option<&'a Value>,
    /// `cache_dir` — opt-in on-disk cache directory.
    pub cache_dir: Option<&'a Path>,
    /// `cache_max_age_hours` — cache freshness window (`None` defaults to 24).
    pub cache_max_age_hours: Option<&'a Value>,
    /// `api_key` — falls back to the `crux/api_key` credential when empty.
    pub api_key: Option<&'a str>,
    /// Fixed timestamp override; None uses the real clock.
    pub now: Option<&'a str>,
}

/// An explicit, bounded CrUX URL sample. The optional cache never stores credentials and
/// never spends a provider request on a bad entry silently.
pub async fn sample<T: Transport>(
    host: &dyn Host,
    root: &Path,
    transport: &T,
    args: &Sample<'_>,
) -> Result<Value, CruxError> {
    let urls_ok = match args.urls {
        Value::Array(items) => {
            !items.is_empty()
                && items.iter().all(|u| {
                    u.as_str()
                        .is_some_and(|s| s.starts_with("https://") || s.starts_with("http://"))
                })
        }
        _ => false,
    };
    if !urls_ok {
        return invalid("urls must be a nonempty list of absolute HTTP(S) URL strings");
    }
    let items = args.urls.as_array().cloned().unwrap_or_default();
    let max_samples = match args.max_samples {
        None => Some(MAX_SAMPLES),
        Some(v) => valid_max_samples(v),
    };
    let age_hours = match args.cache_max_age_hours {
        None => Some(24.0),
        Some(v) => valid_cache_age(v),
    };
    let (Some(max_samples), Some(age_hours)) = (max_samples, age_hours) else {
        return invalid("sample budget must be 1..25 and cache age must be positive");
    };
    let targets = dedup(&items);
    let sampled: Vec<&Value> = targets.iter().take(max_samples as usize).collect();
    if let Some(root_dir) = args.cache_dir {
        if is_symlink(root_dir) {
            return invalid("cache directory must not be a symlink");
        }
    }
    let mut records: Vec<Value> = Vec::new();
    let mut requests = 0i64;
    let mut hits = 0i64;
    for target in &sampled {
        let target = target.as_str().unwrap_or_default();
        let cache = args
            .cache_dir
            .map(|r| cache_file(r, target, args.form_factor));
        if let Some(path) = cache.as_ref() {
            if let Some(record) = read_cache(path, target, args.form_factor, age_hours, args.now) {
                if record.get("cache").and_then(Value::as_str) == Some("hit") {
                    hits += 1;
                }
                records.push(record);
                continue;
            }
        }
        let mut result = query(
            host,
            root,
            transport,
            &Query {
                url: Some(target),
                origin: None,
                form_factor: args.form_factor,
                metrics: None,
                api_key: args.api_key,
                now: args.now,
            },
        )
        .await?;
        if let Value::Object(m) = &mut result {
            m.insert(
                "cache".to_owned(),
                json!(if cache.is_some() { "miss" } else { "disabled" }),
            );
        }
        if result.get("state").and_then(Value::as_str) != Some("not_configured") {
            requests += 1;
        }
        if let (Some(path), Some(true)) =
            (cache.as_ref(), result.get("ok").and_then(Value::as_bool))
        {
            let encoded = pyjson::dumps_opts(&result, true, true).into_bytes();
            if encoded.len() > MAX_CACHE_BYTES {
                if let Value::Object(m) = &mut result {
                    m.insert("cache".to_owned(), json!("not_written_too_large"));
                }
            } else if let Some(dir) = path.parent() {
                write_cache_file(dir, path, &encoded);
            }
        }
        records.push(result);
    }
    let measured: Vec<&Value> = records
        .iter()
        .filter(|r| {
            !matches!(
                r.get("assessment")
                    .and_then(|a| a.get("overall"))
                    .and_then(Value::as_str),
                Some("partial" | "unavailable")
            )
        })
        .collect();
    let complete = measured.len() as i64;
    let measured_targets: Vec<Value> = measured
        .iter()
        .map(|r| {
            let rt = r.get("record_target").cloned().unwrap_or(Value::Null);
            if pystr::truthy(Some(&rt)) {
                rt
            } else {
                r.get("target").cloned().unwrap_or(Value::Null)
            }
        })
        .collect();
    let mut unique = Vec::<Value>::new();
    for target in &measured_targets {
        if !unique.contains(target) {
            unique.push(target.clone());
        }
    }
    let duplicate_record_targets = measured_targets.len() as i64 - unique.len() as i64;
    let state = if complete == 0 {
        "unavailable"
    } else if complete < targets.len() as i64 || duplicate_record_targets > 0 {
        "partial"
    } else {
        "complete"
    };
    Ok(json!({
        "provider": "crux",
        "metric_source": "field",
        "state": state,
        "requested": targets.len() as i64,
        "sampled": sampled.len() as i64,
        "omitted": targets.len() as i64 - sampled.len() as i64,
        "requests": requests,
        "cache_hits": hits,
        "duplicate_record_targets": duplicate_record_targets,
        "cache_max_age_hours": if args.cache_dir.is_some() {
            args.cache_max_age_hours.cloned().unwrap_or(json!(24))
        } else {
            Value::Null
        },
        "cost_mode": "free_within_quota",
        "quota_mode": "Google Cloud API quota",
        "records": records,
    }))
}

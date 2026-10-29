//! Bounded PageSpeed Insights v5 samples, separate from CrUX and local measurements.
//!
//! PSI is a lab tool, so this client keeps the "lab sample, not field data" framing:
//! `lab_only` and the note strings are part of the contract. Results are written to an explicit
//! on-disk cache only when the caller asks for one.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use spiderhead_core::host::Host;
use thiserror::Error;

use crate::credentials;
use crate::http::{Request, Transport, TransportError};
use crate::{pyerr, pyjson, pystr};

/// PSI v5 endpoint.
pub const HOST: &str = "https://www.googleapis.com/pagespeedonline/v5/runPagespeed";
/// `MAX_URLS` — the call is a bounded sample, not a crawl.
pub const MAX_URLS: usize = 25;
/// `TIMEOUT` — Lighthouse runs are slow; the provider gives them a long budget.
pub const TIMEOUT: f64 = 90.0;

/// Errors raised before the call is built (invalid input or wrong type).
#[derive(Debug, Error)]
#[error("{message}")]
pub struct PagespeedError {
    /// Stable error class name reported to callers.
    class: &'static str,
    /// Error message text.
    message: String,
}

impl PagespeedError {
    /// Error class name of the matching failure.
    pub fn python_class(&self) -> &'static str {
        self.class
    }
}

fn raise<T>(class: &'static str, message: String) -> Result<T, PagespeedError> {
    Err(PagespeedError { class, message })
}

/// `accessibility_sample(sample)`: project one parsed PSI response into a bounded
/// accessibility review sample. Scores stay provider-reported; nothing becomes a WCAG verdict.
pub fn accessibility_sample(sample: &Value) -> Value {
    let category = sample
        .get("categories")
        .filter(|v| pystr::truthy(Some(v)))
        .and_then(|v| v.get("accessibility"));
    let audits = sample
        .get("audits")
        .filter(|v| pystr::truthy(Some(v)))
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !category.is_some_and(Value::is_object) || !audits.is_object() {
        return json!({
            "state": "unavailable",
            "reason": "PSI response has no Lighthouse accessibility category",
            "scope": {"url": sample.get("url"), "strategy": sample.get("strategy")},
        });
    }
    let category = category.unwrap();
    let mut findings = Vec::new();
    for (audit_id, audit) in audits.as_object().into_iter().flatten() {
        if !audit.is_object() {
            continue;
        }
        let score = audit.get("score").filter(|v| !v.is_null());
        let mode = audit.get("score_display_mode");
        // A boolean `true` also counts as a score of 1.
        if matches!(score, Some(Value::Number(n)) if n.as_f64() == Some(1.0))
            || matches!(score, Some(Value::Bool(true)))
        {
            continue;
        }
        let special = matches!(
            mode.and_then(Value::as_str),
            Some("manual" | "informative" | "notApplicable")
        );
        let state = if score.is_none() && !special {
            "incomplete"
        } else if score.is_none() || special {
            continue;
        } else {
            "failed"
        };
        findings.push(json!({
            "id": audit_id,
            "state": state,
            "score": audit.get("score").cloned().unwrap_or(Value::Null),
            "display_value": audit.get("display_value").cloned().unwrap_or(Value::Null),
            "score_display_mode": mode.cloned().unwrap_or(Value::Null),
        }));
    }
    json!({
        "state": "complete",
        "scope": {
            "url": sample.get("url"),
            "final_url": sample.get("final_url"),
            "strategy": sample.get("strategy"),
            "provider": "pagespeed",
            "engine": "Lighthouse",
            "version": sample.get("lighthouse_version"),
        },
        "category": {"score": category.get("score"), "title": category.get("title")},
        "findings": findings,
        "note": "This opt-in Lighthouse lab sample is not a WCAG conformance or legal-compliance assessment.",
    })
}

fn credential(host: &dyn Host, root: &Path, value: Option<&str>) -> Option<String> {
    if let Some(v) = value.filter(|v| !v.is_empty()) {
        return Some(v.to_owned());
    }
    credentials::read(host, root, "pagespeed/api_key", "PAGESPEED_API_KEY", "").ok()
}

fn cache_path(cache_dir: &Path, url: &str, strategy: &str) -> PathBuf {
    let digest = Sha256::digest(format!("{url}\0{strategy}").as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    cache_dir.join(format!("psi-v5-{hex}.json"))
}

fn write_cache(path: &Path, value: &Value) {
    let _ = path.parent().map(std::fs::create_dir_all);
    let _ = std::fs::write(path, pyjson::dumps_opts(value, true, true).as_bytes());
}

fn parse(body: &Value, url: &str, strategy: &str) -> Option<Value> {
    let lighthouse = body.get("lighthouseResult")?;
    if !lighthouse.is_object() {
        return None;
    }
    let categories = lighthouse.get("categories")?;
    let audits = lighthouse.get("audits")?;
    if !categories.is_object() || !audits.is_object() {
        return None;
    }
    let mut selected_categories = Map::new();
    for (name, category) in categories.as_object()? {
        if category.is_object() {
            selected_categories.insert(
                name.clone(),
                json!({"score": category.get("score"), "title": category.get("title")}),
            );
        }
    }
    let mut selected_audits = Map::new();
    for (name, audit) in audits.as_object()? {
        if audit.is_object() {
            selected_audits.insert(
                name.clone(),
                json!({
                    "score": audit.get("score"),
                    "display_value": audit.get("displayValue"),
                    "numeric_value": audit.get("numericValue"),
                    "score_display_mode": audit.get("scoreDisplayMode"),
                }),
            );
        }
    }
    let mut parsed = json!({
        "url": url,
        "strategy": strategy,
        "final_url": lighthouse.get("finalUrl"),
        "fetch_time": lighthouse.get("fetchTime"),
        "lighthouse_version": lighthouse.get("lighthouseVersion"),
        "categories": Value::Object(selected_categories),
        "audits": Value::Object(selected_audits),
        "lab_only": true,
        "note": "PSI lab samples are not CrUX field data, local browser timings, or a site score.",
    });
    let accessibility = accessibility_sample(&parsed);
    parsed
        .as_object_mut()
        .map(|m| m.insert("accessibility_sample".to_owned(), accessibility));
    Some(parsed)
}

/// `sample(urls, ...)`: collect bounded mobile-first PSI samples for the given URLs.
///
/// `api_key` falls back to `pagespeed/api_key`/`PAGESPEED_API_KEY`; without one the call returns
/// the `not_configured` envelope without touching the network. `cache_dir` enables the explicit
/// per-(url, strategy) JSON cache.
pub async fn sample<T: Transport>(
    transport: &T,
    host: &dyn Host,
    root: &Path,
    urls: &[Value],
    desktop: bool,
    api_key: Option<&str>,
    cache_dir: Option<&Path>,
) -> Result<Value, PagespeedError> {
    if urls.is_empty()
        || urls.len() > MAX_URLS
        || !urls.iter().all(|u| {
            u.as_str()
                .is_some_and(|s| s.starts_with("http://") || s.starts_with("https://"))
        })
    {
        return raise(
            "ValueError",
            format!("urls must contain 1..{MAX_URLS} absolute HTTP(S) URLs"),
        );
    }
    let Some(key) = credential(host, root, api_key) else {
        return Ok(json!({"ok": false, "state": "not_configured", "verified": false}));
    };
    let mut seen = Vec::<&Value>::new();
    for u in urls {
        if !seen.contains(&u) {
            seen.push(u);
        }
    }
    let mut samples = Vec::new();
    for url in &seen {
        let url = url.as_str().unwrap_or_default();
        for strategy in if desktop {
            vec!["mobile", "desktop"]
        } else {
            vec!["mobile"]
        } {
            let cache = cache_dir.map(|d| cache_path(d, url, strategy));
            if let Some(path) = cache.as_ref().filter(|p| p.is_file()) {
                let cached = std::fs::read_to_string(path)
                    .ok()
                    .and_then(|text| serde_json::from_str::<Value>(&text).ok());
                match cached {
                    Some(cached) if cached.is_object() => {
                        let mut hit = Map::new();
                        hit.insert("cache".to_owned(), json!("hit"));
                        hit.extend(cached.as_object().cloned().unwrap_or_default());
                        samples.push(Value::Object(hit));
                        continue;
                    }
                    Some(other) => {
                        return raise(
                            "TypeError",
                            format!("'{}' object is not a mapping", pystr::type_name(&other)),
                        );
                    }
                    None => {}
                }
            }
            let endpoint = format!(
                "{HOST}?{}",
                crate::http::urlencode(&[
                    ("url".to_owned(), url.to_owned()),
                    ("strategy".to_owned(), strategy.to_owned()),
                ])
            );
            let mut request = Request::new("GET", &endpoint).timeout(TIMEOUT);
            if !key.is_empty() {
                request = request.header("X-goog-api-key", &key);
            }
            let outcome: Result<Value, String> = match transport.send(&request).await {
                Ok(response) => match pystr::decode_utf8_strict(&response.body) {
                    Ok(text) => match pyjson::decode_error(&text) {
                        Some(decode_message) => Err(decode_message),
                        None => serde_json::from_str(&text).map_err(|e| e.to_string()),
                    },
                    Err(message) => Err(message),
                },
                Err(TransportError::Http(response)) => Err(pyerr::http_error(response.status)),
                Err(TransportError::Connect(kind)) => Err(pyerr::url_error(&kind)),
                Err(TransportError::InvalidRequest(message)) => Err(message),
            };
            let parsed = match outcome {
                Ok(body) => match parse(&body, url, strategy) {
                    Some(parsed) => parsed,
                    None => {
                        samples.push(json!({
                            "url": url,
                            "strategy": strategy,
                            "state": "failed",
                            "error": "malformed PSI v5 response",
                        }));
                        continue;
                    }
                },
                Err(message) => {
                    samples.push(json!({
                        "url": url,
                        "strategy": strategy,
                        "state": "failed",
                        "error": message,
                    }));
                    continue;
                }
            };
            if let Some(path) = &cache {
                write_cache(path, &parsed);
            }
            let mut entry = Map::new();
            entry.insert("cache".to_owned(), json!("miss"));
            entry.insert("state".to_owned(), json!("complete"));
            entry.extend(parsed.as_object().cloned().unwrap_or_default());
            samples.push(Value::Object(entry));
        }
    }
    let ok = samples
        .iter()
        .all(|s| s.get("state").and_then(Value::as_str) == Some("complete"));
    Ok(json!({
        "ok": ok,
        "state": if ok { "complete" } else { "partial" },
        "provider": "pagespeed",
        "samples": samples,
        "mobile_first": true,
        "cache": if cache_dir.is_some() { "explicit local cache" } else { "disabled" },
    }))
}

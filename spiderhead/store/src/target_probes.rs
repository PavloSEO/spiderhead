//! Bounded fetches of canonical, hreflang and pagination targets the crawl never captured (#987).
//!
//! A crawl records only what it fetched. A canonical or hreflang target that no link reached is
//! therefore unmeasured even when it answers 404 -- exactly the case CANONICAL_TARGET_ERROR and
//! HREFLANG_BROKEN_TARGET exist for. This module fetches such same-host targets once, after the
//! crawl, through the crawl's own no-follow client, and keeps one `context_items` row per target.
//! It records observations only; deciding whether an observation is a finding stays with the
//! audit. A target without a stored row is still unmeasured, never clean.

use crate::discovery_ledger::py_dumps;
use crate::error::StoreError;
use crate::head_links;
use crate::scan_artifact::ScanError;
use crate::structured_evidence;
use crate::url_links::hostname_of;
use indexmap::IndexMap;
use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};
use spiderhead_core::url_normalize::norm_url;
use spiderhead_parse::parser::meta::robots_directives;
use spiderhead_parse::py_url::urlsplit;
use std::collections::BTreeSet;

/// Context kind of one stored probe.
pub const KIND: &str = "target_probe";
/// Current payload version.
pub const SCHEMA_VERSION: &str = "target_probe.v2";
/// Legacy payload version, still readable.
pub const LEGACY_VERSION: &str = "target_probe.v1";
/// Hard cap that keeps a 1M-URL site from turning the post-crawl step into a second crawl;
/// targets past the cap stay unmeasured, which the audit reports as such.
pub const MAX_PROBES: i64 = 5000;

fn scan_err(message: &str) -> StoreError {
    StoreError::ScanError(ScanError {
        message: message.into(),
    })
}

/// Whether the port part of a netloc is absent or a valid port number.
fn port_ok(netloc: &str) -> bool {
    let host_port = netloc.rsplit_once('@').map_or(netloc, |(_, h)| h);
    let tail = match host_port.rfind(':') {
        Some(i) if !host_port[i..].contains(']') => &host_port[i + 1..],
        _ => return true,
    };
    tail.is_empty() || tail.parse::<u32>().is_ok_and(|p| p <= 65535)
}

/// Same-host http(s) URL with a readable port.
pub fn same_host(url: &str, host: &str) -> bool {
    let Ok(parts) = urlsplit(url) else {
        return false;
    };
    port_ok(&parts.netloc)
        && matches!(parts.scheme.as_str(), "http" | "https")
        && hostname_of(url).unwrap_or_default() == host
}

fn norm(url: &str) -> Result<String, StoreError> {
    norm_url(Some(url)).map_err(|e| StoreError::Invalid(e.to_string()))
}

/// Same-host canonical, hreflang and pagination targets that no captured page answers, keyed by norm.
pub fn candidate_targets(
    con: &Connection,
    host: &str,
) -> Result<IndexMap<String, String>, StoreError> {
    let mut captured = BTreeSet::new();
    {
        let mut stmt = con.prepare("SELECT u.url FROM pages p JOIN urls u USING(url_id)")?;
        for url in stmt.query_map([], |r| r.get::<_, String>(0))? {
            captured.insert(norm(&url?)?);
        }
    }
    let mut targets: IndexMap<String, String> = IndexMap::new();
    let mut offer = |url: &str| -> Result<(), StoreError> {
        if targets.len() as i64 >= MAX_PROBES || url.is_empty() || !same_host(url, host) {
            return Ok(());
        }
        let key = norm(url)?;
        if !captured.contains(&key) && !targets.contains_key(&key) {
            targets.insert(key, url.to_string());
        }
        Ok(())
    };
    {
        let mut stmt = con.prepare(
            "SELECT DISTINCT canonical FROM pages WHERE canonical IS NOT NULL AND canonical<>''",
        )?;
        for canonical in stmt.query_map([], |r| r.get::<_, String>(0))? {
            offer(&canonical?)?;
        }
    }
    for (_source, declarations) in head_links::active(con)? {
        for declaration in declarations.unwrap_or_default() {
            if let Some(target) = declaration.get("target").and_then(Value::as_str) {
                offer(target)?;
            }
        }
    }
    let (_, language) = structured_evidence::read_rows(con);
    for evidence in language.rows()? {
        for declaration in evidence
            .get("declarations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if declaration.is_object() {
                let target = match declaration.get("target") {
                    Some(Value::String(s)) => s.clone(),
                    None | Some(Value::Null) => String::new(),
                    Some(other) => other.to_string(),
                };
                offer(&target)?;
            }
        }
    }
    Ok(targets)
}

/// What a probe fetch observed (the fields `capture` reads from the crawl record).
#[derive(Debug, Clone, Default)]
pub struct ProbeRecord {
    /// HTTP status, when the target answered.
    pub status_code: Option<i64>,
    /// Redirect target the response named.
    pub redirect_url: Option<String>,
    /// Fetch error text.
    pub error: Option<String>,
    /// `<meta name=robots>` content.
    pub meta_robots: Option<String>,
    /// `X-Robots-Tag` value.
    pub x_robots: Option<String>,
    /// Canonical URL of the parsed page.
    pub canonical: Option<String>,
    /// The body was parsed as HTML.
    pub parsed: bool,
}

/// Seam for the crawl's no-follow fetch of one URL.
pub trait ProbeFetcher {
    /// Fetch `url` once; an `Err` carries the error class name.
    fn fetch(&self, url: &str, user_agent: &str) -> Result<ProbeRecord, String>;
}

/// Counters returned by [`capture`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProbeCounts {
    /// Targets fetched.
    pub probed: i64,
    /// Targets that answered a status.
    pub answered: i64,
    /// Targets that did not.
    pub failed: i64,
}

fn sorted_compact(value: &Value) -> String {
    py_dumps(value, true, false)
}

/// Probe every same-host uncaptured head-annotation target once and store the result.
/// `wait` counts a request against the scan budget; `Err(RequestBudgetExhausted)` stops the loop.
pub fn capture(
    con: &Connection,
    user_agent: &str,
    fetcher: &dyn ProbeFetcher,
    mut wait: Option<&mut dyn FnMut() -> Result<(), StoreError>>,
) -> Result<ProbeCounts, StoreError> {
    let start: Option<String> = con
        .query_row("SELECT start_url FROM scan WHERE singleton=1", [], |r| {
            r.get(0)
        })
        .ok()
        .flatten();
    let host = start.as_deref().and_then(hostname_of).unwrap_or_default();
    let mut counts = ProbeCounts::default();
    if host.is_empty() {
        return Ok(counts);
    }
    for url in candidate_targets(con, &host)?.values() {
        if let Some(wait) = wait.as_deref_mut() {
            match wait() {
                Err(StoreError::RequestBudgetExhausted(_)) => break,
                other => other?,
            }
        }
        let (status, payload) = match fetcher.fetch(url, user_agent) {
            Ok(record) => {
                let noindex = if robots_directives(&[
                    record.meta_robots.as_deref(),
                    record.x_robots.as_deref(),
                ])
                .iter()
                .any(|d| d == "noindex")
                {
                    json!(true)
                } else if record.parsed {
                    json!(false)
                } else {
                    Value::Null
                };
                let payload = json!({
                    "schema_version": SCHEMA_VERSION,
                    "url": url,
                    "status_code": record.status_code,
                    "redirect_url": record.redirect_url.clone().unwrap_or_default(),
                    "error": record.error.clone().unwrap_or_default(),
                    "noindex": noindex,
                    "canonical": if record.parsed { json!(record.canonical) } else { Value::Null },
                });
                (record.status_code, payload)
            }
            // One unreachable target must not stop the others.
            Err(kind) => (
                None,
                json!({
                    "schema_version": SCHEMA_VERSION, "url": url, "status_code": null,
                    "redirect_url": "", "error": kind, "noindex": null, "canonical": null,
                }),
            ),
        };
        counts.probed += 1;
        if status.is_some() {
            counts.answered += 1;
        } else {
            counts.failed += 1;
        }
        con.execute(
            "INSERT OR REPLACE INTO context_items(kind,item_key,payload_version,payload_json,completeness,reason) \
             VALUES(?,?,?,?,?,?)",
            params![
                KIND,
                format!("url:{}", norm(url)?),
                "scan_context.v1",
                sorted_compact(&payload),
                if status.is_some() { "complete" } else { "unavailable" },
                payload["error"].as_str().unwrap_or(""),
            ],
        )?;
    }
    Ok(counts)
}

fn is_int(value: Option<&Value>) -> bool {
    value.is_some_and(|v| v.is_i64() || v.is_u64())
}

/// Reject a stored probe that the native scan reader could not have written.
pub fn validate_context(item: &Map<String, Value>, payload: &Value) -> Result<(), StoreError> {
    let invalid = || scan_err("native target probe context is invalid");
    let object = payload.as_object();
    let version = object
        .and_then(|o| o.get("schema_version"))
        .and_then(Value::as_str);
    let status = object.and_then(|o| o.get("status_code"));
    let mut fields: BTreeSet<&str> = [
        "schema_version",
        "url",
        "status_code",
        "redirect_url",
        "error",
    ]
    .into();
    if version == Some(SCHEMA_VERSION) {
        fields.extend(["noindex", "canonical"]);
    }
    let Some(object) = object else {
        return Err(invalid());
    };
    let keys: BTreeSet<&str> = object.keys().map(String::as_str).collect();
    let status_ok = status.is_none_or(|s| s.is_null() || is_int(Some(s)));
    let v2_ok = version != Some(SCHEMA_VERSION)
        || ((object["noindex"].is_null() || object["noindex"].is_boolean())
            && (object["canonical"].is_null() || object["canonical"].is_string()));
    let error = object.get("error").and_then(Value::as_str);
    let expected_completeness = if status.is_some_and(|s| !s.is_null()) {
        "complete"
    } else {
        "unavailable"
    };
    let ok = keys == fields
        && matches!(version, Some(SCHEMA_VERSION) | Some(LEGACY_VERSION))
        && v2_ok
        && object["url"].as_str().is_some_and(|u| !u.is_empty())
        && status_ok
        && object["redirect_url"].is_string()
        && error.is_some()
        && item
            .get("item_key")
            .and_then(Value::as_str)
            .is_some_and(|k| k.starts_with("url:"))
        && item.get("completeness").and_then(Value::as_str) == Some(expected_completeness)
        && item.get("reason").and_then(Value::as_str) == error;
    if ok {
        Ok(())
    } else {
        Err(invalid())
    }
}

/// Stored probes keyed by normalized target URL; empty when no probe ran.
pub fn load(con: &Connection) -> Result<IndexMap<String, Map<String, Value>>, StoreError> {
    let mut stmt = con.prepare("SELECT item_key,payload_json FROM context_items WHERE kind=?")?;
    let rows = stmt.query_map(params![KIND], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut out = IndexMap::new();
    for row in rows {
        let (key, payload) = row?;
        let value: Value = serde_json::from_str(&payload)?;
        out.insert(
            key.strip_prefix("url:").unwrap_or(&key).to_string(),
            value.as_object().cloned().unwrap_or_default(),
        );
    }
    Ok(out)
}

/// Normalize authority like the HTTP client while retaining the analyzer URL policy:
/// `[scheme, host, port, userinfo, path, query, fragment]` (port `null` when it is the default).
pub fn canonical_identity(url: &str) -> Result<Vec<Value>, StoreError> {
    let normalized = norm(url)?;
    let parts = urlsplit(&normalized).map_err(|e| StoreError::Invalid(e.to_string()))?;
    let invalid = || StoreError::Invalid("invalid URL authority".into());
    let host = hostname_of(&normalized).unwrap_or_default();
    let host_port = parts
        .netloc
        .rsplit_once('@')
        .map_or(parts.netloc.as_str(), |(_, h)| h);
    let explicit_port = match host_port.rfind(':') {
        Some(i) if !host_port[i..].contains(']') && !host_port[i + 1..].is_empty() => {
            Some(host_port[i + 1..].parse::<u16>().map_err(|_| invalid())?)
        }
        _ => None,
    };
    let rebuilt = format!(
        "{}://{}{}",
        parts.scheme,
        if host.contains(':') {
            format!("[{host}]")
        } else {
            host.clone()
        },
        explicit_port.map_or(String::new(), |p| format!(":{p}")),
    );
    let authority = url::Url::parse(&rebuilt).map_err(|_| invalid())?;
    let userinfo = if parts.netloc.contains('@') {
        parts
            .netloc
            .rsplit_once('@')
            .map_or("", |(u, _)| u)
            .to_string()
    } else {
        String::new()
    };
    Ok(vec![
        json!(authority.scheme()),
        json!(authority.host_str().unwrap_or("")),
        json!(authority.port()),
        json!(userinfo),
        json!(parts.path),
        json!(parts.query),
        json!(parts.fragment),
    ])
}

fn state_reason(state: &str, reason: impl Into<String>) -> IndexMap<String, String> {
    IndexMap::from([
        ("state".to_string(), state.to_string()),
        ("reason".to_string(), reason.into()),
    ])
}

/// State exactly what the stored response and parsed directives establish.
pub fn indexability(payload: &Map<String, Value>) -> IndexMap<String, String> {
    let Some(status) = payload.get("status_code").and_then(Value::as_i64) else {
        return state_reason("unmeasured", "target probe has no HTTP status");
    };
    if !(200..300).contains(&status) {
        return state_reason("non_indexable", format!("HTTP {status}"));
    }
    match payload.get("noindex") {
        Some(Value::Bool(true)) => return state_reason("non_indexable", "noindex directive"),
        None | Some(Value::Null) => {
            return state_reason("unmeasured", "target probe did not measure HTML directives")
        }
        _ => {}
    }
    if let Some(canonical) = payload
        .get("canonical")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
    {
        let own = payload.get("url").and_then(Value::as_str).unwrap_or("");
        match (canonical_identity(canonical), canonical_identity(own)) {
            (Ok(a), Ok(b)) if a == b => {}
            (Ok(_), Ok(_)) => return state_reason("non_indexable", "canonicalized to another URL"),
            _ => {
                return state_reason(
                    "unmeasured",
                    "target probe canonical identity could not be normalized",
                )
            }
        }
    }
    state_reason(
        "indexable",
        "probed HTML has no noindex or cross-URL canonical",
    )
}

/// The target observation a probe supports, or `None` when it answered no status.
pub fn observation(payload: &Map<String, Value>) -> Option<Map<String, Value>> {
    let status = payload.get("status_code").and_then(Value::as_i64)?;
    let text = |k: &str| {
        payload
            .get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("")
            .to_string()
    };
    let mut out = Map::new();
    out.insert("state".into(), json!("probed"));
    out.insert("status_code".into(), json!(status));
    out.insert("redirect_url".into(), json!(text("redirect_url")));
    out.insert(
        "reason".into(),
        json!("fetched by the target probe; the target was not captured by the crawl"),
    );
    out.insert("probe_error".into(), json!(text("error")));
    out.insert(
        "canonical".into(),
        payload.get("canonical").cloned().unwrap_or(Value::Null),
    );
    out.insert("indexability".into(), json!(indexability(payload)));
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap_or_default()
    }

    fn item(completeness: &str, reason: &str) -> Map<String, Value> {
        obj(
            json!({"item_key": "url:https://e.test/a", "completeness": completeness, "reason": reason}),
        )
    }

    #[test]
    fn same_host_rules() {
        assert!(same_host("https://E.test/a", "e.test"));
        assert!(!same_host("https://other.test/a", "e.test"));
        assert!(!same_host("ftp://e.test/a", "e.test"));
        assert!(!same_host("https://e.test:99999/a", "e.test"));
        assert!(!same_host("https://e.test:abc/a", "e.test"));
    }

    #[test]
    fn validate_context_contract() {
        let good = json!({
            "schema_version": SCHEMA_VERSION, "url": "https://e.test/a", "status_code": 404,
            "redirect_url": "", "error": "", "noindex": null, "canonical": null,
        });
        validate_context(&item("complete", ""), &good).unwrap();
        let no_status = json!({
            "schema_version": SCHEMA_VERSION, "url": "https://e.test/a", "status_code": null,
            "redirect_url": "", "error": "ReadTimeout", "noindex": null, "canonical": null,
        });
        validate_context(&item("unavailable", "ReadTimeout"), &no_status).unwrap();
        assert!(validate_context(&item("complete", "ReadTimeout"), &no_status).is_err());
        assert!(validate_context(&item("unavailable", ""), &good).is_err());
        let mut legacy = good.clone();
        legacy["schema_version"] = json!(LEGACY_VERSION);
        assert!(validate_context(&item("complete", ""), &legacy).is_err()); // extra v2 keys
        let mut bad = good;
        bad["noindex"] = json!("no");
        assert!(validate_context(&item("complete", ""), &bad).is_err());
        assert!(validate_context(&item("complete", ""), &json!([])).is_err());
    }

    #[test]
    fn indexability_states() {
        let state = |v: Value| indexability(&obj(v))["state"].clone();
        assert_eq!(state(json!({})), "unmeasured");
        assert_eq!(state(json!({"status_code": 404})), "non_indexable");
        assert_eq!(
            state(json!({"status_code": 200, "noindex": true})),
            "non_indexable"
        );
        assert_eq!(
            state(json!({"status_code": 200, "noindex": null})),
            "unmeasured"
        );
        assert_eq!(
            state(
                json!({"status_code": 200, "noindex": false, "url": "https://e.test/a", "canonical": "https://e.test/b"})
            ),
            "non_indexable"
        );
        assert_eq!(
            state(
                json!({"status_code": 200, "noindex": false, "url": "https://E.test:443/a", "canonical": "https://e.test/a"})
            ),
            "indexable"
        );
        assert_eq!(
            indexability(&obj(json!({"status_code": 404})))["reason"],
            "HTTP 404"
        );
    }

    #[test]
    fn observation_shape() {
        assert!(observation(&obj(json!({"status_code": null}))).is_none());
        let o = observation(&obj(
            json!({"status_code": 301, "redirect_url": "/x", "error": "", "canonical": null}),
        ))
        .unwrap();
        assert_eq!(o["state"], json!("probed"));
        assert_eq!(o["redirect_url"], json!("/x"));
        assert_eq!(o["indexability"]["state"], json!("non_indexable"));
    }

    #[test]
    fn load_keys_by_normalized_url() {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(
            "CREATE TABLE context_items(kind TEXT,item_key TEXT,payload_version TEXT,payload_json TEXT,completeness TEXT,reason TEXT);
             INSERT INTO context_items VALUES('target_probe','url:https://e.test/a','scan_context.v1','{\"status_code\":200}','complete','');
             INSERT INTO context_items VALUES('other','url:x','v','{}','complete','');",
        )
        .unwrap();
        let loaded = load(&con).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded["https://e.test/a"]["status_code"], json!(200));
    }

    #[test]
    fn canonical_identity_normalises_authority() {
        let a = canonical_identity("HTTPS://Example.COM:443/a?b=1").unwrap();
        let b = canonical_identity("https://example.com/a?b=1").unwrap();
        assert_eq!(a, b);
        assert_ne!(
            a,
            canonical_identity("https://example.com:8443/a?b=1").unwrap()
        );
    }
}

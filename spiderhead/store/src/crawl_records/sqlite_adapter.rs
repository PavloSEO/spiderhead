//! Bounded native crawl adapter for an explicit SQLite scan artifact.

use serde_json::{Map, Value};
use spiderhead_core::crawl_settings::{request_headers, resolve_credential_headers};
use spiderhead_core::host::SystemHost;
use std::collections::BTreeMap;

use crate::frontier::urlsplit;

/// Lowercased host without userinfo, port or IPv6 brackets.
fn hostname_of(url: &str) -> String {
    let netloc = urlsplit(url).netloc;
    let host_port = netloc.rsplit_once('@').map_or(netloc.as_str(), |(_, h)| h);
    let host = if let Some(rest) = host_port.strip_prefix('[') {
        rest.split(']').next().unwrap_or("").to_string()
    } else {
        host_port.split(':').next().unwrap_or("").to_string()
    };
    host.to_lowercase()
}

/// Request headers for `url`: explicit request headers, then credential headers bound to its host.
/// `None` when both are empty.
pub fn headers(settings: &Value, url: &str) -> Option<BTreeMap<String, String>> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let to_text = |v: &Value| {
        v.as_str()
            .map(str::to_string)
            .unwrap_or_else(|| v.to_string())
    };
    if let Some(explicit) = request_headers(settings) {
        for (k, v) in explicit {
            out.insert(k, to_text(&v));
        }
    }
    let entries: Vec<Value> = settings
        .get("http")
        .and_then(|h| h.get("credential_headers"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let host = hostname_of(url);
    let credential: Map<String, Value> = resolve_credential_headers(&entries, &host, &SystemHost);
    for (k, v) in credential {
        out.insert(k, to_text(&v));
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

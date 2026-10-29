//! `inspect_urls` — bounded URL Inspection sample: a diagnostic, never an index census.

use serde_json::{json, Map, Value};
use spiderhead_core::host::Host;
use std::path::Path;

use super::{
    acquire_token, response_object, send, Fetch, GscError, ServiceAccount, INSPECTION_HOST,
    MAX_INSPECTION_URLS, MAX_INSPECTION_URL_BYTES,
};
use crate::http::Transport;

const INDEX_FIELDS: [(&str, usize); 9] = [
    ("verdict", 256),
    ("coverageState", 256),
    ("indexingState", 256),
    ("robotsTxtState", 256),
    ("pageFetchState", 256),
    ("googleCanonical", MAX_INSPECTION_URL_BYTES),
    ("userCanonical", MAX_INSPECTION_URL_BYTES),
    ("lastCrawlTime", 256),
    ("crawledAs", 256),
];

/// Cut text to `limit` characters, not bytes.
fn cut(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

fn malformed(url: &Value) -> Value {
    json!({
        "url": url,
        "state": "failed",
        "error": "Search Console malformed inspection response",
    })
}

fn transport_failed(url: &Value) -> Value {
    json!({
        "url": url,
        "state": "failed",
        "error": "Search Console inspection request failed",
    })
}

/// `inspect_urls` — inspect a declared bounded sample; every URL gets its own outcome.
pub async fn inspect_urls<T: Transport>(
    host: &dyn Host,
    root: &Path,
    site_url: &str,
    urls: Option<&[Value]>,
    token: Option<&Value>,
    service_account: ServiceAccount<'_>,
    transport: &T,
) -> Result<Value, GscError> {
    let urls_ok = urls
        .map(|list| {
            !list.is_empty()
                && list.len() <= MAX_INSPECTION_URLS
                && list.iter().all(|u| {
                    u.as_str()
                        .map(|s| !s.is_empty() && s.len() <= MAX_INSPECTION_URL_BYTES)
                        .unwrap_or(false)
                })
        })
        .unwrap_or(false);
    if site_url.is_empty() || !urls_ok {
        return Err(GscError::Invalid(format!(
            "site_url and 1..{MAX_INSPECTION_URLS} inspection URLs are required"
        )));
    }
    let urls = urls.unwrap_or_default();
    let (bearer, token_error) =
        acquire_token(host, root, token, transport, service_account).await?;
    let Some(bearer) = bearer else {
        return Ok(json!({
            "ok": false,
            "state": "not_configured",
            "verified": false,
            "requests_made": 0,
            "error": token_error,
        }));
    };
    let endpoint = format!("{INSPECTION_HOST}/urlInspection/index:inspect");
    let mut outcomes: Vec<Value> = Vec::new();
    let mut requests_made = 0usize;
    for url in urls {
        requests_made += 1;
        let url_text = url.as_str().unwrap_or_default();
        let payload = json!({"inspectionUrl": url_text, "siteUrl": site_url});
        let body = match send(transport, "POST", &endpoint, Some(&payload), &bearer).await {
            Fetch::Body(text) => response_object(&text),
            // Transport, size and decoding failures are recorded per URL; they do not abort the batch.
            Fetch::Http(_) | Fetch::TooLarge | Fetch::Connect(_) | Fetch::Decode(_) => {
                outcomes.push(transport_failed(url));
                continue;
            }
        };
        let inspection = body
            .as_ref()
            .and_then(|b| b.get("inspectionResult"))
            .filter(|v| v.is_object());
        let Some(inspection) = inspection else {
            outcomes.push(malformed(url));
            continue;
        };
        let index_status = inspection
            .get("indexStatusResult")
            .filter(|v| v.is_object());
        let verdict = index_status
            .and_then(|s| s.get("verdict"))
            .and_then(Value::as_str);
        let Some(index_status) = index_status else {
            outcomes.push(malformed(url));
            continue;
        };
        if !matches!(verdict, Some("PASS" | "FAIL" | "NEUTRAL")) {
            outcomes.push(malformed(url));
            continue;
        }
        let mut index_summary = Map::new();
        for (name, limit) in INDEX_FIELDS {
            match index_status.get(name) {
                Some(Value::String(text)) => {
                    index_summary.insert(name.to_owned(), json!(cut(text, limit)));
                }
                Some(v)
                    if name == "verdict"
                        && matches!(v.as_str(), Some("PASS" | "FAIL" | "NEUTRAL")) =>
                {
                    index_summary.insert(name.to_owned(), v.clone());
                }
                _ => {}
            }
        }
        let mut outcome = json!({
            "url": url_text,
            "state": "complete",
            "index_status": index_summary,
        });
        if let Some(amp) = inspection.get("ampResult").and_then(Value::as_object) {
            let mut amp_summary = Map::new();
            for name in [
                "verdict",
                "ampIndexStatusVerdict",
                "pageFetchState",
                "indexingState",
            ] {
                if let Some(text) = amp.get(name).and_then(Value::as_str) {
                    amp_summary.insert(name.to_owned(), json!(cut(text, 256)));
                }
            }
            outcome
                .as_object_mut()
                .map(|m| m.insert("amp_result".to_owned(), Value::Object(amp_summary)));
        }
        if let Some(rich) = inspection
            .get("richResultsResult")
            .and_then(Value::as_object)
        {
            let verdict_value = rich
                .get("verdict")
                .and_then(Value::as_str)
                .map(|v| json!(cut(v, 64)))
                .unwrap_or(Value::Null);
            let detected = match rich.get("detectedItems") {
                Some(Value::Array(items)) => items
                    .iter()
                    .take(50)
                    .filter_map(Value::as_object)
                    .map(|item| {
                        let rich_type = item
                            .get("richResultType")
                            .and_then(Value::as_str)
                            .map(|t| json!(cut(t, 128)))
                            .unwrap_or(Value::Null);
                        let count = match item.get("items") {
                            Some(Value::Array(list)) => list.len() as i64,
                            _ => 0,
                        };
                        json!({"type": rich_type, "items": count})
                    })
                    .collect::<Vec<Value>>(),
                _ => Vec::new(),
            };
            outcome.as_object_mut().map(|m| {
                m.insert(
                    "rich_results_result".to_owned(),
                    json!({"verdict": verdict_value, "detected_items": detected}),
                )
            });
        }
        outcomes.push(outcome);
    }
    let all_complete = outcomes
        .iter()
        .all(|o| o.get("state").and_then(Value::as_str) == Some("complete"));
    Ok(json!({
        "ok": all_complete,
        "state": if all_complete { "complete" } else { "partial" },
        "scope": "bounded URL diagnostic; not an index census",
        "requests_made": requests_made,
        "urls": outcomes,
    }))
}

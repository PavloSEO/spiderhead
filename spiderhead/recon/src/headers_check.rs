//! One URL's SEO-relevant response headers, transport version and timing.

use serde_json::{json, Value};

use crate::net_client::{http_client, ReconClient};

/// The headers the check reports, in report order.
const SEO_HEADERS: [&str; 14] = [
    "content-type",
    "x-robots-tag",
    "link",
    "cache-control",
    "expires",
    "content-encoding",
    "vary",
    "strict-transport-security",
    "content-length",
    "server",
    "last-modified",
    "etag",
    "location",
    "content-language",
];

/// Fetch one URL and report the SEO-relevant headers, protocol version,
/// timing and body size; transport failures are result data.
pub fn check_headers(url: &str, method: &str, timeout: f64) -> Value {
    let method = method.to_ascii_uppercase();
    if method != "GET" && method != "HEAD" {
        return json!({"ok": false, "url": url, "error": "headers-check method must be GET or HEAD"});
    }
    let Ok((client, http2_capable)) = http_client(timeout) else {
        return json!({"ok": false, "url": url, "error": "httpx is required"});
    };
    let started = std::time::Instant::now();
    let response = match client.request(&method, url, &[], &[]) {
        Ok(response) => response,
        Err(error) => {
            return json!({"ok": false, "url": url, "error": error.to_string()});
        }
    };
    let ttfb_ms = (started.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0;

    // Report order follows the fixed list; repeated headers combine with ", ".
    let mut seo = serde_json::Map::new();
    for name in SEO_HEADERS {
        let values: Vec<String> = response
            .header_list(name)
            .into_iter()
            .map(str::to_owned)
            .collect();
        if !values.is_empty() {
            seo.insert(name.to_string(), json!(values.join(", ")));
        }
    }
    let http_version = if response.http_version == "unmeasurable" {
        "?".to_string()
    } else {
        response.http_version.clone()
    };

    let mut findings: Vec<String> = Vec::new();
    let x_robots = response
        .header("x-robots-tag")
        .unwrap_or_default()
        .to_lowercase();
    if seo.contains_key("x-robots-tag")
        && (x_robots.contains("noindex") || x_robots.contains("none"))
    {
        findings.push("X-Robots-Tag blocks indexing (noindex/none)".to_owned());
    }
    if !http2_capable {
        findings.push("HTTP version not measurable: the h2 package is missing".to_owned());
    } else if http_version.starts_with("HTTP/1") {
        findings.push(format!("served over {http_version} (consider HTTP/2)"));
    }
    if !seo.contains_key("cache-control") {
        findings.push("no Cache-Control header".to_owned());
    }

    json!({
        "ok": true,
        "url": url,
        "final_url": response.url,
        "status_code": response.status_code,
        "http_version": http_version,
        "http_version_measurable": http2_capable,
        "ttfb_ms": ttfb_ms,
        "bytes": response.content.len(),
        "redirected": response.url != url,
        "seo_headers": Value::Object(seo),
        "findings": findings,
    })
}

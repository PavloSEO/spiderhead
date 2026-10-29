//! Detect the CDN in front of a site and observe actual cache behavior.
//!
//! Three requests: a regular GET, a repeated GET through the same client (MISS to HIT
//! warm-up), and a conditional request with `If-None-Match`/`If-Modified-Since`; plus a
//! Brotli probe.

use std::time::Instant;

use serde_json::{json, Map, Value as Json};

use crate::net_client::{http_client, normalize_url, ReconClient};

/// `(name, kind, marker)` signatures; the first match wins.
pub const CDN_SIGNATURES: &[(&str, &str, &str)] = &[
    ("Cloudflare", "header", "cf-ray"),
    ("Cloudflare", "server", "cloudflare"),
    ("Fastly", "header", "x-fastly-request-id"),
    ("Fastly", "value", "fastly"),
    ("Akamai", "header", "x-akamai-transformed"),
    ("Akamai", "server", "akamaighost"),
    ("Amazon CloudFront", "header", "x-amz-cf-id"),
    ("Vercel", "header", "x-vercel-id"),
    ("Netlify", "header", "x-nf-request-id"),
    ("Imperva Incapsula", "header", "x-iinfo"),
    ("Sucuri", "header", "x-sucuri-id"),
    ("BunnyCDN", "server", "bunnycdn"),
    ("KeyCDN", "server", "keycdn-engine"),
    ("Gcore", "server", "gcore"),
    ("Qrator", "server", "qrator"),
    ("DDoS-Guard", "server", "ddos-guard"),
    ("Azure CDN", "header", "x-azure-ref"),
    ("Alibaba Cloud CDN", "header", "x-swift-savetime"),
    ("Yandex Cloud CDN", "header", "x-yandex-req-id"),
    ("Google Cloud CDN", "value", "1.1 google"),
    ("StackPath", "header", "x-hw"),
];

/// Headers used by different CDNs to report cache status.
pub const CACHE_STATUS_HEADERS: &[&str] = &[
    "cf-cache-status",
    "x-cache",
    "x-cache-status",
    "x-proxy-cache",
    "x-vercel-cache",
    "cdn-cache",
    "x-served-by",
    "x-drupal-cache",
];
const HIT_WORDS: &[&str] = &["hit", "stale", "revalidated"];
const MISS_WORDS: &[&str] = &["miss", "expired", "bypass", "dynamic", "none"];

fn detect_cdn(headers: &Map<String, Json>) -> Option<String> {
    let server = headers
        .get("server")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();
    let via = ["via", "x-cache", "x-served-by"]
        .iter()
        .filter_map(|h| headers.get(*h).and_then(|v| v.as_str()))
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    for (name, kind, marker) in CDN_SIGNATURES {
        let hit = match *kind {
            "header" => headers.contains_key(*marker),
            "server" => server.contains(marker),
            "value" => via.contains(marker),
            _ => false,
        };
        if hit {
            return Some(name.to_string());
        }
    }
    None
}

fn cache_status(headers: &Map<String, Json>) -> Option<String> {
    CACHE_STATUS_HEADERS.iter().find_map(|header| {
        headers
            .get(*header)
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .map(|value| format!("{header}: {value}"))
    })
}

fn classify(status: Option<&str>) -> Option<&'static str> {
    let low = status?.to_lowercase();
    if HIT_WORDS.iter().any(|w| low.contains(w)) {
        Some("hit")
    } else if MISS_WORDS.iter().any(|w| low.contains(w)) {
        Some("miss")
    } else {
        Some("unknown")
    }
}

fn parse_cache_control(value: &str) -> Json {
    let mut directives = Map::new();
    for part in value.split(',') {
        let token = part.trim().to_lowercase();
        if token.is_empty() {
            continue;
        }
        let (key, val) = token.split_once('=').unwrap_or((token.as_str(), ""));
        directives.insert(
            key.trim().to_string(),
            if val.trim().is_empty() {
                json!(true)
            } else {
                json!(val.trim())
            },
        );
    }
    let max_age = directives
        .get("max-age")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<i64>().ok());
    json!({
        "raw": if value.is_empty() { Json::Null } else { json!(value) },
        "directives": Json::Object(directives.clone()),
        "max_age": max_age,
        "public": directives.contains_key("public"),
        "no_store": directives.contains_key("no-store"),
        "no_cache": directives.contains_key("no-cache"),
        "immutable": directives.contains_key("immutable"),
    })
}

/// Brotli probe: request `br` explicitly and read the negotiated encoding.
/// The probe fails silently.
fn brotli_probe(client: &dyn ReconClient, url: &str) -> Option<String> {
    let headers = [(
        "Accept-Encoding".to_string(),
        "br, gzip, deflate".to_string(),
    )];
    let resp = client.get(url, &headers, &[]).ok()?;
    resp.header("content-encoding").map(str::to_string)
}

fn revalidate(client: &dyn ReconClient, url: &str, headers: &Map<String, Json>) -> Json {
    let mut conditional: Vec<(String, String)> = Vec::new();
    if let Some(etag) = headers.get("etag").and_then(|v| v.as_str()) {
        conditional.push(("If-None-Match".to_string(), etag.to_string()));
    }
    if let Some(lm) = headers.get("last-modified").and_then(|v| v.as_str()) {
        conditional.push(("If-Modified-Since".to_string(), lm.to_string()));
    }
    if conditional.is_empty() {
        return json!({"supported": false, "reason": "neither ETag nor Last-Modified is available"});
    }
    let mut sent: Vec<&str> = conditional.iter().map(|(k, _)| k.as_str()).collect();
    sent.sort();
    match client.get(url, &conditional, &[]) {
        Ok(resp) => json!({
            "supported": resp.status_code == 304,
            "status_code": resp.status_code,
            "sent": sent,
        }),
        Err(exc) => json!({"supported": false, "reason": exc.to_string()}),
    }
}

/// Inspect CDN, cache, and transport behavior for one URL.
pub fn check_cdn(url: &str, timeout: f64) -> Json {
    let target = normalize_url(url);
    if target.is_empty() {
        return json!({"ok": false, "error": format!("not a valid URL: {}", crate::net_client::py_repr(url))});
    }
    let Ok((client, http2_capable)) = http_client(timeout) else {
        return json!({"ok": false, "error": "httpx is required"});
    };
    check_cdn_with(&client, &target, http2_capable)
}

/// [`check_cdn`] with an injected client.
pub fn check_cdn_with(client: &dyn ReconClient, target: &str, http2_capable: bool) -> Json {
    let started = Instant::now();
    let first = match client.get(target, &[], &[]) {
        Ok(resp) => resp,
        Err(error) => return json!({"ok": false, "url": target, "error": error.to_string()}),
    };
    let ttfb_first = (started.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0;
    let started = Instant::now();
    let second = match client.get(target, &[], &[]) {
        Ok(resp) => resp,
        Err(error) => return json!({"ok": false, "url": target, "error": error.to_string()}),
    };
    let ttfb_second = (started.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0;

    let head1: Map<String, Json> = first
        .headers
        .iter()
        .map(|(k, v)| (k.clone(), json!(v)))
        .collect();
    let head2: Map<String, Json> = second
        .headers
        .iter()
        .map(|(k, v)| (k.clone(), json!(v)))
        .collect();
    let revalidation = revalidate(client, target, &head1);
    let brotli = brotli_probe(client, target);

    let get = |m: &Map<String, Json>, k: &str| m.get(k).cloned().unwrap_or(Json::Null);
    let cache_control = parse_cache_control(
        head1
            .get("cache-control")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    );
    let first_status = cache_status(&head1);
    let second_status = cache_status(&head2);
    let hit_first = classify(first_status.as_deref());
    let hit_second = classify(second_status.as_deref());
    let alt_svc = get(&head1, "alt-svc");
    let content_encoding = get(&head1, "content-encoding");
    let http_version = first.http_version.clone();
    let cdn = detect_cdn(&head1);

    let mut result = json!({
        "ok": true,
        "url": target,
        "final_url": first.url,
        "status_code": first.status_code,
        "cdn": cdn,
        "server": get(&head1, "server"),
        "transport": {
            "http_version": http_version,
            "http_version_measurable": http2_capable,
            "http3_advertised": alt_svc.as_str().unwrap_or("").to_lowercase().contains("h3"),
            "alt_svc": alt_svc,
            "content_encoding": content_encoding,
            "brotli_supported": brotli.as_ref().map(|b| b == "br"),
            "brotli_probe": brotli,
            "ttfb_first_ms": ttfb_first,
            "ttfb_second_ms": ttfb_second,
        },
        "cache": {
            "cache_control": cache_control,
            "etag": get(&head1, "etag"),
            "last_modified": get(&head1, "last-modified"),
            "expires": get(&head1, "expires"),
            "vary": get(&head1, "vary"),
            "age": get(&head1, "age"),
            "status_first": first_status,
            "status_second": second_status,
            "hit_first": hit_first,
            "hit_second": hit_second,
            "warmed_up": hit_first == Some("miss") && hit_second == Some("hit"),
            "revalidation": revalidation,
        },
    });
    result["findings"] = json!(findings(&result));
    result
}

fn findings(result: &Json) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let cache = &result["cache"];
    let transport = &result["transport"];
    let cc = &cache["cache_control"];

    if cc["raw"].is_null() {
        out.push(
            "Cache-Control is missing; browsers and intermediaries must choose a policy"
                .to_string(),
        );
    } else if cc["no_store"].as_bool() == Some(true) {
        out.push("Cache-Control: no-store prevents the response from being cached".to_string());
    } else if cc["max_age"].as_i64() == Some(0) && cc["immutable"].as_bool() != Some(true) {
        out.push("max-age=0 requires every visit to contact the server".to_string());
    }

    let etag_missing = cache["etag"].is_null();
    let lm_missing = cache["last_modified"].is_null();
    if etag_missing && lm_missing {
        out.push("neither ETag nor Last-Modified is present for efficient revalidation".into());
    } else if cache["revalidation"]["supported"].as_bool() == Some(false)
        && !cache["revalidation"]["status_code"].is_null()
    {
        out.push(format!(
            "conditional request returned {} instead of 304; revalidation transfers the \
             response body again",
            cache["revalidation"]["status_code"]
        ));
    }

    if !result["cdn"].is_null() && cache["hit_first"].is_null() {
        out.push(format!(
            "{} is in front of the site, but exposes no cache status; external cache hits \
             cannot be verified",
            result["cdn"].as_str().unwrap_or("")
        ));
    }
    if cache["hit_second"].as_str() == Some("miss") {
        out.push("the repeated request was another MISS; the CDN did not retain the page".into());
    }

    if transport["http_version_measurable"].as_bool() != Some(true) {
        out.push(
            "protocol version is not measurable without the h2 package; install \
             seohead-seotools[all]"
                .to_string(),
        );
    } else if transport["http_version"]
        .as_str()
        .unwrap_or("")
        .starts_with("HTTP/1")
    {
        out.push(format!(
            "served over {}; HTTP/2 would provide multiplexing",
            transport["http_version"].as_str().unwrap_or("")
        ));
    }
    if transport["http3_advertised"].as_bool() != Some(true) {
        out.push("HTTP/3 is not advertised through Alt-Svc".to_string());
    }
    if transport["content_encoding"].is_null() {
        out.push("the response is uncompressed: neither gzip nor Brotli is enabled".into());
    } else if transport["brotli_supported"].is_null() {
        out.push("Brotli support could not be probed".into());
    } else if transport["brotli_supported"].as_bool() == Some(false) {
        out.push(format!(
            "using {} compression; Brotli is not enabled and is often 15-20% smaller",
            transport["content_encoding"].as_str().unwrap_or("")
        ));
    }
    if transport["ttfb_first_ms"].as_f64().unwrap_or(0.0) > 800.0 {
        out.push(format!(
            "TTFB is {} ms, indicating a slow server response",
            transport["ttfb_first_ms"]
        ));
    }
    out
}

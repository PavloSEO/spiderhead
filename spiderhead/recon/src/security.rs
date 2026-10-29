//! Review security headers, version disclosure, and cookies for an SEO audit.
//!
//! Not a penetration test: ordinary GET
//! requests inspect data the site already exposes. Probing service paths
//! (`.git/HEAD`, `.env`, …) is off unless `probe_paths` is explicitly set.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use indexmap::IndexMap;
use serde_json::{json, Value as Json};

use crate::net_client::{http_client, normalize_url, ReconClient, Response};

#[derive(serde::Deserialize)]
struct SecurityTables {
    /// Insertion order is the audit order; a sorted map would reorder
    /// the reported `headers_missing` list.
    security_headers: IndexMap<String, (u32, String)>,
    disclosure_headers: Vec<String>,
    sensitive_paths: Vec<String>,
    path_markers: BTreeMap<String, String>,
}

static TABLES: LazyLock<SecurityTables> = LazyLock::new(|| {
    serde_json::from_str(include_str!("data/security_tables.json"))
        .unwrap_or_else(|e| panic!("embedded security tables are invalid: {e}"))
});

/// Header → (score weight, purpose).
pub static SECURITY_HEADERS: LazyLock<IndexMap<String, (u32, String)>> =
    LazyLock::new(|| TABLES.security_headers.clone());
/// Version-bearing headers.
pub static DISCLOSURE_HEADERS: LazyLock<Vec<String>> =
    LazyLock::new(|| TABLES.disclosure_headers.clone());
/// Service paths commonly left exposed.
pub static SENSITIVE_PATHS: LazyLock<Vec<String>> =
    LazyLock::new(|| TABLES.sensitive_paths.clone());
/// Markers distinguishing exposed content from a custom 404.
pub static PATH_MARKERS: LazyLock<BTreeMap<String, String>> =
    LazyLock::new(|| TABLES.path_markers.clone());

fn grade(score: u32) -> &'static str {
    for (threshold, letter) in [(90, "A"), (75, "B"), (60, "C"), (40, "D"), (20, "E")] {
        if score >= threshold {
            return letter;
        }
    }
    "F"
}

fn check_https_redirect(client: &dyn ReconClient, domain: &str) -> Json {
    let http_url = format!("http://{domain}/");
    let resp = match client.get(&http_url, &[], &[]) {
        Ok(resp) => resp,
        Err(exc) => return json!({"checked": false, "reason": exc.to_string()}),
    };
    json!({
        "checked": true,
        "final_url": resp.url,
        "upgrades": resp.url.starts_with("https://"),
        "status_code": resp.status_code,
    })
}

fn cookie_flags(resp: &Response) -> Vec<Json> {
    resp.header_list("set-cookie")
        .iter()
        .map(|raw| {
            let low = raw.to_lowercase();
            let name = raw.split('=').next().unwrap_or("").trim().to_string();
            let mut same_site = None;
            for part in low.split(';') {
                if part.trim().starts_with("samesite=") {
                    same_site = part.split('=').nth(1).map(|s| s.trim().to_string());
                }
            }
            let attrs: Vec<String> = low.split(';').map(|p| p.trim().to_string()).collect();
            json!({
                "name": name,
                "secure": attrs.iter().any(|a| a == "secure"),
                "http_only": low.contains("httponly"),
                "same_site": same_site,
            })
        })
        .collect()
}

fn probe(client: &dyn ReconClient, base: &str) -> Vec<Json> {
    let mut exposed = Vec::new();
    for path in SENSITIVE_PATHS.iter() {
        let url = format!("{}{}", base.trim_end_matches('/'), path);
        let Ok(resp) = client.get(&url, &[], &[]) else {
            continue;
        };
        if resp.status_code != 200 {
            continue;
        }
        let marker = PATH_MARKERS.get(path);
        let body: String = resp.text().chars().take(400).collect();
        let looks_real = match marker {
            Some(marker) => body.contains(marker.as_str()),
            None => !resp
                .header("content-type")
                .unwrap_or("")
                .to_lowercase()
                .contains("html"),
        };
        if looks_real {
            exposed.push(json!({
                "path": path,
                "status_code": resp.status_code,
                "content_type": resp.header("content-type"),
                "bytes": resp.content.len(),
            }));
        }
    }
    exposed
}

/// Build a page security profile.
pub fn check_security(url: &str, probe_paths: bool, timeout: f64) -> Json {
    let target = normalize_url(url);
    if target.is_empty() {
        return json!({"ok": false, "error": format!("not a valid URL: {}", crate::net_client::py_repr(url))});
    }
    let Ok((client, _)) = http_client(timeout) else {
        return json!({"ok": false, "error": "httpx is required"});
    };
    check_security_with(&client, &target, probe_paths)
}

/// [`check_security`] with an injected client.
pub fn check_security_with(client: &dyn ReconClient, target: &str, probe_paths: bool) -> Json {
    let resp = match client.get(target, &[], &[]) {
        Ok(resp) => resp,
        Err(error) => return json!({"ok": false, "url": target, "error": error.to_string()}),
    };
    let headers: BTreeMap<String, String> = resp
        .headers
        .iter()
        .map(|(k, v)| (k.to_lowercase(), v.clone()))
        .collect();
    let cookies = cookie_flags(&resp);
    // reference passes `urlsplit(resp.url).netloc` — the authority keeps its port.
    let domain = url::Url::parse(&resp.url)
        .ok()
        .map(|u| match u.port() {
            Some(port) => format!("{}:{port}", u.host_str().unwrap_or_default()),
            None => u.host_str().unwrap_or_default().to_owned(),
        })
        .unwrap_or_default();
    let https = check_https_redirect(client, &domain);
    let exposed = probe_paths.then(|| probe(client, &resp.url));

    let mut present = serde_json::Map::new();
    let mut missing: Vec<Json> = Vec::new();
    let mut score = 0u32;
    // Iterate in table order so the JSON output keeps its order.
    for (header, (weight, purpose)) in SECURITY_HEADERS.iter() {
        if let Some(value) = headers.get(header.as_str()) {
            present.insert(
                header.clone(),
                json!(value.chars().take(300).collect::<String>()),
            );
            score += weight;
        } else if header == "x-frame-options"
            && headers
                .get("content-security-policy")
                .map(|v| v.to_lowercase().contains("frame-ancestors"))
                .unwrap_or(false)
        {
            present.insert(header.clone(), json!("covered by CSP frame-ancestors"));
            score += weight;
        } else {
            missing.push(json!({"header": header, "purpose": purpose}));
        }
    }

    let mut disclosure = serde_json::Map::new();
    for h in DISCLOSURE_HEADERS.iter() {
        if let Some(v) = headers.get(h.as_str()) {
            disclosure.insert(h.clone(), json!(v));
        }
    }

    let mut result = json!({
        "ok": true,
        "url": target,
        "final_url": resp.url,
        "status_code": resp.status_code,
        "score": score,
        "grade": grade(score),
        "headers_present": Json::Object(present),
        "headers_missing": missing,
        "version_disclosure": Json::Object(disclosure),
        "cookies": cookies,
        "https_redirect": https,
        "exposed_paths": exposed,
        "probed": probe_paths,
    });
    result["findings"] = json!(findings(&result));
    result
}

fn findings(result: &Json) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in result["headers_missing"].as_array().into_iter().flatten() {
        out.push(format!(
            "missing {}: {}",
            item["header"].as_str().unwrap_or(""),
            item["purpose"].as_str().unwrap_or("")
        ));
    }
    if let Some(map) = result["version_disclosure"].as_object() {
        for (header, value) in map {
            let value = value.as_str().unwrap_or("");
            if value.chars().any(|c| c.is_ascii_digit()) {
                out.push(format!("{header} discloses version information: {value}"));
            }
        }
    }
    let https = &result["https_redirect"];
    if https["checked"].as_bool() == Some(true) && https["upgrades"].as_bool() == Some(false) {
        out.push(
            "http does not redirect to https; some traffic and signals remain unencrypted"
                .to_string(),
        );
    }
    let insecure: Vec<&str> = result["cookies"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["secure"].as_bool() == Some(false))
        .filter_map(|c| c["name"].as_str())
        .collect();
    if !insecure.is_empty() {
        out.push(format!(
            "cookies without the Secure flag: {}",
            insecure
                .iter()
                .take(5)
                .copied()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let no_same_site: Vec<&str> = result["cookies"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["same_site"].is_null())
        .filter_map(|c| c["name"].as_str())
        .collect();
    if !no_same_site.is_empty() {
        out.push(format!(
            "cookies without SameSite: {}",
            no_same_site
                .iter()
                .take(5)
                .copied()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for item in result["exposed_paths"].as_array().into_iter().flatten() {
        out.push(format!(
            "publicly exposed path {} ({} bytes); restrict it immediately",
            item["path"].as_str().unwrap_or(""),
            item["bytes"].as_u64().unwrap_or(0)
        ));
    }
    out
}

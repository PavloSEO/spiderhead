//! Fetch a page and match its headers, HTML, cookies and scripts against technology fingerprints.

use super::*;

/// Fingerprints occur early; cap memory on oversized pages
pub fn max_html_bytes() -> usize {
    TABLES.max_html_bytes
}

pub(super) fn generator_re() -> &'static Regex {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?i)<meta[^>]+name=["']generator["'][^>]+content=["']([^"']+)"#)
            .unwrap_or_else(|e| panic!("generator regex: {e}"))
    });
    &RE
}

pub(super) fn version_re() -> &'static Regex {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(\d+\.\d+(?:\.\d+)?)").unwrap_or_else(|e| panic!("{e}")));
    &RE
}

/// Extract `<script src>` values through the DOM.
pub(super) fn script_sources(html: &str) -> Vec<String> {
    let soup = Soup::parse_document(html);
    let sel = scraper::Selector::parse("script[src]").unwrap_or_else(|e| panic!("{e}"));
    soup.select(&sel)
        .filter_map(|tag| tag.value().attr("src").map(str::to_string))
        .collect()
}

/// Human-readable evidence when a fingerprint matches.
pub fn _match(
    kind: &str,
    marker: &str,
    html_low: &str,
    headers: &BTreeMap<String, String>,
    cookies: &BTreeMap<String, String>,
    scripts_low: &str,
) -> Option<String> {
    let low = marker.to_lowercase();
    if kind == "header" && headers.contains_key(&low) {
        return Some(format!("header {marker}"));
    }
    if kind == "value" {
        for (name, value) in headers {
            if value.to_lowercase().contains(&low) {
                return Some(format!("{name}: {}", &value[..value.len().min(60)]));
            }
        }
    }
    if kind == "cookie" {
        for name in cookies.keys() {
            if name.to_lowercase().contains(&low) {
                return Some(format!("cookie {name}"));
            }
        }
    }
    if kind == "html" && html_low.contains(&low) {
        return Some(format!("HTML: {marker}"));
    }
    if kind == "script" && scripts_low.contains(&low) {
        return Some(format!("script: {marker}"));
    }
    None
}

/// Read response cookies without letting a malformed `Set-Cookie` abort the
/// fingerprint: a valueless name is kept with an empty value and reported
/// Returns `(cookies, malformed_names)`.
pub fn response_cookies(jar: &[Cookie]) -> (BTreeMap<String, String>, Vec<String>) {
    let mut cookies: BTreeMap<String, String> = BTreeMap::new();
    let mut malformed: Vec<String> = Vec::new();
    for cookie in jar {
        match &cookie.value {
            None => {
                malformed.push(cookie.name.clone());
                cookies.entry(cookie.name.clone()).or_default();
            }
            Some(value) => {
                cookies.insert(cookie.name.clone(), value.clone());
            }
        }
    }
    malformed.sort();
    malformed.dedup();
    (cookies, malformed)
}

/// Fetch one page and detect technologies with a single request
pub fn detect_tech(url: &str, timeout: f64) -> Json {
    let target = normalize_url(url);
    if target.is_empty() {
        return json!({"ok": false, "error": format!("Not a valid URL: {}", crate::net_client::py_repr(url))});
    }
    let Ok((client, _)) = http_client(timeout) else {
        return json!({"ok": false, "error": "httpx is required"});
    };
    detect_tech_with(&client, &target)
}

/// [`detect_tech`] with an injected client.
pub fn detect_tech_with(client: &dyn ReconClient, target: &str) -> Json {
    let resp = match client.get(target, &[], &[]) {
        Ok(resp) => resp,
        Err(error) => return json!({"ok": false, "url": target, "error": error.to_string()}),
    };
    let html: String = resp.text().chars().take(max_html_bytes()).collect();
    let (cookies, malformed) = response_cookies(&resp.cookies);
    let headers: BTreeMap<String, String> = resp
        .headers
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    analyze_tech(
        &html,
        &AnalyzeInput {
            headers,
            cookies,
            malformed_cookies: malformed,
            url: target.to_string(),
            final_url: Some(resp.url.clone()),
            status_code: Some(resp.status_code),
            rendered: false,
        },
    )
}

/// Inputs to [`analyze_tech`] beside the document itself.
#[derive(Default)]
pub struct AnalyzeInput {
    /// Response headers (names are lowercased inside).
    pub headers: BTreeMap<String, String>,
    /// Response cookies.
    pub cookies: BTreeMap<String, String>,
    /// Cookie names the site sent with no value.
    pub malformed_cookies: Vec<String>,
    /// Requested URL.
    pub url: String,
    /// Final URL after redirects (defaults to `url`).
    pub final_url: Option<String>,
    /// HTTP status code.
    pub status_code: Option<u16>,
    /// Whether `html` is a rendered DOM snapshot.
    pub rendered: bool,
}

/// Fingerprint an already-fetched document; makes zero network requests
pub fn analyze_tech(html: &str, input: &AnalyzeInput) -> Json {
    let headers: BTreeMap<String, String> = input
        .headers
        .iter()
        .map(|(k, v)| (k.to_lowercase(), v.clone()))
        .collect();
    let cookies = input.cookies.clone();
    let mut malformed = input.malformed_cookies.clone();
    malformed.sort();
    malformed.dedup();
    let html: String = html.chars().take(max_html_bytes()).collect();
    let page_url = input.final_url.clone().unwrap_or_else(|| input.url.clone());
    let html_low = html.to_lowercase();
    let scripts = script_sources(&html);
    let scripts_low = scripts.join(" ").to_lowercase();

    let mut found: BTreeMap<String, Json> = BTreeMap::new();
    for (category, name, kind, marker) in SIGNATURES.iter() {
        if let Some(hit) = _match(kind, marker, &html_low, &headers, &cookies, &scripts_low) {
            found
                .entry(name.clone())
                .or_insert_with(|| json!({"name": name, "category": category, "evidence": hit}));
        }
    }

    let generator = generator_re()
        .captures(&html)
        .map(|m| m[1].trim().to_string());
    if let Some(label) = &generator {
        found.entry(label.clone()).or_insert_with(
            || json!({"name": label, "category": "cms", "evidence": "meta name=generator"}),
        );
        let label_low = label.to_lowercase();
        let mut names: Vec<String> = Vec::new();
        for (n, entry) in &found {
            if entry["category"].as_str() == Some("cms") && label_low.contains(&n.to_lowercase()) {
                names.push(n.clone());
            }
        }
        for n in names {
            if let Some(ver) = version_re().captures(label) {
                if let Some(entry) = found.get_mut(&n) {
                    entry["version"] = json!(ver[1].to_string());
                }
            }
        }
    }

    if let Some(powered) = headers.get("x-powered-by") {
        let powered_low = powered.to_lowercase();
        let drop_names: Vec<String> = found
            .iter()
            .filter(|(n, e)| {
                e["category"].as_str() == Some("runtime")
                    && powered_low.starts_with(&n.to_lowercase())
            })
            .map(|(n, _)| n.clone())
            .collect();
        for n in drop_names {
            found.remove(&n);
        }
        found.entry(powered.clone()).or_insert_with(|| {
            json!({"name": powered, "category": "runtime", "evidence": format!("x-powered-by: {powered}")})
        });
    }

    // Optional user-supplied fingerprint database; built-in signatures win.
    // A configured database that fails reports `external_db_failed`, never the
    // plain "not configured" state (issue #240).
    let mut external_report = json!({"loaded": false});
    let configured_path = tech_db::get_external_db_path();
    match tech_db::detect_external(
        &html,
        &headers,
        &cookies,
        &scripts,
        &page_url,
        configured_path.as_deref(),
    ) {
        Ok(ext) if ext.get("db_loaded").and_then(|v| v.as_bool()) == Some(true) => {
            external_report = json!({
                "loaded": true,
                "path": ext.get("db_path"),
                "technologies_in_db": ext.get("technologies_count"),
            });
            for t in ext
                .get("technologies")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default()
            {
                let name = t["name"].as_str().unwrap_or("").to_string();
                found.entry(name).or_insert(t);
            }
        }
        Ok(_) => {}
        Err(exc) => {
            if let Some(path) = &configured_path {
                external_report = json!({
                    "loaded": false,
                    "state": "external_db_failed",
                    "path": path,
                    "error": exc,
                });
            }
        }
    }

    for (name, pattern) in IDENTIFIER_PATTERNS.iter() {
        if !found.contains_key(name) {
            continue;
        }
        let mut ids: Vec<String> = pattern
            .captures_iter(&html)
            .map(|m| m[1].to_string())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        ids.sort();
        if !ids.is_empty() {
            if let Some(entry) = found.get_mut(name) {
                entry["identifiers"] = json!(ids);
            }
        }
    }

    let mut by_category: BTreeMap<String, Vec<Json>> = BTreeMap::new();
    for entry in found.values() {
        by_category
            .entry(entry["category"].as_str().unwrap_or("").to_string())
            .or_default()
            .push(entry.clone());
    }
    for items in by_category.values_mut() {
        items.sort_by_key(|e| e["name"].as_str().unwrap_or("").to_lowercase());
    }

    let page_host = _host(&page_url);
    let third_party: Vec<String> = scripts
        .iter()
        .map(|s| _host(s))
        .filter(|h| !h.is_empty() && *h != page_host)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let mut technologies: Vec<Json> = found.into_values().collect();
    technologies.sort_by(|a, b| {
        (
            a["category"].as_str().unwrap_or(""),
            a["name"].as_str().unwrap_or("").to_lowercase(),
        )
            .cmp(&(
                b["category"].as_str().unwrap_or(""),
                b["name"].as_str().unwrap_or("").to_lowercase(),
            ))
    });

    let findings = findings(&by_category, &third_party, &malformed);
    let mut by_cat_json = Map::new();
    for (cat, items) in by_category {
        by_cat_json.insert(cat, Json::Array(items));
    }
    json!({
        "ok": true,
        "url": if input.url.is_empty() { page_url.clone() } else { input.url.clone() },
        "final_url": page_url,
        "status_code": input.status_code,
        "generator": generator,
        "technologies": technologies,
        "by_category": Json::Object(by_cat_json),
        "scripts_total": scripts.len(),
        "third_party_hosts": third_party,
        "malformed_cookies": malformed,
        "external_db": external_report,
        "measurement": {
            "representation": if input.rendered { "rendered_dom" } else { "static_markup" },
            "script_executed": input.rendered,
        },
        "findings": findings,
    })
}

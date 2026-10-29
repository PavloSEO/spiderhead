//! Classify regional URLs and discover regional links in page HTML.
//!
//! Pure functions over URLs and markup: no network access.

use super::*;

/// Classify a URL relative to the main host's regional structure
pub fn classify_url(url: &str, main_host: &str) -> Json {
    let normalized = normalize_url(url);
    let host = Url::parse(&normalized)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_lowercase()))
        .unwrap_or_default();
    let main_host = hostname_of(&normalize_url(main_host));
    let main_host = if main_host.is_empty() {
        main_host.to_string()
    } else {
        main_host
    };
    let main_base = registrable_domain(&main_host);
    let mut out = json!({"url": url, "host": host, "scheme": null, "region": null, "slug": null});
    if host.is_empty() {
        return out;
    }

    if registrable_domain(&host) != main_base {
        let label = registrable_domain(&host)
            .split('.')
            .next()
            .unwrap_or("")
            .to_string();
        out["scheme"] = json!("domain");
        out["region"] = detect_region(&label).map(Json::from).unwrap_or(Json::Null);
        out["slug"] = json!(label);
        return out;
    }

    let sub = if host != main_base {
        host[..host.len() - main_base.len()]
            .trim_matches('.')
            .to_string()
    } else {
        String::new()
    };
    let sub_head = sub.split('.').next().unwrap_or("").to_string();
    if !sub_head.is_empty() && !NON_REGION_HOSTS.contains(&sub_head) {
        if let Some(region) = detect_region(&sub_head) {
            out["scheme"] = json!("subdomain");
            out["region"] = json!(region);
            out["slug"] = json!(sub_head);
            return out;
        }
    }

    let path = Url::parse(&normalized)
        .ok()
        .map(|u| u.path().to_string())
        .unwrap_or_default();
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if let Some(head_seg) = segments.first() {
        let head = head_seg.to_lowercase();
        if !NON_REGION_PATHS.contains(&head) {
            if let Some(region) = detect_region(&head) {
                out["scheme"] = json!("folder");
                out["region"] = json!(region);
                out["slug"] = json!(head);
                return out;
            }
        }
    }

    out["scheme"] = if sub_head.is_empty() || NON_REGION_HOSTS.contains(&sub_head) {
        json!("main")
    } else {
        json!("subdomain")
    };
    out
}

/// Find regional links in a page (city switcher); pure, no network
pub fn discover_regional_links(html: &str, base_url: &str) -> Vec<Json> {
    let main_host = hostname_of(&normalize_url(base_url));
    let soup = parse_document(html);
    let resolve_from = document_base_url(DocumentSource::Tree(&soup), &normalize_url(base_url));
    let sel = scraper::Selector::parse("a[href]").unwrap_or_else(|e| panic!("{e}"));
    let mut seen: Vec<(String, Json)> = Vec::new();
    let mut keys: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for a in soup.select(&sel) {
        let href = a.value().attr("href").unwrap_or("").trim().to_string();
        if href.is_empty()
            || ["#", "javascript:", "mailto:", "tel:"]
                .iter()
                .any(|p| href.starts_with(p))
        {
            continue;
        }
        let absolute = Url::parse(&resolve_from)
            .ok()
            .and_then(|b| b.join(&href).ok())
            .map(|u| u.to_string())
            .unwrap_or(href.clone());
        let mut info = classify_url(&absolute, &main_host);
        if info["region"].is_null() {
            let anchor = get_text(&a, " ");
            let anchor = anchor.split_whitespace().collect::<Vec<_>>().join(" ");
            let text_region =
                detect_region_name(&anchor).or_else(|| detect_region(&anchor.to_lowercase()));
            let Some(text_region) = text_region else {
                continue;
            };
            info["region"] = json!(text_region);
            info["from_anchor_text"] = json!(true);
        }
        let path = Url::parse(&absolute)
            .ok()
            .map(|u| u.path().to_string())
            .unwrap_or_default();
        let key = format!(
            "{}|{}|{}",
            info["region"].as_str().unwrap_or(""),
            info["host"].as_str().unwrap_or(""),
            path
        );
        if !keys.contains(&key) {
            keys.insert(key);
            info["url"] = json!(absolute.split('#').next().unwrap_or("").to_string());
            seen.push((String::new(), info));
        }
    }
    seen.into_iter().map(|(_, v)| v).collect()
}

/// Normalized phone numbers for cross-region comparison.
pub(super) fn phones(html: &str) -> Vec<String> {
    let digits_re = Regex::new(r"\D").unwrap_or_else(|e| panic!("{e}"));
    let mut found: Vec<String> = tel_re()
        .find_iter(html)
        .map(|m| digits_re.replace_all(m.as_str(), "").to_string())
        .filter(|p| p.len() >= 10)
        .map(|p| p[p.len() - 10..].to_string())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    found.sort();
    found
}

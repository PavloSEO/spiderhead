//! Audit regional site structures built with subdomains, folders, or satellites.
//!
//! Three structural models — subdomain
//! (`msk.site.ru`), folder (`site.ru/msk/`), satellite (`site-msk.ru`) — are
//! not equivalent; the audit answers which versions exist, which model they
//! use, whether they are reachable, and whether they undermine one another.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{json, Map, Value as Json};
use spiderhead_parse::dom::{get_text, parse_document};
use spiderhead_parse::duplicate::{simhash, similarity};
use spiderhead_parse::parser::tokenize::{document_base_url, DocumentSource};
use spiderhead_parse::{parse_html, ParseOptions};
use url::Url;

use crate::net_client::{http_client, normalize_url, registrable_domain, ReconClient};

mod patterns;
pub use patterns::*;
mod classify;
pub use classify::*;
mod fetch;
use fetch::*;

#[derive(serde::Deserialize)]
struct RegionTables {
    region_slugs: BTreeMap<String, String>,
    region_names: BTreeMap<String, String>,
    non_region_hosts: Vec<String>,
    non_region_paths: Vec<String>,
}

static TABLES: LazyLock<RegionTables> = LazyLock::new(|| {
    serde_json::from_str(include_str!("data/regions_tables.json"))
        .unwrap_or_else(|e| panic!("embedded regions tables are invalid: {e}"))
});

/// URL token → city name.
pub static REGION_SLUGS: LazyLock<BTreeMap<String, String>> =
    LazyLock::new(|| TABLES.region_slugs.clone());
/// Normalized Russian city name → city name.
pub static REGION_NAMES: LazyLock<BTreeMap<String, String>> =
    LazyLock::new(|| TABLES.region_names.clone());
/// Infrastructure subdomains that are never regions.
pub static NON_REGION_HOSTS: LazyLock<Vec<String>> =
    LazyLock::new(|| TABLES.non_region_hosts.clone());
/// First path segments that must never be regions.
pub static NON_REGION_PATHS: LazyLock<Vec<String>> =
    LazyLock::new(|| TABLES.non_region_paths.clone());

static NAMES_BY_LENGTH: LazyLock<Vec<(String, String)>> = LazyLock::new(|| {
    let mut pairs: Vec<(String, String)> = REGION_NAMES
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    pairs.sort_by_key(|(key, _)| std::cmp::Reverse(key.len()));
    pairs
});

/// Build findings from measured facts.
fn findings(main: &Json, pages: &[Json], schemes: &BTreeMap<String, usize>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if pages.is_empty() {
        return vec![
            "No regional URLs were found in the HTML: the site may have no regional versions, \
             or its city switcher may be rendered by JavaScript. Satellites on separate \
             domains cannot be discovered from this page; provide them explicitly through \
             extra."
                .to_string(),
        ];
    }
    let main_host = hostname_of(
        main.get("final_url")
            .or_else(|| main.get("url"))
            .and_then(|u| u.as_str())
            .unwrap_or(""),
    );
    let live: Vec<&Json> = pages
        .iter()
        .filter(|p| p["ok"].as_bool() == Some(true) && p["status"].as_u64() == Some(200))
        .collect();

    let dead: Vec<&Json> = pages
        .iter()
        .filter(|p| p["ok"].as_bool() != Some(true) || p["status"].as_u64().unwrap_or(0) >= 400)
        .collect();
    if !dead.is_empty() {
        let names: Vec<String> = dead
            .iter()
            .take(5)
            .filter_map(|p| p["url"].as_str().map(str::to_string))
            .collect();
        out.push(format!(
            "{} regional URLs are unavailable or return errors: {}{}",
            dead.len(),
            names.join(", "),
            if dead.len() > 5 { " and others" } else { "" }
        ));
    }

    let to_main: Vec<&Json> = live
        .iter()
        .filter(|p| {
            p["redirected"].as_bool() == Some(true)
                && hostname_of(p["final_url"].as_str().unwrap_or("")) == main_host
                && hostname_of(p["url"].as_str().unwrap_or("")) != main_host
        })
        .copied()
        .collect();
    if !to_main.is_empty() {
        let names: Vec<String> = to_main
            .iter()
            .take(5)
            .filter_map(|p| p["url"].as_str().map(str::to_string))
            .collect();
        out.push(format!(
            "{} regional hosts redirect to the main site; these hosts do not provide \
             distinct regional pages and cannot rank as such: {}",
            to_main.len(),
            names.join(", ")
        ));
    }

    let mut canon_out: Vec<String> = Vec::new();
    for p in &live {
        let canon = p["canonical"].as_str().unwrap_or("");
        if canon.is_empty() {
            continue;
        }
        let final_url = p["final_url"].as_str().unwrap_or("");
        let resolved = Url::parse(final_url)
            .ok()
            .and_then(|b| b.join(canon).ok())
            .map(|u| u.to_string())
            .unwrap_or_else(|| canon.to_string());
        let c_host = hostname_of(&resolved);
        if !c_host.is_empty() && c_host != hostname_of(final_url) {
            canon_out.push(format!("{} → {}", p["url"].as_str().unwrap_or(""), canon));
        }
    }
    if !canon_out.is_empty() {
        out.push(format!(
            "Regional pages canonicalize to another host, effectively excluding themselves \
             from search results: {}",
            canon_out
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }

    let noindexed: Vec<String> = live
        .iter()
        .filter(|p| p["noindex"].as_bool() == Some(true))
        .filter_map(|p| p["url"].as_str().map(str::to_string))
        .collect();
    if !noindexed.is_empty() {
        out.push(format!(
            "Regional pages marked noindex: {}",
            noindexed
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    let hashed: Vec<(&Json, u64)> = live
        .iter()
        .filter(|p| p["word_count"].as_u64().unwrap_or(0) > 50)
        .map(|p| (*p, simhash(p["html"].as_str().unwrap_or(""), 4)))
        .collect();
    let mut twins: Vec<String> = Vec::new();
    for i in 0..hashed.len() {
        for j in (i + 1)..hashed.len() {
            let sim = similarity(hashed[i].1, hashed[j].1);
            if sim >= 0.95 {
                twins.push(format!(
                    "{} ≈ {} ({:.0}%)",
                    hashed[i].0["url"].as_str().unwrap_or(""),
                    hashed[j].0["url"].as_str().unwrap_or(""),
                    sim * 100.0
                ));
            }
        }
    }
    if !twins.is_empty() {
        out.push(format!(
            "Content matches across {} region pairs; the regional versions differ only by \
             URL: {}",
            twins.len(),
            twins.iter().take(5).cloned().collect::<Vec<_>>().join("; ")
        ));
    }

    let mut same_title: BTreeMap<String, usize> = BTreeMap::new();
    for p in &live {
        let title = p["title"].as_str().unwrap_or("");
        if !title.is_empty() {
            *same_title.entry(title.to_string()).or_default() += 1;
        }
    }
    let dupes = same_title.values().filter(|n| **n > 1).count();
    if dupes > 0 {
        out.push(format!(
            "The same title is used across multiple regions ({dupes} duplicates); the city \
             is not inserted into the title."
        ));
    }

    let no_region_in_title: Vec<String> = live
        .iter()
        .filter(|p| {
            let region = p["region"].as_str().unwrap_or("");
            let title = p["title"].as_str().unwrap_or("");
            !region.is_empty()
                && !title.is_empty()
                && !title
                    .to_lowercase()
                    .contains(&region.split('-').next().unwrap_or("").to_lowercase())
        })
        .filter_map(|p| p["url"].as_str().map(str::to_string))
        .collect();
    if !no_region_in_title.is_empty() {
        out.push(format!(
            "The city is missing from the title on {} pages, leaving search engines with no \
             title-level regional signal: {}",
            no_region_in_title.len(),
            no_region_in_title
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    let phone_sets: std::collections::BTreeSet<String> = live
        .iter()
        .filter_map(|p| p["phones"].as_array())
        .filter(|arr| !arr.is_empty())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join("|")
        })
        .collect();
    if phone_sets.len() == 1 && live.len() > 1 {
        out.push(
            "The phone number is identical across all regions. Yandex may treat this as \
             evidence that the branches are not distinct and the pages are templated."
                .to_string(),
        );
    }

    if schemes.get("subdomain").is_some() && schemes.get("folder").is_some() {
        out.push(
            "Regional versions use both subdomains and folders. Choose one model to prevent \
             the two structures from competing with each other."
                .to_string(),
        );
    }

    let satellites = schemes.get("domain").copied().unwrap_or(0);
    if satellites > 0 {
        out.push(format!(
            "{satellites} regional URLs use separate domains. Check for affiliate site \
             signals: Yandex may cluster domains with identical content and contact details \
             and retain only one in search results."
        ));
    }

    if out.is_empty() {
        out.push(format!(
            "Checked regions: {}; no major structural errors were found.",
            live.len()
        ));
    }
    out
}

/// Analyze a site's regional structure.
pub fn analyze_regions(
    url: &str,
    extra: Option<&[String]>,
    limit: i64,
    timeout: f64,
    render: bool,
) -> Json {
    if url.trim().is_empty() {
        return json!({"ok": false, "error": "URL is required"});
    }
    if limit < 1 {
        return json!({"ok": false, "error": "limit must be a number"});
    }
    let start = normalize_url(url);
    let main_host = hostname_of(&start);
    if main_host.is_empty() {
        return json!({"ok": false, "error": format!("Unable to parse URL: {}", crate::net_client::py_repr(url))});
    }
    let Ok((client, _)) = http_client(timeout) else {
        return json!({"ok": false, "error": "httpx is required"});
    };
    analyze_regions_with(&client, &start, &main_host, extra, limit, render)
}

/// [`analyze_regions`] with an injected client.
pub fn analyze_regions_with(
    client: &dyn ReconClient,
    start: &str,
    main_host: &str,
    extra: Option<&[String]>,
    limit: i64,
    render: bool,
) -> Json {
    let main = fetch(client, start);
    if main["ok"].as_bool() != Some(true) {
        return json!({
            "ok": false,
            "error": main["error"].as_str().unwrap_or("Homepage is unavailable"),
            "url": start,
        });
    }
    let switcher_html = main["html"].as_str().unwrap_or("").to_string();
    let render_note = if render {
        // Rendered HTML is unavailable here; record the degraded state.
        Some("Rendering failed (rendering is unavailable); only raw HTML was analyzed".to_string())
    } else {
        None
    };
    let mut candidates = discover_regional_links(&switcher_html, start);
    for raw in extra.into_iter().flatten() {
        let absolute = normalize_url(raw.trim());
        if absolute.is_empty() {
            continue;
        }
        let mut info = classify_url(&absolute, main_host);
        info["url"] = json!(absolute);
        info["from_input"] = json!(true);
        candidates.push(info);
    }

    let mut by_url: Map<String, Json> = Map::new();
    for c in candidates {
        let key = c["url"]
            .as_str()
            .unwrap_or("")
            .trim_end_matches('/')
            .to_string();
        by_url.entry(key).or_insert(c);
    }
    let mut regional: Vec<Json> = by_url
        .into_values()
        .filter(|c| !c["region"].is_null())
        .collect();
    regional.sort_by(|a, b| {
        (
            a["region"].as_str().unwrap_or(""),
            a["url"].as_str().unwrap_or(""),
        )
            .cmp(&(
                b["region"].as_str().unwrap_or(""),
                b["url"].as_str().unwrap_or(""),
            ))
    });
    let truncated = regional.len().saturating_sub(limit as usize);

    let mut pages: Vec<Json> = Vec::new();
    for c in regional.iter().take(limit.max(0) as usize) {
        let mut page = fetch(client, c["url"].as_str().unwrap_or(""));
        page["region"] = c["region"].clone();
        page["scheme"] = c["scheme"].clone();
        page["slug"] = c["slug"].clone();
        pages.push(page);
    }

    let mut schemes: BTreeMap<String, usize> = BTreeMap::new();
    for p in &pages {
        if let Some(s) = p["scheme"].as_str() {
            *schemes.entry(s.to_string()).or_default() += 1;
        }
    }
    let found = findings(&main, &pages, &schemes);
    let scheme = if schemes.len() == 1 {
        schemes.keys().next().cloned().unwrap_or_default()
    } else if schemes.is_empty() {
        "none".to_string()
    } else {
        "mixed".to_string()
    };

    for p in &mut pages {
        p.as_object_mut().map(|o| o.remove("html"));
    }
    let regions_found: Vec<String> = {
        let mut v: Vec<String> = pages
            .iter()
            .filter_map(|p| p["region"].as_str().map(str::to_string))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        v.sort();
        v
    };
    json!({
        "ok": true,
        "url": start,
        "main_host": main_host,
        "scheme": scheme,
        "schemes": schemes,
        "regions_found": regions_found,
        "regions_total": regional.len(),
        "checked": pages.len(),
        "truncated": truncated,
        "pages": pages,
        "rendered": render,
        "render_note": render_note,
        "findings": found,
    })
}

//! Turn raw technology matches into findings and aggregate them per site.

use super::*;

/// Host of a script/page URL, lowercased; `""` on parse failure.
pub fn _host(src: &str) -> String {
    let candidate = if src.contains("//") {
        src.to_string()
    } else {
        format!("//{src}")
    };
    url::Url::parse(&candidate)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_lowercase()))
        .unwrap_or_default()
}

/// Findings for an analyzed page.
pub(super) fn findings(
    by_category: &BTreeMap<String, Vec<Json>>,
    third_party: &[String],
    malformed_cookies: &[String],
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if !malformed_cookies.is_empty() {
        let noun = if malformed_cookies.len() == 1 {
            "header"
        } else {
            "headers"
        };
        out.push(format!(
            "The site sent {} Set-Cookie {noun} with no name=value pair ({}); each name was \
             matched with an empty value, and clients may discard such a header outright.",
            malformed_cookies.len(),
            malformed_cookies
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !by_category.contains_key("cms") && !by_category.contains_key("framework") {
        out.push(
            "No CMS or framework was detected; the site may be custom-built or rendered \
             entirely by JavaScript."
                .to_string(),
        );
    }
    if !by_category.contains_key("analytics") {
        out.push("No analytics integration was detected on the page.".to_string());
    }
    let pixels = by_category.get("pixel").map(|v| v.len()).unwrap_or(0);
    if pixels >= 3 {
        out.push(format!(
            "{pixels} advertising pixels are loaded on the page, adding transfer weight and \
             network requests."
        ));
    }
    if third_party.len() >= 10 {
        out.push(format!(
            "Scripts are loaded from {} third-party domains, which can increase performance \
             and availability risk.",
            third_party.len()
        ));
    }
    let ssr: Vec<String> = by_category
        .get("framework")
        .map(|v| {
            v.iter()
                .filter(|e| SSR_FRAMEWORKS.contains(e["name"].as_str().unwrap_or("")))
                .filter_map(|e| e["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if let Some(cms) = by_category.get("cms") {
        if !ssr.is_empty() {
            let names: Vec<String> = cms
                .iter()
                .filter_map(|e| e["name"].as_str().map(str::to_string))
                .chain(ssr)
                .collect();
            out.push(format!(
                "Detected both {}, suggesting a headless architecture; verify rendering and \
                 crawlability.",
                names.join(", ")
            ));
        }
    }
    out
}

/// Collapse a URL path to a template, masking id-shaped segments
pub fn _url_template(url: &str) -> String {
    let path = url::Url::parse(url)
        .ok()
        .map(|u| u.path().to_string())
        .unwrap_or_else(|| url.split(['?', '#']).next().unwrap_or("").to_string());
    let path = path.trim_matches('/');
    if path.is_empty() {
        return "/".to_string();
    }
    let parts: Vec<String> = path
        .split('/')
        .map(|seg| {
            if seg.chars().all(|c| c.is_ascii_digit()) || seg.len() >= 8 {
                "*".to_string()
            } else {
                seg.to_string()
            }
        })
        .collect();
    format!("/{}", parts.join("/"))
}

/// Aggregate per-page analysis results site-wide.
pub fn tag_coverage(pages: &[Json], tags: Option<&[String]>) -> Json {
    let owned: Vec<String>;
    let tags: &[String] = match tags {
        Some(t) => t,
        None => {
            owned = DEFAULT_COVERAGE_TAGS.clone();
            &owned
        }
    };
    let fetched: Vec<&Json> = pages
        .iter()
        .filter(|p| p.get("ok").and_then(|v| v.as_bool()) == Some(true))
        .collect();
    let mut stamps: BTreeSet<String> = BTreeSet::new();
    for p in &fetched {
        stamps.insert(
            p.get("measurement")
                .and_then(|m| m.get("representation"))
                .and_then(|r| r.as_str())
                .unwrap_or("static_markup")
                .to_string(),
        );
    }

    let mut by_template: BTreeMap<String, Vec<&Json>> = BTreeMap::new();
    for page in &fetched {
        let key = page
            .get("template")
            .and_then(|t| t.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| {
                _url_template(
                    page.get("url")
                        .or_else(|| page.get("final_url"))
                        .and_then(|u| u.as_str())
                        .unwrap_or(""),
                )
            });
        by_template.entry(key).or_default().push(*page);
    }

    let has_tech = |page: &Json, name: &str| -> Option<Json> {
        page.get("technologies")
            .and_then(|t| t.as_array())
            .and_then(|arr| {
                arr.iter()
                    .find(|t| t.get("name").and_then(|n| n.as_str()) == Some(name))
                    .cloned()
            })
    };

    let mut rows: Vec<Json> = Vec::new();
    for tag in tags {
        let mut identifiers: BTreeSet<String> = BTreeSet::new();
        let mut by_template_row = Map::new();
        let mut pages_with_tag = 0usize;
        for (template, group) in &by_template {
            let mut present = 0usize;
            let mut likely_injected = 0usize;
            for page in group {
                if let Some(entry) = has_tech(page, tag) {
                    present += 1;
                    for id in entry
                        .get("identifiers")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default()
                    {
                        if let Some(s) = id.as_str() {
                            identifiers.insert(s.to_string());
                        }
                    }
                } else {
                    let managers: &[String] = INJECTED_BY
                        .get(tag.as_str())
                        .map(|v| v.as_slice())
                        .unwrap_or(&[]);
                    if managers.iter().any(|m| has_tech(page, m).is_some()) {
                        likely_injected += 1;
                    }
                }
            }
            let n = group.len();
            by_template_row.insert(
                template.clone(),
                json!({
                    "pages": n,
                    "with_tag": present,
                    "fraction": if n > 0 { (present as f64 / n as f64 * 1e4).round() / 1e4 } else { 0.0 },
                    "likely_injected_by_manager": likely_injected,
                }),
            );
            pages_with_tag += present;
        }
        rows.push(json!({
            "tag": tag,
            "pages_with_tag": pages_with_tag,
            "fraction": if fetched.is_empty() { 0.0 } else { (pages_with_tag as f64 / fetched.len() as f64 * 1e4).round() / 1e4 },
            "identifiers": identifiers.iter().cloned().collect::<Vec<_>>(),
            "conflicting_identifiers": identifiers.len() > 1,
            "by_template": Json::Object(by_template_row),
        }));
    }

    let stamp_list: Vec<String> = stamps.into_iter().collect();
    let measurement_stamp = if stamp_list.len() == 1 {
        json!(stamp_list[0])
    } else {
        json!(stamp_list)
    };
    json!({
        "ok": true,
        "pages_considered": fetched.len(),
        "pages_excluded_fetch_failed": pages.len() - fetched.len(),
        "measurement_stamp": measurement_stamp,
        "tags": rows,
    })
}

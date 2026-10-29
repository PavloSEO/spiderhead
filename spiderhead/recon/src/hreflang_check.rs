//! Hreflang alternates review (`hreflang-check`): extract a page's
//! `link[rel=alternate][hreflang]` set and validate its codes, duplicates,
//! `x-default` and self-reference.

use std::collections::HashSet;

use serde_json::{json, Value as Json};
use spiderhead_core::url_normalize::norm_url;
use spiderhead_parse::dom::{attr, find_all, Soup};
use spiderhead_parse::hreflang_code::code_error;
use spiderhead_parse::parser::tokenize::{document_base_url, DocumentSource};
use spiderhead_parse::py_url::urljoin;

use crate::net_client::{http_client, py_repr, ReconClient};

/// Pure extraction of hreflang alternates from HTML: `{"hreflang", "href"}`
/// in document order, hrefs resolved against the document base.
pub fn extract_hreflang(html: &str, base_url: &str) -> Vec<Json> {
    let soup = Soup::parse_document(html);
    // Alternates are relative URLs like any other: resolve them from the base.
    let resolve_from = if base_url.is_empty() {
        String::new()
    } else {
        document_base_url(DocumentSource::Tree(&soup), base_url)
    };
    let mut out = Vec::new();
    for link in find_all(&soup, "link") {
        let rel = attr(&link, "rel").unwrap_or("").to_lowercase();
        let Some(hreflang) = attr(&link, "hreflang").filter(|h| !h.is_empty()) else {
            continue;
        };
        if !rel.contains("alternate") {
            continue;
        }
        let href = attr(&link, "href").unwrap_or("");
        let resolved = if resolve_from.is_empty() {
            href.to_string()
        } else {
            urljoin(&resolve_from, href).unwrap_or_else(|_| href.to_string())
        };
        out.push(json!({"hreflang": hreflang, "href": resolved}));
    }
    out
}

/// Review an alternate set: malformed codes, case-folded duplicates, a missing
/// `x-default`, and (when `page_url` is given) a self-reference in the set.
pub fn validate(alternates: &[Json], page_url: &str) -> Vec<String> {
    if alternates.is_empty() {
        return vec!["no hreflang annotations found".to_string()];
    }
    let langs: Vec<&str> = alternates
        .iter()
        .map(|a| a["hreflang"].as_str().unwrap_or(""))
        .collect();
    let mut issues = Vec::new();
    for lang in &langs {
        if let Some(reason) = code_error(lang) {
            issues.push(format!(
                "malformed hreflang code: {} ({reason})",
                py_repr(lang)
            ));
        }
    }
    // Language tags are case-insensitive, so "en-US" and "en-us" are the same
    // annotation twice, not two annotations.
    let folded: Vec<String> = langs.iter().map(|l| l.trim().to_lowercase()).collect();
    let mut dupes: Vec<&String> = folded
        .iter()
        .filter(|lang| folded.iter().filter(|c| *c == *lang).count() > 1)
        .collect();
    dupes.sort();
    dupes.dedup();
    for lang in dupes {
        issues.push(format!("duplicate hreflang: {lang}"));
    }
    if !folded.iter().any(|lang| lang == "x-default") {
        issues.push("no x-default alternate".to_string());
    }
    let hrefs: HashSet<String> = alternates
        .iter()
        .map(|a| norm_url(Some(a["href"].as_str().unwrap_or(""))).unwrap_or_default())
        .collect();
    if !page_url.is_empty() && !hrefs.contains(&norm_url(Some(page_url)).unwrap_or_default()) {
        issues.push("page does not self-reference in its hreflang set".to_string());
    }
    issues
}

/// Fetch `url` and review its hreflang set over the given client (parity seam).
pub fn check_hreflang_with(client: &dyn ReconClient, url: &str) -> Json {
    let response = match client.get(url, &[], &[]) {
        Ok(response) => response,
        Err(error) => {
            return json!({"ok": false, "url": url, "error": error.to_string()});
        }
    };
    let final_url = response.url.clone();
    let status = response.status_code;
    if !(200..300).contains(&status) {
        // An error page carries no hreflang, and "no hreflang annotations found"
        // is a statement about a page that was never served.
        return json!({
            "ok": false,
            "url": url,
            "final_url": final_url,
            "status_code": status,
            "error": format!("The page returned HTTP {status}, so it has no annotations to check"),
        });
    }
    let alternates = extract_hreflang(&response.text(), &final_url);
    json!({
        "ok": true,
        "url": url,
        "final_url": final_url,
        "status_code": status,
        "count": alternates.len(),
        "alternates": alternates,
        "issues": validate(&alternates, &final_url),
    })
}

/// Fetch `url` and review its hreflang set live.
pub fn check_hreflang(url: &str, timeout: f64) -> Json {
    match http_client(timeout) {
        Ok((client, _)) => check_hreflang_with(&client, url),
        Err(error) => json!({"ok": false, "url": url, "error": error.to_string()}),
    }
}

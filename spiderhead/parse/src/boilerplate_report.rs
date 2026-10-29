//! Report whether boilerplate (header/nav/footer) matches across a site.
//! Pure: SHA-1 over region markup.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::bs4_out::outer_html_without;
use crate::dom::{all_elements, parse_document};
use crate::duplicate::{content_hash, round4};
use crate::parser::shared::{collapse_whitespace, is_inert_template_content};
use crate::types::Result;

/// Tags hashed as boilerplate regions.
pub const BOILERPLATE_TAGS: [&str; 3] = ["header", "nav", "footer"];
/// Hash value of a page with no boilerplate regions.
pub const NO_BOILERPLATE_REGIONS: &str = "no_boilerplate_regions";

/// One page: `{"url","html"}` or `{"url","hash"}` when already hashed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoilerplatePage {
    /// Page URL.
    #[serde(default)]
    pub url: String,
    /// Raw HTML.
    #[serde(default)]
    pub html: Option<String>,
    /// Precomputed boilerplate hash.
    #[serde(default)]
    pub hash: Option<String>,
}

/// A group of pages sharing a hash.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoilerplateGroup {
    /// The hash.
    pub hash: String,
    /// Pages in the group.
    pub count: u64,
    /// `count / total` rounded to 4 places.
    pub fraction: f64,
    /// Whether this is the dominant template.
    pub dominant: bool,
    /// First URL seen.
    pub sample_url: String,
    /// Sorted URLs.
    pub urls: Vec<String>,
}

/// Result of [`boilerplate_consistency_report`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoilerplateReport {
    /// Always true.
    pub ok: bool,
    /// Pages compared.
    pub count: u64,
    /// Dominant hash, `None` when nothing was measured.
    pub dominant_hash: Option<String>,
    /// All groups by (-count, hash).
    pub groups: Vec<BoilerplateGroup>,
    /// Absent when the input is empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minority_groups: Option<Vec<BoilerplateGroup>>,
    /// Pages with no boilerplate regions (absent on the empty-input branch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_boilerplate_count: Option<u64>,
}

/// SHA-1 hex digest of a page's header/nav/footer markup (template content excluded).
pub fn boilerplate_hash(html: &str) -> String {
    let soup = parse_document(html);
    let pieces: Vec<String> = all_elements(&soup)
        .filter(|el| BOILERPLATE_TAGS.contains(&el.value().name()))
        .filter(|el| !is_inert_template_content(el))
        .map(|el| outer_html_without(&el, &["template"]))
        .collect();
    if pieces.is_empty() {
        return NO_BOILERPLATE_REGIONS.to_owned();
    }
    let basis: String = pieces.iter().map(|p| collapse_whitespace(p)).collect();
    content_hash(&basis)
}

/// Group pages by boilerplate hash and report the minority groups.
pub fn boilerplate_consistency_report(pages: &[BoilerplatePage]) -> Result<BoilerplateReport> {
    if pages.is_empty() {
        return Ok(BoilerplateReport {
            ok: true,
            count: 0,
            dominant_hash: None,
            groups: Vec::new(),
            minority_groups: None,
            no_boilerplate_count: None,
        });
    }
    let mut by_hash: Vec<(String, Vec<String>)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for page in pages {
        let hash = match page.hash.as_deref().filter(|h| !h.is_empty()) {
            Some(h) => h.to_owned(),
            None => boilerplate_hash(page.html.as_deref().unwrap_or("")),
        };
        let slot = *index.entry(hash.clone()).or_insert_with(|| {
            by_hash.push((hash, Vec::new()));
            by_hash.len() - 1
        });
        by_hash[slot].1.push(page.url.clone());
    }
    let total: usize = by_hash.iter().map(|(_, urls)| urls.len()).sum();
    // Pages without boilerplate regions were never compared, so that bucket cannot be dominant.
    let dominant_hash = by_hash
        .iter()
        .filter(|(h, _)| h != NO_BOILERPLATE_REGIONS)
        .max_by(|a, b| (a.1.len(), &a.0).cmp(&(b.1.len(), &b.0)))
        .map(|(h, _)| h.clone());
    let mut groups: Vec<BoilerplateGroup> = by_hash
        .iter()
        .map(|(hash, urls)| {
            let mut sorted = urls.clone();
            sorted.sort();
            BoilerplateGroup {
                hash: hash.clone(),
                count: urls.len() as u64,
                fraction: round4(urls.len() as f64 / total as f64),
                dominant: Some(hash) == dominant_hash.as_ref(),
                sample_url: urls[0].clone(),
                urls: sorted,
            }
        })
        .collect();
    groups.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.hash.cmp(&b.hash)));
    let no_boilerplate_count = by_hash
        .iter()
        .find(|(h, _)| h == NO_BOILERPLATE_REGIONS)
        .map_or(0, |(_, urls)| urls.len() as u64);
    let minority_groups = groups.iter().filter(|g| !g.dominant).cloned().collect();
    Ok(BoilerplateReport {
        ok: true,
        count: total as u64,
        dominant_hash,
        groups,
        minority_groups: Some(minority_groups),
        no_boilerplate_count: Some(no_boilerplate_count),
    })
}

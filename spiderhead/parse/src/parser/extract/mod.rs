//! Links, forms, text, URL sources, resource declarations and frames.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use fancy_regex::Regex;
use serde_json::json;

use super::shared::{
    collapse_whitespace, is_external, is_inert_template_content, URL_SOURCE_ATTRS,
};
use crate::content_area::{find_content_root, ContentRoot, TEXT_EXCLUDED_TAGS};
use crate::dom::{all_elements, find, find_all, get_text, ElementRef, Soup};
use crate::link_position::RuleSet;
use crate::models::{
    FormInfo, FrameInfo, InvalidLinkTarget, LinkInfo, LinkObservation, LinkPlacement,
    ResourceDeclaration, ResourceKind,
};
use crate::py_str::py_strip;
use crate::py_url::urlparse;
use crate::types::{ContentAreaConfig, Json, ParseError, ParseOptions, PositionRule, Result};
use crate::urlutil::urljoin;

mod extract_form_observations;
pub use extract_form_observations::*;

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|_| unreachable!("static pattern {pattern}"))
}

static CSS_URL: LazyLock<Regex> = LazyLock::new(|| re(r#"(?i)url\(\s*(['"]?)([^'")]+)\1\s*\)"#));
static HIDDEN_STYLE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)(?:display\s*:\s*none|visibility\s*:\s*hidden)"));
static META_REFRESH_URL: LazyLock<Regex> = LazyLock::new(|| re(r#"(?i)url\s*=\s*['"]?([^\s'"]+)"#));
static DISCLOSURE: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r"(?i)\b(?:sponsored|paid\s+(?:partnership|placement|post|content)|advertisement|",
        r"affiliate\s+links?|реклама|рекламный|партн[её]рский\s+материал)\b"
    ))
});

fn attr<'a>(tag: &ElementRef<'a>, name: &str) -> Option<&'a str> {
    tag.value().attr(name)
}

/// `tag.find_all(name)`: descendants only.
fn find_in<'a>(tag: &ElementRef<'a>, name: &'static str) -> impl Iterator<Item = ElementRef<'a>> {
    tag.descendants()
        .skip(1)
        .filter_map(ElementRef::wrap)
        .filter(move |e| e.value().name() == name)
}

/// Reason for an href that failed to parse.
pub const LINK_REASON_UNPARSABLE: &str = "unparsable_syntax";
/// Reason for an href with a non-fetchable scheme.
pub const LINK_REASON_UNSUPPORTED_SCHEME: &str = "unsupported_scheme";

/// Result of link observation: links, optional placement, rejected targets.
#[derive(Debug, Clone, Default)]
pub struct LinkObservations {
    /// Links in document order.
    pub links: Vec<LinkInfo>,
    /// Counts of stored/total/external/omitted links; present only when a cap was given.
    pub observation: Option<LinkObservation>,
    /// Placement counts; `None` when not measured.
    pub placement: Option<LinkPlacement>,
    /// Rejected hrefs (only when `record_invalid_targets`).
    pub invalid_targets: Vec<InvalidLinkTarget>,
}

/// Options of [`extract_link_observations`].
#[derive(Debug, Clone, Default)]
pub struct LinkObservationOptions<'a> {
    /// Classify each link's position.
    pub classify_links: bool,
    /// Content-area configuration.
    pub content_area_config: Option<&'a ContentAreaConfig>,
    /// Ordered position rules.
    pub position_rules: Option<&'a [PositionRule]>,
    /// Per-document cap.
    pub cap: Option<usize>,
    /// Record rejected hrefs.
    pub record_invalid_targets: bool,
}

/// Require a finite caller cap whenever declaration extraction is enabled.
/// (the other cap validation is a type guarantee).
pub fn resource_declaration_cap(options: &ParseOptions) -> Result<Option<usize>> {
    if !options.resource_declarations {
        return Ok(None);
    }
    match options.max_resource_declarations {
        Some(cap) => Ok(Some(cap)),
        None => Err(ParseError::Invalid(
            "max_resource_declarations must be a non-negative integer".to_owned(),
        )),
    }
}

/// The cheap facts to count a valid anchor — `(href, raw_href,
/// external)` — or the rejection reason (`""` = the anchor names no target at all).
pub fn link_target_reason(
    tag: &ElementRef<'_>,
    base_url: &str,
    final_url: &str,
) -> std::result::Result<(String, String, bool), &'static str> {
    if is_inert_template_content(tag) {
        return Err("");
    }
    let href_raw = py_strip(attr(tag, "href").unwrap_or(""));
    if href_raw.is_empty() || href_raw.starts_with('#') {
        return Err("");
    }
    let Ok(parts) = urlparse(href_raw) else {
        return Err(LINK_REASON_UNPARSABLE);
    };
    let scheme = parts.scheme.to_lowercase();
    if !scheme.is_empty() && scheme != "http" && scheme != "https" {
        return Err(LINK_REASON_UNSUPPORTED_SCHEME);
    }
    let Ok(href) = urljoin(base_url, href_raw) else {
        return Err(LINK_REASON_UNPARSABLE);
    };
    let external = is_external(&href, final_url);
    Ok((href, href_raw.to_owned(), external))
}

/// One `LinkInfo` for an accepted anchor (position when classifying, `anchor_source` when the
pub fn link_info(
    tag: &ElementRef<'_>,
    href: &str,
    href_raw: &str,
    external: bool,
    classify_links: bool,
    content_root: Option<&ContentRoot<'_>>,
    rules: Option<&[PositionRule]>,
) -> LinkInfo {
    let set = classify_links.then(|| RuleSet::new(rules));
    link_info_with(tag, href, href_raw, external, set.as_ref(), content_root)
}

fn link_info_with(
    tag: &ElementRef<'_>,
    href: &str,
    href_raw: &str,
    external: bool,
    rules: Option<&RuleSet>,
    content_root: Option<&ContentRoot<'_>>,
) -> LinkInfo {
    let rel_tokens: Vec<String> = attr(tag, "rel")
        .unwrap_or("")
        .split_whitespace()
        .map(str::to_lowercase)
        .collect();
    let text = collapse_whitespace(&get_text(tag, " "));
    let mut entry = LinkInfo {
        href: href.to_owned(),
        raw_href: href_raw.to_owned(),
        text: text.clone(),
        rel: rel_tokens.join(" "),
        target: py_strip(attr(tag, "target").unwrap_or("")).to_owned(),
        nofollow: rel_tokens.iter().any(|t| t == "nofollow"),
        external,
        position: None,
        anchor_source: None,
    };
    if text.is_empty() {
        for img in find_in(tag, "img") {
            let alt = collapse_whitespace(attr(&img, "alt").unwrap_or(""));
            if !alt.is_empty() {
                entry.text = alt;
                entry.anchor_source = Some("alt".to_owned());
                break;
            }
        }
    }
    if let Some(rules) = rules {
        entry.position = Some(rules.classify(tag, content_root));
    }
    entry
}

/// Bounded local text markers around a sponsored link; a review heuristic, never a legal
/// verdict. Shape: JSON object.
pub fn sponsorship_disclosure(tag: &ElementRef<'_>) -> Json {
    let block = tag
        .ancestors()
        .filter_map(ElementRef::wrap)
        .find(|a| {
            matches!(
                a.value().name(),
                "p" | "li" | "div" | "section" | "article" | "td" | "figure"
            )
        })
        .unwrap_or(*tag);
    let previous = block.prev_siblings().find_map(ElementRef::wrap);
    let roots: Vec<ElementRef<'_>> = previous.into_iter().chain([block]).collect();
    let hidden_style = &*HIDDEN_STYLE;
    let mut parts: Vec<String> = Vec::new();
    let mut size = 0usize;
    let mut truncated = false;
    for root in roots {
        let own = crate::dom::container_kind(root.value().name());
        for node in root.descendants() {
            let scraper::Node::Text(text) = node.value() else {
                continue;
            };
            let mut kind = None;
            let mut skip = false;
            for parent in node.ancestors().filter_map(ElementRef::wrap) {
                let el = parent.value();
                if TEXT_EXCLUDED_TAGS.contains(&el.name())
                    || el.attr("hidden").is_some()
                    || el.attr("aria-hidden").unwrap_or("").to_lowercase() == "true"
                    || hidden_style
                        .is_match(el.attr("style").unwrap_or(""))
                        .unwrap_or(false)
                {
                    skip = true;
                    break;
                }
                if kind.is_none() {
                    kind = crate::dom::container_kind(el.name());
                }
            }
            if skip || kind != own {
                continue;
            }
            let part = collapse_whitespace(text);
            let len = part.chars().count();
            if size + len > 500 {
                parts.push(
                    super::shared::char_prefix(&part, 500usize.saturating_sub(size)).to_owned(),
                );
                truncated = true;
                break;
            }
            size += len + 1;
            parts.push(part);
        }
        if truncated {
            break;
        }
    }
    let context = super::shared::char_prefix(&parts.join(" "), 500).to_owned();
    let marker = DISCLOSURE
        .find(&context)
        .ok()
        .flatten()
        .map(|m| m.as_str().to_owned());
    json!({
        "state": if truncated { "partial" } else { "measured" },
        "detected": marker.is_some(),
        "marker": marker,
        "context": context,
        "context_truncated": truncated,
        "basis": "local_text_marker",
        "heuristic": true,
    })
}

/// Collect `<a href>` links resolved against `base_url`, optionally with placement and rejected
/// targets.
pub fn extract_link_observations(
    soup: &Soup,
    base_url: &str,
    final_url: &str,
    options: &LinkObservationOptions<'_>,
) -> Result<LinkObservations> {
    let classify = options.classify_links && options.cap != Some(0);
    let (root, rules) = if classify {
        (
            Some(find_content_root(soup, options.content_area_config)?),
            Some(RuleSet::new(options.position_rules)),
        )
    } else {
        (None, None)
    };
    let mut out = LinkObservations::default();
    let (mut total, mut external_total) = (0u64, 0u64);
    for tag in find_all(soup, "a") {
        match link_target_reason(&tag, base_url, final_url) {
            Err(reason) => {
                if options.record_invalid_targets && !reason.is_empty() {
                    out.invalid_targets.push(InvalidLinkTarget {
                        raw_href: py_strip(attr(&tag, "href").unwrap_or("")).to_owned(),
                        reason: reason.to_owned(),
                    });
                }
            }
            Ok((href, raw, external)) => {
                total += 1;
                external_total += u64::from(external);
                if options.cap.is_none_or(|cap| out.links.len() < cap) {
                    out.links.push(link_info_with(
                        &tag,
                        &href,
                        &raw,
                        external,
                        rules.as_ref(),
                        root.as_ref(),
                    ));
                }
            }
        }
    }
    if options.cap.is_some() {
        let stored = out.links.len() as u64;
        out.observation = Some(LinkObservation {
            stored,
            total,
            external_total,
            omitted: total - stored,
        });
    }
    Ok(out)
}

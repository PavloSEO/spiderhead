//! Content-derived facts: headings, link placement, images, plugins, trust signals, ids.

use std::collections::{BTreeMap, HashMap};
use std::sync::LazyLock;

use fancy_regex::Regex;

use super::extract::link_target_reason;
use super::shared::{
    char_prefix, collapse_whitespace, is_inert_template_content, DUPLICATE_ID_CAP,
    DUPLICATE_ID_CHARS,
};
use crate::content_area::find_content_root;
use crate::dom::{all_elements, find_all, get_text, ElementRef, Soup};
use crate::link_position::{content_or_other, RuleSet};
use crate::models::{
    DuplicateId, HeadingLinkRef, HeadingRef, ImageInfo, ImageLinkRef, LinkPlacement, TrustSignal,
    TrustSignals,
};
use crate::py_str::py_strip;
use crate::py_url::urlparse;
use crate::types::{ContentAreaConfig, Json, ParseError, PositionRule, Result};

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|_| unreachable!("static pattern {pattern}"))
}

static HIDDEN_STYLE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)(?:^|;)\s*(?:display|visibility)\s*:\s*(?:none|hidden)\b"));
static LOREM: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)lorem\s+ipsum\s+dolor\s+sit\s+amet"));
static URL_PATH_DATE: LazyLock<Regex> =
    LazyLock::new(|| re(r"/(19|20)\d{2}(?:[-/](?:0?[1-9]|1[0-2])){1,2}(?:[-/]|$)"));
static AUTHOR_REL: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\bauthor\b"));
static BYLINE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\bbyline\b"));

fn attr<'a>(tag: &ElementRef<'a>, name: &str) -> Option<&'a str> {
    tag.value().attr(name)
}

/// `collapse_whitespace(tag.get_text(" "))`.
fn text_of(tag: &ElementRef<'_>) -> String {
    collapse_whitespace(&get_text(tag, " "))
}

static NULL: Json = Json::Null;

const NAMED_CONTENT_STRATEGIES: [&str; 5] = [
    "include_selector",
    "root_selector",
    "auto_main",
    "auto_role_main",
    "auto_article",
];

/// `tag.find_all(name)`: descendants only.
fn find_in<'a>(tag: &ElementRef<'a>, name: &'static str) -> impl Iterator<Item = ElementRef<'a>> {
    tag.descendants()
        .skip(1)
        .filter_map(ElementRef::wrap)
        .filter(move |e| e.value().name() == name)
}

/// Maximum items in each `LinkPlacement` list.
pub const MAX_PLACEMENT_ITEMS: usize = 50;
/// Maximum trust signals kept per list.
pub const TRUST_SIGNAL_CAP: usize = 8;
/// Maximum chars of a trust-signal value.
pub const TRUST_VALUE_CHARS: usize = 160;
/// Legacy plugin-dependent elements.
pub const PLUGIN_TAGS: [&str; 3] = ["object", "embed", "applet"];
/// Heading tags.
pub const HEADING_TAGS: [&str; 6] = ["h1", "h2", "h3", "h4", "h5", "h6"];

/// `{"h1": [...], ..., "h6": [...]}` for headings that have text.
pub fn extract_headings(soup: &Soup) -> BTreeMap<String, Vec<String>> {
    let mut headings = BTreeMap::new();
    for level in HEADING_TAGS {
        let found: Vec<String> = find_all(soup, level)
            .map(|tag| text_of(&tag))
            .filter(|text| !text.is_empty())
            .collect();
        if !found.is_empty() {
            headings.insert(level.to_owned(), found);
        }
    }
    headings
}

/// Whether `tag` belongs to a subtree hidden by document semantics (hidden, inert, aria-hidden,
/// closed dialog, explicit display/visibility style).
fn is_hidden_heading_content(tag: &ElementRef<'_>) -> bool {
    std::iter::once(*tag)
        .chain(tag.ancestors().filter_map(ElementRef::wrap))
        .any(|candidate| {
            let el = candidate.value();
            el.attr("hidden").is_some()
                || el.attr("inert").is_some()
                || py_strip(el.attr("aria-hidden").unwrap_or("")).to_lowercase() == "true"
                || (el.name() == "dialog" && el.attr("open").is_none())
                || el
                    .attr("style")
                    .is_some_and(|s| HIDDEN_STYLE.is_match(s).unwrap_or(false))
        })
}

/// Every h1-h6 with text in DOM order with level, text and page region; hidden/inert/closed
/// dialog subtrees excluded.
pub fn heading_outline(
    soup: &Soup,
    content_area_config: Option<&ContentAreaConfig>,
    position_rules: Option<&[PositionRule]>,
) -> Result<Vec<HeadingRef>> {
    let root = find_content_root(soup, content_area_config)?;
    let named_root = if NAMED_CONTENT_STRATEGIES.contains(&root.strategy.as_str()) {
        root.element
    } else {
        None
    };
    let rules = RuleSet::new(position_rules);
    let mut outline = Vec::new();
    for tag in all_elements(soup).filter(|e| HEADING_TAGS.contains(&e.value().name())) {
        if is_inert_template_content(&tag) || is_hidden_heading_content(&tag) {
            continue;
        }
        let text = text_of(&tag);
        if text.is_empty() {
            continue;
        }
        let mut region = rules.matched(&tag);
        if region.is_empty() {
            if let Some(named) = &named_root {
                region = content_or_other(&tag, named);
            }
        }
        outline.push(HeadingRef {
            level: tag.value().name().as_bytes()[1] - b'0',
            text,
            region,
        });
    }
    Ok(outline)
}

/// Alt text of an H1 whose own text is empty when an image supplies it.
pub fn h1_alt_only_text(soup: &Soup) -> Option<String> {
    for tag in find_all(soup, "h1") {
        if is_inert_template_content(&tag) || !text_of(&tag).is_empty() {
            continue;
        }
        for img in find_in(&tag, "img") {
            if is_inert_template_content(&img) {
                continue;
            }
            let alt = collapse_whitespace(attr(&img, "alt").unwrap_or(""));
            if !alt.is_empty() {
                return Some(alt);
            }
        }
    }
    None
}

/// A placement record with nothing in it (a document with no anchors).
pub fn empty_link_placement() -> LinkPlacement {
    LinkPlacement::default()
}

/// The level of the nearest `h1`-`h6` ancestor of `tag`.
fn heading_ancestor_level(tag: &ElementRef<'_>) -> Option<u8> {
    tag.ancestors()
        .filter_map(ElementRef::wrap)
        .find(|p| HEADING_TAGS.contains(&p.value().name()))
        .map(|p| p.value().name().as_bytes()[1] - b'0')
}

/// Whether this `<a>` says anything about where it goes (text, aria-label, title, image alt).
fn anchor_names_its_destination(tag: &ElementRef<'_>) -> bool {
    if !text_of(tag).is_empty() {
        return true;
    }
    for name in ["aria-label", "title"] {
        if attr(tag, name).is_some_and(|v| !collapse_whitespace(v).is_empty()) {
            return true;
        }
    }
    find_in(tag, "img")
        .any(|img| attr(&img, "alt").is_some_and(|v| !collapse_whitespace(v).is_empty()))
}

/// Anchors inside a heading and image links that name nothing.
pub fn link_placement(soup: &Soup, base_url: &str, final_url: &str) -> LinkPlacement {
    let mut placement = LinkPlacement::default();
    for tag in find_all(soup, "a") {
        let Ok((href, _, _)) = link_target_reason(&tag, base_url, final_url) else {
            continue;
        };
        if let Some(level) = heading_ancestor_level(&tag) {
            placement.in_heading_total += 1;
            if placement.in_heading.len() < MAX_PLACEMENT_ITEMS {
                placement.in_heading.push(HeadingLinkRef {
                    level,
                    destination: href.clone(),
                    anchor: text_of(&tag),
                });
            }
        }
        if find_in(&tag, "img").next().is_some() && !anchor_names_its_destination(&tag) {
            placement.image_no_text_total += 1;
            if placement.image_no_text.len() < MAX_PLACEMENT_ITEMS {
                placement
                    .image_no_text
                    .push(ImageLinkRef { destination: href });
            }
        }
    }
    placement
}

/// Every `<img>`'s alt evidence. The retained-analysis variant (`base_url` given) adds URL and
/// layout facts that `ImageInfo` cannot carry; only the ordinary projection is produced here.
pub fn extract_images(soup: &Soup, base_url: Option<&str>, max_images: usize) -> Vec<ImageInfo> {
    let _ = (base_url, max_images);
    find_all(soup, "img")
        .filter(|tag| !is_inert_template_content(tag))
        .map(|tag| {
            let alt = attr(&tag, "alt");
            ImageInfo {
                has_alt: alt.is_some(),
                alt_length: collapse_whitespace(alt.unwrap_or("")).chars().count() as u64,
            }
        })
        .collect()
}

/// Count of legacy plugin-dependent elements.
pub fn unsupported_plugin_count(soup: &Soup) -> u64 {
    let mut count = 0;
    for name in PLUGIN_TAGS {
        for tag in find_all(soup, name) {
            if is_inert_template_content(&tag) {
                continue;
            }
            if name == "object" {
                let kind = py_strip(attr(&tag, "type").unwrap_or("")).to_lowercase();
                let essence = py_strip(kind.split(';').next().unwrap_or(""));
                if essence.starts_with("image/") || essence == "application/pdf" {
                    continue;
                }
            }
            count += 1;
        }
    }
    count
}

/// Count of `<link rel=alternate media=...>` that name no URL.
pub fn mobile_alternate_broken_count(soup: &Soup) -> u64 {
    find_all(soup, "link")
        .filter(|tag| attr(tag, "media").is_some_and(|m| !m.is_empty()))
        .filter(|tag| {
            attr(tag, "rel")
                .unwrap_or("")
                .split_whitespace()
                .any(|t| t.to_lowercase() == "alternate")
        })
        .filter(|tag| !is_inert_template_content(tag))
        .filter(|tag| py_strip(attr(tag, "href").unwrap_or("")).is_empty())
        .count() as u64
}

/// Occurrences of the Lorem Ipsum passage in scoped content text.
pub fn count_lorem_ipsum(content_text: &str) -> u64 {
    LOREM.find_iter(content_text).flatten().count() as u64
}

const ARTICLE_JSONLD_TYPES: [&str; 16] = [
    "article",
    "newsarticle",
    "analysisnewsarticle",
    "askpublicnewsarticle",
    "backgroundnewsarticle",
    "opinionnewsarticle",
    "reportagenewsarticle",
    "reviewnewsarticle",
    "blogposting",
    "liveblogposting",
    "advertisercontentarticle",
    "satiricalarticle",
    "scholarlyarticle",
    "techarticle",
    "socialmediaposting",
    "report",
];
const DATE_META_NAMES: [&str; 13] = [
    "date",
    "dc.date",
    "dc.date.issued",
    "dcterms.created",
    "dcterms.modified",
    "datecreated",
    "datepublished",
    "datemodified",
    "publishdate",
    "publish-date",
    "pubdate",
    "last-modified",
    "article_date",
];
const TRUST_DATE_PROPS: [&str; 4] = ["datepublished", "datemodified", "datecreated", "uploaddate"];

/// Every object inside arbitrarily nested JSON-LD, in stack-walk order.
fn jsonld_node_walk(blocks: &[Json]) -> Vec<&serde_json::Map<String, Json>> {
    let mut out = Vec::new();
    let mut stack: Vec<&Json> = vec![];
    // The walk starts from the whole block list.
    let root_list: Vec<&Json> = blocks.iter().collect();
    stack.extend(root_list);
    while let Some(cur) = stack.pop() {
        match cur {
            Json::Object(map) => {
                out.push(map);
                stack.extend(map.values());
            }
            Json::Array(items) => stack.extend(items.iter()),
            _ => {}
        }
    }
    out
}

/// The first plain string a JSON-LD value carries (name, @id, list item).
fn ld_scalar(value: &Json) -> String {
    match value {
        Json::String(s) => s.clone(),
        Json::Object(map) => {
            for key in ["name", "@id", "url"] {
                if let Some(Json::String(inner)) = map.get(key) {
                    if !py_strip(inner).is_empty() {
                        return inner.clone();
                    }
                }
            }
            String::new()
        }
        Json::Array(items) => items
            .iter()
            .map(ld_scalar)
            .find(|s| !s.is_empty())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Objective authorship, date and article-scope markup facts.
pub fn extract_trust_signals(
    soup: &Soup,
    jsonld_blocks: &[Json],
    page_url: &str,
) -> Result<TrustSignals> {
    let mut author: Vec<TrustSignal> = Vec::new();
    let mut dates: Vec<TrustSignal> = Vec::new();
    let mut article: Vec<String> = Vec::new();

    fn add(family: &mut Vec<TrustSignal>, signal: &str, confidence: &str, value: &str) {
        if family.len() >= TRUST_SIGNAL_CAP {
            return;
        }
        family.push(TrustSignal {
            signal: signal.to_owned(),
            confidence: confidence.to_owned(),
            value: char_prefix(&collapse_whitespace(value), TRUST_VALUE_CHARS).to_owned(),
        });
    }

    for tag in find_all(soup, "meta") {
        if is_inert_template_content(&tag) {
            continue;
        }
        let Some(content) = attr(&tag, "content").filter(|c| !py_strip(c).is_empty()) else {
            continue;
        };
        let name = py_strip(attr(&tag, "name").unwrap_or("")).to_lowercase();
        let prop = py_strip(attr(&tag, "property").unwrap_or("")).to_lowercase();
        if name == "author" {
            add(&mut author, "meta_author", "high", content);
        } else if name == "article:author" || prop == "article:author" {
            add(&mut author, "meta_article_author", "high", content);
        }
        if prop == "article:published_time" {
            add(&mut dates, "meta_article_published", "high", content);
        } else if prop == "article:modified_time" {
            add(&mut dates, "meta_article_modified", "high", content);
        } else if prop == "og:updated_time" {
            add(&mut dates, "meta_og_updated", "high", content);
        } else if DATE_META_NAMES.contains(&name.as_str()) {
            add(&mut dates, &format!("meta_date:{name}"), "high", content);
        }
        if prop == "og:type" && py_strip(content).to_lowercase() == "article" {
            article.push("og_type".to_owned());
        }
    }

    let matches = |regex: &Regex, value: Option<&str>| {
        value.is_some_and(|v| regex.is_match(v).unwrap_or(false))
    };
    for tag in all_elements(soup).filter(|e| matches!(e.value().name(), "a" | "link")) {
        if !matches(&AUTHOR_REL, attr(&tag, "rel")) || is_inert_template_content(&tag) {
            continue;
        }
        let value = match attr(&tag, "href").filter(|h| !h.is_empty()) {
            Some(href) => href.to_owned(),
            None => text_of(&tag),
        };
        add(&mut author, "rel_author", "high", &value);
    }
    for tag in all_elements(soup) {
        if !matches(&AUTHOR_REL, attr(&tag, "itemprop")) || is_inert_template_content(&tag) {
            continue;
        }
        let value = attr(&tag, "content")
            .filter(|v| !v.is_empty())
            .or_else(|| attr(&tag, "href").filter(|v| !v.is_empty()))
            .map_or_else(|| text_of(&tag), str::to_owned);
        add(&mut author, "microdata_author", "high", &value);
    }
    for tag in all_elements(soup) {
        if !matches(&BYLINE, attr(&tag, "class")) || is_inert_template_content(&tag) {
            continue;
        }
        let text = text_of(&tag);
        if !text.is_empty() {
            add(&mut author, "byline_class", "medium", &text);
        }
    }
    for tag in all_elements(soup) {
        let Some(prop) = attr(&tag, "itemprop") else {
            continue;
        };
        if is_inert_template_content(&tag) {
            continue;
        }
        let prop = py_strip(prop).to_lowercase();
        if TRUST_DATE_PROPS.contains(&prop.as_str()) {
            let value = attr(&tag, "content")
                .filter(|v| !v.is_empty())
                .or_else(|| attr(&tag, "datetime").filter(|v| !v.is_empty()))
                .map_or_else(|| get_text(&tag, " "), str::to_owned);
            add(
                &mut dates,
                &format!("microdata_date:{prop}"),
                "high",
                &value,
            );
        }
    }
    for tag in find_all(soup, "time") {
        if let Some(datetime) = attr(&tag, "datetime") {
            if !is_inert_template_content(&tag) {
                add(&mut dates, "time_element", "medium", datetime);
            }
        }
    }

    for node in jsonld_node_walk(jsonld_blocks) {
        let declared: Vec<&Json> = match node.get("@type") {
            Some(Json::Array(items)) => items.iter().collect(),
            Some(other) => vec![other],
            None => vec![&NULL],
        };
        for item in declared {
            if let Json::String(kind) = item {
                if ARTICLE_JSONLD_TYPES.contains(&kind.to_lowercase().as_str()) {
                    let marker = format!("jsonld_type:{kind}");
                    if !article.contains(&marker) {
                        article.push(marker);
                    }
                }
            }
        }
        if let Some(found) = node.get("author").filter(|v| !v.is_null()) {
            add(&mut author, "jsonld_author", "high", &ld_scalar(found));
        }
        for (key, value) in node {
            if TRUST_DATE_PROPS.contains(&key.to_lowercase().as_str()) {
                add(
                    &mut dates,
                    &format!("jsonld_date:{key}"),
                    "high",
                    &ld_scalar(value),
                );
            }
        }
    }

    if find_all(soup, "article").any(|tag| !is_inert_template_content(&tag)) {
        article.push("article_element".to_owned());
    }

    let parsed = urlparse(page_url).map_err(|e| ParseError::Invalid(e.to_string()))?;
    if let Ok(Some(found)) = URL_PATH_DATE.find(&parsed.path) {
        add(
            &mut dates,
            "url_path_date",
            "medium",
            found.as_str().trim_matches(['/', '-']),
        );
    }

    Ok(TrustSignals {
        author,
        dates,
        article,
    })
}

/// Repeated literal `id` attributes in document order, bounded by `DUPLICATE_ID_CAP`.
pub fn extract_duplicate_ids(soup: &Soup) -> Vec<DuplicateId> {
    let mut order: Vec<String> = Vec::new();
    let mut counts: HashMap<String, u64> = HashMap::new();
    for tag in all_elements(soup) {
        let Some(id) = attr(&tag, "id").filter(|v| !v.is_empty()) else {
            continue;
        };
        if is_inert_template_content(&tag) {
            continue;
        }
        let id = char_prefix(id, DUPLICATE_ID_CHARS).to_owned();
        let count = counts.entry(id.clone()).or_insert(0);
        if *count == 0 {
            order.push(id);
        }
        *count += 1;
    }
    order
        .into_iter()
        .filter_map(|id| {
            let count = counts[&id];
            (count > 1).then_some(DuplicateId { id, count })
        })
        .take(DUPLICATE_ID_CAP)
        .collect()
}

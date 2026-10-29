//! Head metadata readers: title, meta tags, canonical, robots, hreflang, JSON-LD, AMP.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use fancy_regex::Regex;

use super::pyjson;
use super::shared::{
    char_prefix, collapse_whitespace, is_inert_template_content, rel_has, FOREIGN_CONTENT,
};
use crate::dom::{find, find_all, get_text, ElementRef, Soup};
use crate::models::{HreflangAlternate, JsonldInvalid};
use crate::py_str::py_strip;
use crate::types::{Json, Result};
use crate::urlutil::urljoin;

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|_| unreachable!("static pattern {pattern}"))
}

static CHARSET_META_RE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)<meta\b[^>]*>"));
static CHARSET_VALUE_RE: LazyLock<Regex> =
    LazyLock::new(|| re(r#"(?i)charset\s*=\s*["']?([a-zA-Z0-9_\-:.()]{2,})"#));
static ATTR_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(r#"([a-zA-Z_:][-a-zA-Z0-9_:.]*)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'=<>`]+))"#)
});
static DOCTYPE_RE: LazyLock<Regex> = LazyLock::new(|| re(r"(?is)<!DOCTYPE\b[^>]*>"));
static CANONICAL_REL_RE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?:^|[\s,;])[cC][aA][nN][oO][nN][iI][cC][aA][lL](?:$|[\s,;])"));

fn attr<'a>(tag: &ElementRef<'a>, name: &str) -> Option<&'a str> {
    tag.value().attr(name)
}

/// `_ci(name)` matcher on the `name` attribute: case-insensitive equality.
fn name_is(tag: &ElementRef<'_>, name: &str) -> bool {
    attr(tag, "name").is_some_and(|v| v.to_lowercase() == name.to_lowercase())
}

/// Robots-directive meta names, in reading order.
pub const ROBOTS_META_NAMES: [&str; 7] = [
    "robots",
    "googlebot",
    "googlebot-news",
    "bingbot",
    "msnbot",
    "yandex",
    "slurp",
];
/// Agents whose directives Google honours.
pub const GOOGLE_ROBOTS_AGENTS: [&str; 2] = ["googlebot", "googlebot-news"];
/// Directives that carry a value.
pub const VALUED_DIRECTIVES: [&str; 4] = [
    "max-snippet",
    "max-image-preview",
    "max-video-preview",
    "unavailable_after",
];
/// Early-window sizes of the raw charset/doctype scans.
pub const CHARSET_WINDOW_CHARS: usize = 1024;
/// Doctype scan window.
pub const DOCTYPE_WINDOW_CHARS: usize = 2048;
/// Chars of an invalid JSON-LD block kept as excerpt.
pub const JSONLD_EXCERPT_CHARS: usize = 200;

/// Charset named by an early `<meta charset>`/`http-equiv` tag in the first
/// [`CHARSET_WINDOW_CHARS`] of raw markup.
pub fn document_charset(html: &str) -> Option<String> {
    let window = char_prefix(html, CHARSET_WINDOW_CHARS);
    for tag_match in CHARSET_META_RE.find_iter(window).flatten() {
        let mut attrs: BTreeMap<String, String> = BTreeMap::new();
        for caps in ATTR_RE.captures_iter(tag_match.as_str()).flatten() {
            let name = caps.get(1).map_or("", |g| g.as_str()).to_lowercase();
            let value = (2..=4)
                .find_map(|i| caps.get(i))
                .map_or("", |g| g.as_str())
                .to_owned();
            attrs.insert(name, value);
        }
        if let Some(charset) = attrs.get("charset").filter(|v| !v.is_empty()) {
            return Some(charset.clone());
        }
        if attrs
            .get("http-equiv")
            .is_some_and(|v| v.to_lowercase() == "content-type")
        {
            let content = attrs.get("content").map_or("", String::as_str);
            if let Ok(Some(caps)) = CHARSET_VALUE_RE.captures(content) {
                return caps.get(1).map(|g| g.as_str().to_owned());
            }
        }
    }
    None
}

/// Raw `<!DOCTYPE ...>` text from the first [`DOCTYPE_WINDOW_CHARS`].
pub fn document_doctype(html: &str) -> Option<String> {
    let window = char_prefix(html, DOCTYPE_WINDOW_CHARS);
    let found = DOCTYPE_RE.find(window).ok().flatten()?;
    Some(py_strip(found.as_str()).to_owned())
}

/// True when the root `<html>` carries `amp` or `⚡`.
pub fn document_html_amp(soup: &Soup) -> bool {
    find(soup, "html")
        .is_some_and(|root| root.value().attr("amp").is_some() || root.value().attr("⚡").is_some())
}

/// First `<meta name=...>` (case-insensitive).
pub(crate) fn first_meta_tag<'a>(soup: &'a Soup, name: &str) -> Option<ElementRef<'a>> {
    find_all(soup, "meta").find(|tag| name_is(tag, name) && !is_inert_template_content(tag))
}

/// Count of live `<meta name=...>` tags with non-empty `content`.
pub(crate) fn live_meta_tag_count(soup: &Soup, name: &str) -> usize {
    find_all(soup, "meta")
        .filter(|tag| name_is(tag, name) && !is_inert_template_content(tag))
        .filter(|tag| attr(tag, "content").is_some_and(|c| !py_strip(c).is_empty()))
        .count()
}

/// `content` of `<meta name=...>` (case-insensitive).
pub(crate) fn meta_content(soup: &Soup, name: &str) -> Option<String> {
    let tag = first_meta_tag(soup, name)?;
    attr(&tag, "content").map(collapse_whitespace)
}

/// The tag [`document_title`] reads, ignoring SVG/MathML `<title>`.
pub(crate) fn title_tag(soup: &Soup) -> Option<ElementRef<'_>> {
    find_all(soup, "title").find(|tag| !crate::dom::has_ancestor_named(tag, &FOREIGN_CONTENT))
}

/// The HTML document title, ignoring SVG/MathML `<title>`.
pub fn document_title(soup: &Soup) -> Option<String> {
    let tag = title_tag(soup)?;
    Some(collapse_whitespace(&get_text(&tag, ""))).filter(|t| !t.is_empty())
}

/// First real `<link rel=canonical>`.
pub(crate) fn canonical_tag(soup: &Soup) -> Option<ElementRef<'_>> {
    find_all(soup, "link")
        .find(|tag| rel_has(attr(tag, "rel"), "canonical") && !is_inert_template_content(tag))
}

/// Raw canonical/pagination attributes beside their resolved targets, bounded per page
/// (100).
pub fn extract_head_links(soup: &Soup, base_url: &str) -> Vec<BTreeMap<String, String>> {
    let mut declarations = Vec::new();
    for tag in find_all(soup, "link") {
        if is_inert_template_content(&tag) {
            continue;
        }
        let rel = attr(&tag, "rel").unwrap_or("");
        let candidate = CANONICAL_REL_RE.is_match(rel).unwrap_or(false);
        if !candidate && !(rel_has(Some(rel), "next") || rel_has(Some(rel), "prev")) {
            continue;
        }
        let href = attr(&tag, "href").unwrap_or("");
        let target = if py_strip(href).is_empty() {
            String::new()
        } else {
            urljoin(base_url, py_strip(href)).unwrap_or_default()
        };
        let mut declaration = BTreeMap::new();
        declaration.insert("rel".to_owned(), rel.to_owned());
        declaration.insert("href".to_owned(), href.to_owned());
        declaration.insert("target".to_owned(), target);
        for name in ["hreflang", "lang", "media", "type"] {
            if let Some(value) = attr(&tag, name) {
                declaration.insert(name.to_owned(), value.to_owned());
            }
        }
        declarations.push(declaration);
        if declarations.len() == 100 {
            break;
        }
    }
    declarations
}

/// Every non-empty robots-directive `<meta>`, document order.
pub(crate) fn robots_directive_tags(soup: &Soup) -> Vec<ElementRef<'_>> {
    find_all(soup, "meta")
        .filter(|tag| !is_inert_template_content(tag))
        .filter(|tag| {
            attr(tag, "name")
                .is_some_and(|n| ROBOTS_META_NAMES.contains(&n.to_lowercase().as_str()))
        })
        .filter(|tag| attr(tag, "content").is_some_and(|c| !py_strip(c).is_empty()))
        .collect()
}

/// `(name, content)` of every robots-directive meta, document order.
pub fn robots_meta_entries(soup: &Soup) -> Vec<(String, String)> {
    robots_directive_tags(soup)
        .iter()
        .map(|tag| {
            (
                py_strip(attr(tag, "name").unwrap_or("")).to_lowercase(),
                collapse_whitespace(attr(tag, "content").unwrap_or("")),
            )
        })
        .collect()
}

/// `content` of every robots-directive meta, document order.
pub fn robots_meta_values(soup: &Soup) -> Vec<String> {
    robots_meta_entries(soup)
        .into_iter()
        .map(|(_, c)| c)
        .collect()
}

/// Robots meta content with the agent scope kept (`"<name>: "` prefix except for `robots`).
pub fn robots_meta_scoped(soup: &Soup) -> Vec<String> {
    robots_meta_entries(soup)
        .into_iter()
        .map(|(name, content)| {
            if name == "robots" {
                content
            } else {
                format!("{name}: {content}")
            }
        })
        .collect()
}

/// Every `<link rel="alternate" hreflang="...">` tag, document order.
pub(crate) fn hreflang_tags(soup: &Soup) -> Vec<ElementRef<'_>> {
    find_all(soup, "link")
        .filter(|tag| attr(tag, "hreflang").is_some_and(|h| !h.is_empty()))
        .filter(|tag| rel_has(attr(tag, "rel"), "alternate"))
        .collect()
}

/// First `<meta http-equiv=refresh>` content as written, `""` when none.
pub fn meta_refresh_content(soup: &Soup) -> String {
    find_all(soup, "meta")
        .filter(|meta| !is_inert_template_content(meta))
        .find(|meta| py_strip(attr(meta, "http-equiv").unwrap_or("")).to_lowercase() == "refresh")
        .map(|meta| collapse_whitespace(attr(&meta, "content").unwrap_or("")))
        .unwrap_or_default()
}

/// First live `<meta name=fragment>` content as written.
pub fn meta_fragment_content(soup: &Soup) -> String {
    first_meta_tag(soup, "fragment")
        .map(|tag| collapse_whitespace(attr(&tag, "content").unwrap_or("")))
        .unwrap_or_default()
}

/// Split robots directive strings into lower-case tokens, Google-effective only.
pub fn robots_directives(values: &[Option<&str>]) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut add = |token: String| {
        if !tokens.contains(&token) {
            tokens.push(token);
        }
    };
    for value in values.iter().map(|v| v.unwrap_or("")) {
        for raw in value.replace(';', ",").split(',') {
            let mut token = py_strip(raw).to_lowercase();
            if token.contains(':') && !VALUED_DIRECTIVES.iter().any(|d| token.starts_with(d)) {
                let (agent, rest) = token.split_once(':').unwrap_or((&token, ""));
                if !GOOGLE_ROBOTS_AGENTS.contains(&py_strip(agent)) {
                    continue;
                }
                token = py_strip(rest).to_owned();
            }
            if token.is_empty() {
                continue;
            }
            if token == "none" {
                add(token);
                add("noindex".to_owned());
                add("nofollow".to_owned());
            } else {
                add(token);
            }
        }
    }
    tokens
}

/// Every `<link rel=alternate media=...>` that is not an hreflang alternate; keys `media`,
/// `raw_href`, `url`.
pub fn extract_mobile_alternates(
    soup: &Soup,
    base_url: &str,
) -> Result<Vec<BTreeMap<String, String>>> {
    let mut alternates = Vec::new();
    for tag in find_all(soup, "link") {
        if !rel_has(attr(&tag, "rel"), "alternate") {
            continue;
        }
        let hreflang = attr(&tag, "hreflang").unwrap_or("");
        let media = attr(&tag, "media").unwrap_or("");
        if !hreflang.is_empty() || media.is_empty() || is_inert_template_content(&tag) {
            continue;
        }
        let raw_href = py_strip(attr(&tag, "href").unwrap_or("")).to_owned();
        let url = if raw_href.is_empty() {
            String::new()
        } else {
            urljoin(base_url, &raw_href)?
        };
        let mut row = BTreeMap::new();
        row.insert("media".to_owned(), collapse_whitespace(media));
        row.insert("raw_href".to_owned(), raw_href);
        row.insert("url".to_owned(), url);
        alternates.push(row);
    }
    Ok(alternates)
}

/// Live JSON-LD `<script>` tags (normalised type, inert content excluded).
/// Shared with `schema_org`.
pub fn live_jsonld_tags(soup: &Soup) -> Vec<ElementRef<'_>> {
    find_all(soup, "script")
        .filter(|tag| {
            py_strip(attr(tag, "type").unwrap_or("")).to_lowercase() == "application/ld+json"
        })
        .filter(|tag| !is_inert_template_content(tag))
        .collect()
}

/// Parse every JSON-LD block: `(parsed blocks, invalid records)`.
/// The `error` text matches the standard JSON parser's wording.
pub fn extract_jsonld(soup: &Soup) -> (Vec<Json>, Vec<JsonldInvalid>) {
    let mut out = Vec::new();
    let mut invalid = Vec::new();
    for (index, tag) in live_jsonld_tags(soup).iter().enumerate() {
        let index = (index + 1) as u64;
        let raw = get_text(tag, "");
        let text = py_strip(&raw);
        if text.is_empty() {
            invalid.push(JsonldInvalid {
                index,
                error: "block is empty".to_owned(),
                excerpt: String::new(),
            });
            continue;
        }
        match pyjson::loads(text) {
            Ok(value) => out.push(value),
            Err(err) => invalid.push(JsonldInvalid {
                index,
                error: err.0,
                excerpt: char_prefix(text, JSONLD_EXCERPT_CHARS).to_owned(),
            }),
        }
    }
    (out, invalid)
}

/// Resolved URL of the first `<link rel=amphtml>`, `""` when none.
pub fn extract_amphtml(soup: &Soup, base_url: &str) -> Result<String> {
    for tag in find_all(soup, "link") {
        let rel = attr(&tag, "rel").unwrap_or("");
        if !rel
            .split_whitespace()
            .any(|t| t.to_lowercase() == "amphtml")
        {
            continue;
        }
        if is_inert_template_content(&tag) {
            continue;
        }
        let raw_href = py_strip(attr(&tag, "href").unwrap_or(""));
        if !raw_href.is_empty() {
            return urljoin(base_url, raw_href);
        }
    }
    Ok(String::new())
}

/// Every hreflang alternate in document order.
pub fn extract_hreflang(soup: &Soup, base_url: &str) -> Result<Vec<HreflangAlternate>> {
    let mut alternates = Vec::new();
    for tag in hreflang_tags(soup) {
        if is_inert_template_content(&tag) {
            continue;
        }
        let raw_href = py_strip(attr(&tag, "href").unwrap_or("")).to_owned();
        let url = if raw_href.is_empty() {
            String::new()
        } else {
            urljoin(base_url, &raw_href)?
        };
        alternates.push(HreflangAlternate {
            lang: py_strip(attr(&tag, "hreflang").unwrap_or("")).to_owned(),
            raw_href,
            url,
        });
    }
    Ok(alternates)
}

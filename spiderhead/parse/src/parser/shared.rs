//! Constants and pure helpers shared by the parser layers.

use crate::dom::ElementRef;
use crate::py_url::{unquote, urlparse};
use crate::urlutil::hostname;

pub use spiderhead_core::user_agents::CHECKS_PARSER_USER_AGENT as DEFAULT_UA;

/// Default fetch timeout in seconds.
pub const DEFAULT_TIMEOUT: f64 = 15.0;
/// Redirect limit of the parser's own fetch.
pub const MAX_REDIRECTS: u32 = 8;
/// Maximum repeated ids reported.
pub const DUPLICATE_ID_CAP: usize = 20;
/// Maximum chars of a reported id.
pub const DUPLICATE_ID_CHARS: usize = 256;
/// Elements whose subtree is foreign content (a `<title>` there is not the document title).
pub const FOREIGN_CONTENT: [&str; 2] = ["svg", "math"];
/// Containers whose content never renders (excluded from link/URL discovery).
pub const INERT_LINK_CONTAINERS: [&str; 1] = ["template"];
/// Query parameter of the deprecated AJAX crawling scheme.
pub const ESCAPED_FRAGMENT_PARAM: &str = "_escaped_fragment_";

/// URL-bearing attributes beyond `a[href]`: tag -> attributes.
pub const URL_SOURCE_ATTRS: [(&str, &[&str]); 18] = [
    ("img", &["src", "srcset"]),
    ("source", &["src", "srcset"]),
    ("script", &["src"]),
    ("link", &["href"]),
    ("iframe", &["src"]),
    ("embed", &["src"]),
    ("video", &["src", "poster"]),
    ("audio", &["src"]),
    ("track", &["src"]),
    ("input", &["src", "formaction"]),
    ("button", &["formaction"]),
    ("form", &["action"]),
    ("a", &["ping"]),
    ("blockquote", &["cite"]),
    ("q", &["cite"]),
    ("del", &["cite"]),
    ("ins", &["cite"]),
    ("object", &["data"]),
];

/// Request headers mirroring a real navigation (no `Accept-Encoding`: the client sets it).
pub fn default_headers() -> Vec<(&'static str, &'static str)> {
    vec![
        ("User-Agent", DEFAULT_UA),
        (
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8",
        ),
        ("Accept-Language", "en-US,en;q=0.9"),
        ("Upgrade-Insecure-Requests", "1"),
        ("Sec-Fetch-Dest", "document"),
        ("Sec-Fetch-Mode", "navigate"),
        ("Sec-Fetch-Site", "none"),
        ("Sec-Fetch-User", "?1"),
        ("Cache-Control", "no-cache"),
        ("Pragma", "no-cache"),
    ]
}

/// True when `href_abs` points to a different host than `base_url` (case-insensitive;
/// unparsable or host-less is internal).
pub fn is_external(href_abs: &str, base_url: &str) -> bool {
    let (Ok(target), Ok(base)) = (hostname(href_abs), hostname(base_url)) else {
        return false;
    };
    match target {
        None => false,
        Some(target) => target != base.unwrap_or_default(),
    }
}

/// True when `url` is written in the deprecated AJAX crawling scheme (`_escaped_fragment_`
/// query key or a `#!` fragment).
pub fn uses_ajax_crawling_scheme(url: &str) -> bool {
    if url.is_empty() {
        return false;
    }
    let Ok(parts) = urlparse(url) else {
        return false;
    };
    if parts.fragment.starts_with('!') {
        return true;
    }
    // Blank values count: every non-empty `&` segment contributes its key.
    parts
        .query
        .split('&')
        .filter(|segment| !segment.is_empty())
        .any(|segment| {
            let key = segment.split_once('=').map_or(segment, |(k, _)| k);
            unquote(&key.replace('+', " ")) == ESCAPED_FRAGMENT_PARAM
        })
}

/// True when `tag` itself, or any ancestor, is named in `names`.
pub fn has_ancestor(tag: &ElementRef<'_>, names: &[&str]) -> bool {
    crate::dom::has_ancestor_named(tag, names)
}

/// True when `tag` is, or sits inside, a `<template>`.
/// Public so `asset_evidence` and others share one fragment-exclusion rule.
pub fn is_inert_template_content(tag: &ElementRef<'_>) -> bool {
    has_ancestor(tag, &INERT_LINK_CONTAINERS)
}

/// Whether a `rel` attribute value contains `token` (ASCII-whitespace split, case-insensitive).
pub fn rel_has(rel: Option<&str>, token: &str) -> bool {
    let target = token.to_lowercase();
    rel.is_some_and(|value| {
        value
            .split(['\t', '\r', '\n', '\x0c', ' '])
            .any(|t| t.to_lowercase() == target)
    })
}

/// Case-insensitive attribute equality.
pub fn eq_ci(value: Option<&str>, target: &str) -> bool {
    value.is_some_and(|v| v.to_lowercase() == target.to_lowercase())
}

/// Collapse runs of whitespace to single spaces and trim; `None` or empty input gives an empty string.
pub fn collapse_whitespace(text: &str) -> String {
    text.split(crate::py_str::is_py_space)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// First `n` characters of `text`.
pub(crate) fn char_prefix(text: &str, n: usize) -> &str {
    match text.char_indices().nth(n) {
        Some((at, _)) => &text[..at],
        None => text,
    }
}

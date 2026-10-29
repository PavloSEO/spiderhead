//! Pure HTML/resource helpers for asset-weight evidence used by the parser and the store.
//! Pure helpers for asset-weight evidence. The network analysis (`analyze_page_asset_weight`)
//! lives in the `checks` crate.

use std::sync::OnceLock;

use fancy_regex::Regex;
use serde::{Deserialize, Serialize};
use unicode_general_category::{get_general_category, GeneralCategory as G};

use crate::dom::{ElementRef, Soup};
use crate::parser::shared::{is_inert_template_content, rel_has};
use crate::py_str::{is_py_space, py_strip};
use crate::types::Result;
use crate::urlutil::urljoin;

/// Matches `max-age=N` as a whole `Cache-Control` directive.
const MAX_AGE_PATTERN: &str =
    r"(?i)(?:^|,)[\s\x1c-\x1f]*max-age[\s\x1c-\x1f]*=[\s\x1c-\x1f]*(\d+)(?=[\s\x1c-\x1f]*(?:,|$))";
/// Captures the body of each `@font-face { ... }` block.
const FONT_FACE_PATTERN: &str = r"(?is)@font-face[\s\x1c-\x1f]*\{([^}]*)\}";
/// Matches a swap-like `font-display` declaration.
const FONT_DISPLAY_OK_PATTERN: &str =
    r"(?i)font-display[\s\x1c-\x1f]*:[\s\x1c-\x1f]*(swap|fallback|optional)";
/// Matches transpiler and polyfill markers in JavaScript.
const LEGACY_JS_PATTERN: &str = r"core-js|regeneratorRuntime|_babelPolyfill|@babel/runtime|Object\.assign[\s\x1c-\x1f]*=[\s\x1c-\x1f]*function";

static MAX_AGE: OnceLock<Option<Regex>> = OnceLock::new();
static FONT_FACE: OnceLock<Option<Regex>> = OnceLock::new();
static FONT_DISPLAY_OK: OnceLock<Option<Regex>> = OnceLock::new();
static LEGACY_JS: OnceLock<Option<Regex>> = OnceLock::new();

/// Compiled pattern, built once; a pattern that failed to compile matches nothing.
fn compiled(cell: &'static OnceLock<Option<Regex>>, pattern: &str) -> Option<&'static Regex> {
    cell.get_or_init(|| Regex::new(pattern).ok()).as_ref()
}

/// Value of one decimal digit (category Nd). Nd characters come in runs of ten starting at a zero,
/// so the value is the offset from the start of the character's run.
fn decimal_digit(c: char) -> Option<u64> {
    if get_general_category(c) != G::DecimalNumber {
        return None;
    }
    let mut start = c as u32;
    while start > 0 {
        match char::from_u32(start - 1) {
            Some(prev) if get_general_category(prev) == G::DecimalNumber => start -= 1,
            _ => break,
        }
    }
    Some(u64::from(c as u32 - start) % 10)
}

/// Value of a run of Unicode decimal digits; `None` when a character is not one or the value
/// does not fit in `u64`.
fn decimal_value(digits: &str) -> Option<u64> {
    digits.chars().try_fold(0u64, |acc, c| {
        acc.checked_mul(10)?.checked_add(decimal_digit(c)?)
    })
}

/// Splits lines at LF, VT, FF, CR, U+0085, U+2028 and U+2029 without keeping the ends.
/// A CR LF pair is one boundary; no trailing empty line.
fn py_splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        let end = match c {
            '\r' if matches!(chars.peek(), Some((_, '\n'))) => {
                chars.next();
                index + 2
            }
            '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}'
            | '\u{2028}' | '\u{2029}' => index + c.len_utf8(),
            _ => continue,
        };
        lines.push(&text[start..index]);
        start = end;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// `Cache-Control` max-age (seconds) from which an asset counts as long-lived: 7 days.
pub const LONG_CACHE_SECONDS: u64 = 7 * 24 * 3600;

/// Verdict of [`check_cache_lifetime`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheLifetime {
    /// Long-lived enough for a static asset.
    pub ok: bool,
    /// Parsed max-age; `Some(0)` for no-store/no-cache, `None` when absent or beyond 19 digits.
    pub max_age: Option<u64>,
    /// Why it is not ok; `None` when ok.
    pub reason: Option<String>,
}

/// A render-blocking head resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderBlockingResource {
    /// Resolved URL.
    pub url: String,
    /// `script` or `link`.
    pub tag: String,
}

/// A `@font-face` block without a swap-like `font-display`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingFontDisplay {
    /// Trimmed block text, first 200 chars.
    pub excerpt: String,
}

/// Fraction of `text` that is whitespace (0.0 for empty).
pub fn whitespace_ratio(text: &str) -> f64 {
    let total = text.chars().count();
    if total == 0 {
        return 0.0;
    }
    let spaces = text.chars().filter(|&c| is_py_space(c)).count();
    spaces as f64 / total as f64
}

/// Heuristic: minified CSS/JS reads as long lines with little whitespace.
pub fn looks_minified(text: &str) -> bool {
    let stripped = py_strip(text);
    let lines = py_splitlines(stripped);
    // An empty text still counts as one line.
    let line_count = lines.len().max(1);
    let chars = stripped.chars().count();
    let sparse = whitespace_ratio(stripped) < 0.15;
    if chars < 200 {
        return line_count <= 1 && sparse;
    }
    let average_line = chars as f64 / line_count as f64;
    sparse && average_line > 200.0
}

/// Whether one `<script>`/`<link>` tag blocks first paint as written; `attrs` are the tag's
/// attributes (name -> value, presence matters for `async`/`defer`).
pub fn is_render_blocking(tag_name: &str, attrs: &[(String, String)]) -> bool {
    let get = |name: &str| {
        attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    match tag_name {
        "script" => {
            if get("async").is_some() || get("defer").is_some() {
                return false;
            }
            let kind = py_strip(get("type").unwrap_or("")).to_lowercase();
            matches!(
                kind.as_str(),
                "" | "text/javascript" | "application/javascript"
            )
        }
        "link" => {
            let media = py_strip(get("media").unwrap_or("")).to_lowercase();
            matches!(media.as_str(), "" | "all" | "screen")
        }
        _ => false,
    }
}

/// Render-blocking `<script src>` / `<link rel=stylesheet>` in `<head>`, skipping `<template>`
/// descendants. Called by `parser::parse_html`.
pub fn find_render_blocking_resources(
    soup: &Soup,
    base_url: &str,
) -> Result<Vec<RenderBlockingResource>> {
    let Some(head) = crate::dom::find(soup, "head") else {
        return Ok(Vec::new());
    };
    let inside = |name: &'static str| {
        head.descendants()
            .skip(1)
            .filter_map(ElementRef::wrap)
            .filter(move |e| e.value().name() == name && !is_inert_template_content(e))
    };
    let attrs_of = |tag: &ElementRef<'_>| -> Vec<(String, String)> {
        tag.value()
            .attrs()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect()
    };
    let mut out = Vec::new();
    for tag in inside("script") {
        let src = tag.value().attr("src").unwrap_or("");
        if !src.is_empty() && is_render_blocking("script", &attrs_of(&tag)) {
            out.push(RenderBlockingResource {
                url: urljoin(base_url, src)?,
                tag: "script".to_owned(),
            });
        }
    }
    for tag in inside("link") {
        let href = tag.value().attr("href").unwrap_or("");
        if !href.is_empty()
            && rel_has(tag.value().attr("rel"), "stylesheet")
            && is_render_blocking("link", &attrs_of(&tag))
        {
            out.push(RenderBlockingResource {
                url: urljoin(base_url, href)?,
                tag: "link".to_owned(),
            });
        }
    }
    Ok(out)
}

/// `@font-face` blocks without `font-display: swap|fallback|optional`.
pub fn find_missing_font_display(css_text: &str) -> Vec<MissingFontDisplay> {
    let (Some(face), Some(display_ok)) = (
        compiled(&FONT_FACE, FONT_FACE_PATTERN),
        compiled(&FONT_DISPLAY_OK, FONT_DISPLAY_OK_PATTERN),
    ) else {
        return Vec::new();
    };
    face.captures_iter(css_text)
        .filter_map(|item| item.ok())
        .filter_map(|captures| captures.get(1).map(|block| block.as_str().to_owned()))
        .filter(|block| !display_ok.is_match(block.as_str()).unwrap_or(false))
        .map(|block| MissingFontDisplay {
            excerpt: py_strip(&block).chars().take(200).collect(),
        })
        .collect()
}

/// Whether `js_text` carries a transpiler/polyfill marker (heuristic).
pub fn looks_legacy_transpiled(js_text: &str) -> bool {
    compiled(&LEGACY_JS, LEGACY_JS_PATTERN)
        .is_some_and(|regex| regex.is_match(js_text).unwrap_or(false))
}

/// Whether a static asset's `Cache-Control` is long-lived.
pub fn check_cache_lifetime(cache_control: Option<&str>) -> CacheLifetime {
    let value = cache_control.unwrap_or("");
    let lowered = value.to_lowercase();
    if lowered.contains("no-store") || lowered.contains("no-cache") {
        return CacheLifetime {
            ok: false,
            max_age: Some(0),
            reason: Some("no-store/no-cache on a static asset".to_owned()),
        };
    }
    let digits = compiled(&MAX_AGE, MAX_AGE_PATTERN)
        .and_then(|regex| regex.captures(value).ok().flatten())
        .and_then(|captures| captures.get(1).map(|m| m.as_str().to_owned()));
    if digits.as_ref().is_some_and(|d| d.chars().count() > 19) {
        return CacheLifetime {
            ok: false,
            max_age: None,
            reason: Some("Cache-Control max-age exceeds the supported integer bound".to_owned()),
        };
    }
    let Some(max_age) = digits.as_deref().and_then(decimal_value) else {
        return CacheLifetime {
            ok: false,
            max_age: None,
            reason: Some("no Cache-Control max-age".to_owned()),
        };
    };
    if lowered.contains("immutable") || max_age >= LONG_CACHE_SECONDS {
        return CacheLifetime {
            ok: true,
            max_age: Some(max_age),
            reason: None,
        };
    }
    CacheLifetime {
        ok: false,
        max_age: Some(max_age),
        reason: Some("max-age is short for a static asset".to_owned()),
    }
}

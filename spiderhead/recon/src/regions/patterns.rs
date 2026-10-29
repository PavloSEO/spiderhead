//! Regular expressions and name matching for regional hosts, slugs and labels.

use super::*;

pub(super) fn tel_re() -> &'static Regex {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?:\+7|\b8)[\s\-(]*\d{3}[\s\-)]*\d{3}[\s\-]*\d{2}[\s\-]*\d{2}")
            .unwrap_or_else(|e| panic!("{e}"))
    });
    &RE
}
pub(super) fn slug_split_re() -> &'static Regex {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[^a-z0-9]+").unwrap_or_else(|e| panic!("{e}")));
    &RE
}
pub(super) fn cyrillic_re() -> &'static Regex {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)[а-яё]").unwrap_or_else(|e| panic!("{e}")));
    &RE
}
pub(super) fn norm_ru_re() -> &'static Regex {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[^а-я0-9]+").unwrap_or_else(|e| panic!("{e}")));
    &RE
}
pub(super) fn meta_robots_re() -> &'static Regex {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?i)<meta[^>]+name=["']robots["'][^>]+content=["']([^"']+)"#)
            .unwrap_or_else(|e| panic!("{e}"))
    });
    &RE
}

/// Normalize Russian text to a lowercase, space-delimited comparison form
pub(super) fn norm_ru(value: &str) -> String {
    norm_ru_re()
        .replace_all(&value.to_lowercase().replace('ё', "е"), " ")
        .trim()
        .to_string()
}

/// Return a region found in Russian anchor or heading text
pub fn detect_region_name(text: &str) -> Option<String> {
    if text.is_empty() || !cyrillic_re().is_match(text) {
        return None;
    }
    let normalized = norm_ru(text);
    if normalized.is_empty() {
        return None;
    }
    if let Some(exact) = REGION_NAMES.get(&normalized) {
        return Some(exact.clone());
    }
    for (key, name) in NAMES_BY_LENGTH.iter() {
        let pattern = format!("(?<![а-я]){}(?![а-я])", regex::escape(key));
        if Regex::new(&pattern)
            .map(|re| re.is_match(&normalized))
            .unwrap_or(false)
        {
            return Some(name.clone());
        }
    }
    None
}

/// Slug candidates: the token plus its non-alphanumeric components
pub(super) fn slugs_of(token: &str) -> Vec<String> {
    let token = token.trim().to_lowercase();
    if token.is_empty() {
        return Vec::new();
    }
    let mut out = vec![token.clone()];
    for part in slug_split_re().split(&token) {
        if !part.is_empty() && part != token {
            out.push(part.to_string());
        }
    }
    out
}

/// Return a region for a URL token, testing the full token before its parts
pub fn detect_region(token: &str) -> Option<String> {
    for candidate in slugs_of(token) {
        if let Some(name) = REGION_SLUGS.get(&candidate) {
            return Some(name.clone());
        }
    }
    None
}

pub(super) fn hostname_of(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_lowercase()))
        .unwrap_or_default()
}

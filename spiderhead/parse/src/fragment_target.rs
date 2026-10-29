//! Legacy `_escaped_fragment_` target for pages on the retired AJAX-crawling scheme.
//!
//! A page opts in with a `#!` fragment in its own URL, or a page-wide
//! `<meta name="fragment" content="!">` tag; the target is the companion URL that carries
//! the snapshot in `_escaped_fragment_`.

use std::sync::OnceLock;

use fancy_regex::Regex;

use crate::py_url::{parse_qsl, urlencode, urlparse, urlunsplit, UrlError};

/// The meta-tag signal, matched case-insensitively.
const FRAGMENT_META_PATTERN: &str = r#"(?i)<meta[^>]+name=["']fragment["'][^>]+content=["']!["']"#;

static FRAGMENT_META: OnceLock<Option<Regex>> = OnceLock::new();

/// Whether the raw HTML carries the page-wide fragment meta tag.
fn fragment_meta_declared(html: &str) -> bool {
    FRAGMENT_META
        .get_or_init(|| Regex::new(FRAGMENT_META_PATTERN).ok())
        .as_ref()
        .is_some_and(|regex| regex.is_match(html).unwrap_or(false))
}

/// Insert or overwrite one key in an ordered pair list, keeping the first occurrence's position.
fn set_pair(pairs: &mut Vec<(String, String)>, key: &str, value: String) {
    match pairs.iter_mut().find(|(existing, _)| existing == key) {
        Some(pair) => pair.1 = value,
        None => pairs.push((key.to_owned(), value)),
    }
}

/// The `_escaped_fragment_` URL for a page that declares the legacy scheme, or `None`.
///
/// The escaped value is the fragment after `#!` (empty for the meta-tag signal). Any query
/// parameters survive, and a value already present under the same key is overwritten. Duplicate
/// keys collapse to one entry, keeping the first position and the last value. Returns
/// [`UrlError::InvalidIpv6`] for an invalid IPv6 host.
pub fn legacy_fragment_target(url: &str, html: &str) -> Result<Option<String>, UrlError> {
    let parts = urlparse(url)?;
    let escaped = if let Some(value) = parts.fragment.strip_prefix('!') {
        value.to_owned()
    } else if !html.is_empty() && fragment_meta_declared(html) {
        String::new()
    } else {
        return Ok(None);
    };
    let mut query: Vec<(String, String)> = Vec::new();
    for (key, value) in parse_qsl(&parts.query) {
        set_pair(&mut query, &key, value);
    }
    set_pair(&mut query, "_escaped_fragment_", escaped);
    Ok(Some(urlunsplit(
        &parts.scheme,
        &parts.netloc,
        &parts.path,
        &urlencode(&query),
        "",
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_bang_url_produces_its_escaped_fragment_url() {
        let target = legacy_fragment_target("https://example.com/app#!/product/1", "").unwrap();
        assert_eq!(
            target.as_deref(),
            Some("https://example.com/app?_escaped_fragment_=%2Fproduct%2F1")
        );
    }

    #[test]
    fn page_wide_fragment_meta_tag_is_honoured_without_a_hash_bang() {
        let html = r#"<meta name="fragment" content="!">"#;
        let target = legacy_fragment_target("https://example.com/app", html).unwrap();
        assert_eq!(
            target.as_deref(),
            Some("https://example.com/app?_escaped_fragment_=")
        );
    }

    #[test]
    fn page_with_neither_signal_has_no_legacy_fragment_target() {
        let target = legacy_fragment_target("https://example.com/app", "<html></html>").unwrap();
        assert_eq!(target, None);
    }

    #[test]
    fn existing_query_string_is_preserved_alongside_the_escaped_fragment() {
        let target = legacy_fragment_target("https://example.com/app?lang=en#!/x", "")
            .unwrap()
            .unwrap_or_default();
        assert!(target.contains("_escaped_fragment_=%2Fx"), "{target}");
        assert!(target.contains("lang=en"), "{target}");
    }

    #[test]
    fn existing_escaped_fragment_key_is_overwritten_in_place() {
        let target = legacy_fragment_target(
            "https://example.com/app?_escaped_fragment_=old&z=1#!/new",
            "",
        )
        .unwrap();
        assert_eq!(
            target.as_deref(),
            Some("https://example.com/app?_escaped_fragment_=%2Fnew&z=1")
        );
    }
}

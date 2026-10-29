//! Human face of a check: registry title, fix hint and the category that
//! groups it on the dashboard. The raw code stays a secondary label.

use crate::check_titles;
use crate::i18n;

/// What the registry says about one check.
pub struct CheckInfo {
    /// Russian title when the table has one, else the registry message.
    pub title: String,
    /// Russian fix hint when the table has one, else the registry hint.
    pub fix: Option<String>,
    /// Evidence source tag.
    pub source: String,
}

/// Registry data for a check id (case-insensitive); unknown ids fall back to
/// the code itself so the list never shows a blank row.
pub fn info(id: &str) -> CheckInfo {
    let found = spiderhead_formats::check_registry::builtin()
        .ok()
        .and_then(|r| r.get(&id.to_uppercase()).or_else(|| r.get(id)));
    let ru_title = check_titles::title(id);
    let ru_fix = check_titles::fix(id);
    match found {
        Some(m) => CheckInfo {
            title: ru_title.map_or_else(|| m.message.clone(), str::to_string),
            fix: ru_fix.map(str::to_string).or_else(|| m.fix.clone()),
            source: m.source.clone(),
        },
        None => CheckInfo {
            title: ru_title.unwrap_or(id).to_string(),
            fix: ru_fix.map(str::to_string),
            source: String::new(),
        },
    }
}

/// Dashboard group of a check, decided by the vocabulary of its id. Order of
/// the rules matters: the first match wins.
pub fn category(id: &str) -> &'static str {
    let id = id.to_uppercase();
    let has = |words: &[&str]| words.iter().any(|w| id.contains(w));
    if has(&[
        "BROKEN_PAGE",
        "SERVER_ERROR",
        "NO_RESPONSE",
        "STATUS_CODE",
        "TIMEOUT",
    ]) {
        i18n::CAT_RESPONSE
    } else if has(&[
        "SECURITY",
        "HTTPS",
        "HSTS",
        "MIXED_CONTENT",
        "CSP",
        "X_FRAME",
        "PROTOCOL",
    ]) {
        i18n::CAT_SECURITY
    } else if has(&["TITLE", "META_DESC", "DESCRIPTION"]) {
        i18n::CAT_TITLES
    } else if has(&["H1", "H2", "HEADING"]) {
        i18n::CAT_HEADINGS
    } else if has(&[
        "CANONICAL",
        "NOINDEX",
        "ROBOTS",
        "DIRECTIVE",
        "NOFOLLOW",
        "PAGINATION",
        "AMP_",
    ]) {
        i18n::CAT_DIRECTIVES
    } else if has(&["IMAGE", "ALT_"]) {
        i18n::CAT_IMAGES
    } else if has(&["HREFLANG", "SITEMAP", "LANG"]) {
        i18n::CAT_INTL
    } else if has(&["JSONLD", "SCHEMA", "STRUCTURED", "OG_", "TWITTER", "SOCIAL"]) {
        i18n::CAT_STRUCTURED
    } else if has(&[
        "RENDER",
        "JS_",
        "RESOURCE",
        "CSS",
        "LIGHTHOUSE",
        "LAYOUT",
        "MOBILE",
        "VIEWPORT",
    ]) {
        i18n::CAT_RENDER
    } else if has(&["URL_", "_URL", "UNDERSCORE", "UPPERCASE", "PARAMETER"]) {
        i18n::CAT_URL
    } else if has(&["REDIRECT", "LINK", "ANCHOR", "INLINK", "OUTLINK", "ORPHAN"]) {
        i18n::CAT_LINKS
    } else if has(&[
        "CONTENT",
        "WORD",
        "DUPLICATE",
        "READAB",
        "LOREM",
        "SPELL",
        "THIN",
        "TEXT",
    ]) {
        i18n::CAT_CONTENT
    } else {
        i18n::CAT_OTHER
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_checks_get_a_title_and_most_get_a_real_category() {
        let reg = spiderhead_formats::check_registry::builtin().expect("registry");
        let mut other = 0;
        for c in reg.checks() {
            assert!(!info(&c.id).title.is_empty());
            if category(&c.id) == i18n::CAT_OTHER {
                other += 1;
            }
        }
        // «Прочее» must stay the exception, not the bucket.
        assert!(
            other * 3 < reg.len(),
            "{other} of {} checks are uncategorised",
            reg.len()
        );
        assert_eq!(category("broken_page_4xx"), i18n::CAT_RESPONSE);
        assert_eq!(info("nope").title, "nope");
    }
}

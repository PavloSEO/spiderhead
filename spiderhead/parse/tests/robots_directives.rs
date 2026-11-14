//! Ports of the parse-side cases of the reference suite: directive parsing,
//! `<meta>` robots collection and the document title. Cases that go through the crawl record,
//! the indexability verdict or the checks rules are deferred (see the slice report).

use std::collections::BTreeSet;

use spiderhead_parse::parser::meta::robots_directives;
use spiderhead_parse::types::ParseOptions;
use spiderhead_parse::{parse_html, ParsedPage};

fn tokens(values: &[Option<&str>]) -> BTreeSet<String> {
    robots_directives(values).into_iter().collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

fn parsed(html: &str) -> ParsedPage {
    parse_html(html, "https://example.com/", &ParseOptions::default()).expect("parse_html")
}

// --- directive parsing -------------------------------------------------------------------

#[test]
fn none_is_shorthand_for_noindex_nofollow() {
    // "none" contains neither substring, so a substring search misses it.
    let t = tokens(&[Some("none")]);
    assert!(t.contains("noindex") && t.contains("nofollow"), "{t:?}");
}

#[test]
fn user_agent_prefix_is_stripped() {
    // X-Robots-Tag may address one crawler: "googlebot: noindex, nofollow".
    assert!(tokens(&[Some("googlebot: noindex, nofollow")]).contains("noindex"));
}

#[test]
fn valued_directives_keep_their_value() {
    let t = tokens(&[Some("index, max-snippet:-1, max-image-preview:large")]);
    assert!(t.contains("max-snippet:-1"), "{t:?}");
    assert!(!t.contains("noindex"), "{t:?}");
}

#[test]
fn empty_and_none_inputs() {
    assert!(robots_directives(&[None, Some("")]).is_empty());
}

#[test]
fn a_directive_scoped_to_a_non_google_agent_is_dropped() {
    // #201: a Bingbot/Yandex-only directive must not read as a global one.
    assert!(tokens(&[Some("bingbot: noindex")]).is_empty());
    assert!(tokens(&[Some("yandex: noindex")]).is_empty());
}

#[test]
fn a_generic_directive_still_combines_with_a_googlebot_one() {
    assert_eq!(
        tokens(&[Some("index, googlebot: noindex")]),
        set(&["index", "noindex"])
    );
}

// --- collection ----------------------------------------------------------------------------

#[test]
fn crawler_addressed_tags_are_collected() {
    let html = r#"<html><head><meta name="robots" content="index"><meta name="googlebot" content="noindex"></head><body>x</body></html>"#;
    let page = parsed(html);
    // "robots" keeps its literal meaning; robots_meta carries every tag.
    assert_eq!(page.robots.as_deref(), Some("index"));
    assert_eq!(page.robots_meta, vec!["index", "noindex"]);
}

#[test]
fn unrelated_meta_names_are_not_directives() {
    let html = r#"<html><head><meta name="viewport" content="noindex"></head></html>"#;
    assert!(parsed(html).robots_meta.is_empty());
}

#[test]
fn robots_meta_scoped_prefixes_only_the_non_generic_names() {
    // #201: the generic tag's content stays bare; a named one is prefixed the same way an
    // X-Robots-Tag agent scope is, so robots_directives can filter it.
    let html = r#"<html><head><meta name="robots" content="index"><meta name="bingbot" content="noindex"></head><body>x</body></html>"#;
    assert_eq!(
        parsed(html).robots_meta_scoped,
        vec!["index", "bingbot: noindex"]
    );
}

// --- document title ------------------------------------------------------------------------

#[test]
fn svg_title_is_not_the_page_title() {
    let html = "<html><head><svg><title>Menu</title></svg><title>The real one</title></head><body>x</body></html>";
    assert_eq!(parsed(html).title.as_deref(), Some("The real one"));
}

#[test]
fn svg_title_does_not_hide_a_missing_title() {
    let html = "<html><head></head><body><svg><title>Close</title></svg></body></html>";
    assert_eq!(parsed(html).title, None);
}

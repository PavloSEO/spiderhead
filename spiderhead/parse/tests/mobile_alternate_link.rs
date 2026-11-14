//! Ports of the parser-level cases in the reference suite (#1016): a
//! media-qualified rel=alternate that names no URL. The crawl -> evidence -> rules pipeline
//! cases need the crawl and checks crates and are deferred.

use spiderhead_parse::dom::parse_document;
use spiderhead_parse::parser::content::mobile_alternate_broken_count;

fn count(head: &str) -> u64 {
    let soup = parse_document(&format!("<html><head>{head}</head><body></body></html>"));
    mobile_alternate_broken_count(&soup)
}

#[test]
fn media_alternate_without_href_counts() {
    assert_eq!(
        count(r#"<link rel="alternate" media="only screen and (max-width: 640px)">"#),
        1
    );
}

#[test]
fn media_alternate_with_blank_href_counts() {
    assert_eq!(
        count(r#"<link rel="alternate" media="(max-width: 640px)" href="  ">"#),
        1
    );
}

#[test]
fn media_alternate_with_href_is_valid() {
    assert_eq!(
        count(r#"<link rel="alternate" media="(max-width: 640px)" href="https://m.example.com/">"#),
        0
    );
}

#[test]
fn alternate_without_media_is_not_a_mobile_alternate() {
    assert_eq!(count(r#"<link rel="alternate" hreflang="de">"#), 0);
}

#[test]
fn media_on_a_non_alternate_link_is_ignored() {
    assert_eq!(
        count(r#"<link rel="stylesheet" media="print" href="/print.css">"#),
        0
    );
}

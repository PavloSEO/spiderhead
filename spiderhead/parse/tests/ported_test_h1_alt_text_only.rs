//! H1 alt-only text, ported from the reference suite.
//! The two crawl-pipeline tests (`H1_ALT_TEXT_ONLY` through `collect_urls`/`run_rules`) are
//! deferred until the crawl and checks crates exist.

use spiderhead_parse::dom::parse_document_lxml;
use spiderhead_parse::parse_html;
use spiderhead_parse::parser::content::h1_alt_only_text;
use spiderhead_parse::types::ParseOptions;

#[test]
fn h1_with_only_an_alt_bearing_image_returns_its_alt_text() {
    let soup = parse_document_lxml(
        "<html><body><h1><img src='logo.png' alt='Acme Pumps'></h1></body></html>",
    );
    assert_eq!(h1_alt_only_text(&soup).as_deref(), Some("Acme Pumps"));
}

#[test]
fn logo_beside_real_heading_text_is_not_flagged() {
    let soup = parse_document_lxml(
        "<html><body><h1><img src='logo.png' alt='Logo'> Foundation Repair Guide</h1></body></html>",
    );
    assert_eq!(h1_alt_only_text(&soup), None);
}

#[test]
fn plain_empty_h1_with_no_image_returns_none() {
    let soup = parse_document_lxml("<html><body><h1></h1></body></html>");
    assert_eq!(h1_alt_only_text(&soup), None);
}

#[test]
fn h1_with_real_text_and_no_image_returns_none() {
    let soup = parse_document_lxml("<html><body><h1>Ordinary Heading</h1></body></html>");
    assert_eq!(h1_alt_only_text(&soup), None);
}

#[test]
fn image_with_empty_alt_inside_an_otherwise_empty_h1_does_not_qualify() {
    let soup =
        parse_document_lxml("<html><body><h1><img src='deco.png' alt=''></h1></body></html>");
    assert_eq!(h1_alt_only_text(&soup), None);
}

#[test]
fn headings_dict_still_reads_this_h1_as_empty() {
    let html = "<html><body><h1><img src='logo.png' alt='Acme Pumps'></h1></body></html>";
    let parsed =
        parse_html(html, "https://example.com/", &ParseOptions::default()).expect("parse_html");
    assert!(!parsed.headings.contains_key("h1"));
    assert_eq!(parsed.h1_alt_only_text.as_deref(), Some("Acme Pumps"));
}

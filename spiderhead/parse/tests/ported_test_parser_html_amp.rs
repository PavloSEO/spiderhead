//! `<html ⚡>` marker is captured as data by parse_html.

use spiderhead_parse::dom::parse_document_lxml;
use spiderhead_parse::models::ParsedPage;
use spiderhead_parse::parse_html;
use spiderhead_parse::parser::meta::document_html_amp;
use spiderhead_parse::types::ParseOptions;

fn parse(html: &str, url: &str) -> ParsedPage {
    parse_html(html, url, &ParseOptions::default()).expect("parse_html")
}

#[test]
fn root_html_tag_amp_marker() {
    let cases: [(&str, bool); 5] = [
        ("<html amp>", true),
        ("<html ⚡>", true),
        ("<html amp lang=\"en\">", true),
        ("<html>", false),
        ("<html lang=\"en\">", false),
    ];
    for (open_tag, expected) in cases {
        let html = format!("{open_tag}<head><title>t</title></head><body><p>x</p></body></html>");
        assert_eq!(
            parse(&html, "https://example.com/a").html_amp,
            expected,
            "open tag: {open_tag}"
        );
    }
}

#[test]
fn amp_word_in_body_text_is_not_a_marker() {
    let html =
        "<html><head><title>t</title></head><body><p>amp ⚡ is a framework</p></body></html>";
    assert!(!parse(html, "https://example.com/a").html_amp);
}

#[test]
fn amp_attribute_on_nested_element_is_not_a_marker() {
    let html = "<html><head></head><body><div amp=\"1\">x</div></body></html>";
    let soup = parse_document_lxml(html);
    assert!(!document_html_amp(&soup));
}

//! are read, hreflang alternates are not mobile alternates, and inert template content is skipped.

use std::collections::BTreeMap;

use spiderhead_parse::dom::parse_document;
use spiderhead_parse::parse_html;
use spiderhead_parse::parser::meta::extract_mobile_alternates;
use spiderhead_parse::types::ParseOptions;

const PAGE: &str = r#"<html><head>
<title>Desktop</title>
<link rel="alternate" media="only screen and (max-width: 640px)" href="/m/page">
<link rel="alternate" hreflang="fr" href="/fr/page">
<link rel="alternate" media="print" href="https://other.example.com/print">
</head><body><h1>Hi</h1><template><link rel="alternate" media="all" href="/inert"></template></body></html>"#;

fn alternate(media: &str, raw_href: &str, url: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("media".to_owned(), media.to_owned()),
        ("raw_href".to_owned(), raw_href.to_owned()),
        ("url".to_owned(), url.to_owned()),
    ])
}

#[test]
fn mobile_alternate_is_read_and_resolved_against_base() {
    let soup = parse_document(PAGE);
    let found = extract_mobile_alternates(&soup, "https://example.com/page").expect("extract");
    assert_eq!(
        found,
        vec![
            alternate(
                "only screen and (max-width: 640px)",
                "/m/page",
                "https://example.com/m/page",
            ),
            alternate(
                "print",
                "https://other.example.com/print",
                "https://other.example.com/print",
            ),
        ]
    );
}

#[test]
fn hreflang_alternates_are_not_mobile_alternates() {
    let soup = parse_document(r#"<link rel="alternate" hreflang="fr" media="all" href="/fr">"#);
    let found = extract_mobile_alternates(&soup, "https://example.com/").expect("extract");
    assert!(found.is_empty());
}

#[test]
fn alternate_without_media_is_not_read() {
    let soup = parse_document(r#"<link rel="alternate" href="/feed.xml">"#);
    let found = extract_mobile_alternates(&soup, "https://example.com/").expect("extract");
    assert!(found.is_empty());
}

#[test]
fn parse_html_exposes_mobile_alternates() {
    let parsed =
        parse_html(PAGE, "https://example.com/page", &ParseOptions::default()).expect("parse_html");
    let raw: Vec<&str> = parsed
        .mobile_alternates
        .iter()
        .map(|item| item["raw_href"].as_str())
        .collect();
    assert_eq!(raw, ["/m/page", "https://other.example.com/print"]);
    let langs: Vec<&str> = parsed
        .hreflang
        .iter()
        .map(|item| item.lang.as_str())
        .collect();
    assert_eq!(langs, ["fr"]);
}

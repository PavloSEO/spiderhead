//! The hreflang declarations are kept as the document wrote them, and the position boolean
//! still answers where the tags sat. The crawl, evidence-frame and analyzer tests of that file
//! are deferred: they need the crawl, evidence and checks crates.

use spiderhead_parse::models::{HreflangAlternate, ParsedPage};
use spiderhead_parse::parse_html;
use spiderhead_parse::types::ParseOptions;

fn parse(html: &str, url: &str) -> ParsedPage {
    parse_html(html, url, &ParseOptions::default()).expect("parse_html")
}

/// reference `_page(*alternates, body=...)`: a page that passes the title and description rules.
fn page(alternates: &[(&str, &str)], body: &str) -> String {
    let links: String = alternates
        .iter()
        .map(|(lang, href)| format!(r#"<link rel="alternate" hreflang="{lang}" href="{href}">"#))
        .collect();
    format!(
        r#"<html><head><title>Title long enough for the length rule</title><meta name="description" content="A description long enough for the length rule to pass.">{links}</head><body>{body}</body></html>"#
    )
}

fn triples(alternates: &[HreflangAlternate]) -> Vec<(&str, &str, &str)> {
    alternates
        .iter()
        .map(|a| (a.lang.as_str(), a.raw_href.as_str(), a.url.as_str()))
        .collect()
}

#[test]
fn the_code_and_the_href_survive_exactly_as_written() {
    let parsed = parse(
        &page(
            &[
                ("en-GB", "/en/about"),
                ("FR", "https://example.fr/a-propos"),
            ],
            "<h1>t</h1>",
        ),
        "https://example.com/about",
    );
    assert_eq!(
        triples(&parsed.hreflang),
        vec![
            ("en-GB", "/en/about", "https://example.com/en/about"),
            (
                "FR",
                "https://example.fr/a-propos",
                "https://example.fr/a-propos"
            ),
        ]
    );
}

#[test]
fn document_order_is_preserved() {
    let parsed = parse(
        &page(
            &[("de", "/de/"), ("es", "/es/"), ("x-default", "/")],
            "<h1>t</h1>",
        ),
        "https://example.com/",
    );
    let langs: Vec<&str> = parsed.hreflang.iter().map(|a| a.lang.as_str()).collect();
    assert_eq!(langs, ["de", "es", "x-default"]);
}

#[test]
fn an_alternate_that_points_nowhere_is_recorded_rather_than_dropped() {
    let parsed = parse(
        r#"<html><head><link rel="alternate" hreflang="de"></head><body></body></html>"#,
        "https://example.com/",
    );
    assert_eq!(triples(&parsed.hreflang), vec![("de", "", "")]);
}

#[test]
fn a_template_only_alternate_is_not_a_declaration() {
    let parsed = parse(
        r#"<html><head></head><body><template><link rel="alternate" hreflang="de" href="/de/"></template></body></html>"#,
        "https://example.com/",
    );
    assert!(parsed.hreflang.is_empty());
}

#[test]
fn the_position_boolean_still_answers_its_own_question() {
    let in_head = parse(
        &page(&[("de", "/de/")], "<h1>t</h1>"),
        "https://example.com/",
    );
    let in_body = parse(
        r#"<html><head></head><body><link rel="alternate" hreflang="de" href="/de/"></body></html>"#,
        "https://example.com/",
    );
    assert_eq!(in_head.position.hreflang_outside_head, Some(false));
    assert_eq!(in_body.position.hreflang_outside_head, Some(true));
}

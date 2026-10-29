//! Parser-level trust-signal tests ported from the reference suite.
//!
//! Only the cases that need the parser alone are here: the declared carriers,
//! the measured absence, malformed JSON-LD, template content, the URL-path
//! date convention, the per-family bound, and direct calls to the extractor.
//! The checks, crawl and store cases of that file are listed in the report.

use spiderhead_parse::dom::parse_document;
use spiderhead_parse::models::TrustSignal;
use spiderhead_parse::parse_html;
use spiderhead_parse::parser::content::{extract_trust_signals, TRUST_SIGNAL_CAP};
use spiderhead_parse::types::ParseOptions;

const RICH_PAGE: &str = r#"<html><head>
<title>A thoroughly declared article</title>
<meta name="author" content="Jane Writer">
<meta property="article:published_time" content="2026-09-01T10:00:00Z">
<meta property="article:modified_time" content="2026-09-20T11:00:00Z">
<meta property="og:type" content="article">
<script type="application/ld+json">
{"@type": "NewsArticle", "headline": "Declared",
 "author": {"@type": "Person", "name": "JSON Author"},
 "datePublished": "2026-09-01", "dateModified": "2026-09-20"}
</script>
</head><body>
<article>
<span class="byline">By the desk</span>
<a href="/about/jane" rel="author">Jane's page</a>
<span itemprop="author" content="Micro Author">micro</span>
<time datetime="2026-09-21">yesterday</time>
</article>
</body></html>"#;

const BLOG_URL: &str = "https://example.com/blog/post";

fn signals_of(html: &str, url: &str) -> spiderhead_parse::models::TrustSignals {
    parse_html(html, url, &ParseOptions::default())
        .expect("parse_html")
        .trust_signals
}

fn named<'a>(items: &'a [TrustSignal], name: &str) -> &'a TrustSignal {
    items
        .iter()
        .find(|item| item.signal == name)
        .unwrap_or_else(|| panic!("no signal named {name}"))
}

#[test]
fn every_declared_carrier_is_recorded_with_its_own_name_and_confidence() {
    let signals = signals_of(RICH_PAGE, BLOG_URL);
    assert_eq!(named(&signals.author, "meta_author").value, "Jane Writer");
    assert_eq!(named(&signals.author, "meta_author").confidence, "high");
    assert_eq!(named(&signals.author, "jsonld_author").value, "JSON Author");
    assert_eq!(
        named(&signals.author, "microdata_author").confidence,
        "high"
    );
    // rel=author records the href as written, not a resolved link.
    assert_eq!(named(&signals.author, "rel_author").value, "/about/jane");
    // A CSS class is a convention, not a declaration: medium, never high.
    assert_eq!(named(&signals.author, "byline_class").confidence, "medium");

    assert_eq!(
        named(&signals.dates, "meta_article_published").confidence,
        "high"
    );
    assert_eq!(
        named(&signals.dates, "meta_article_modified").value,
        "2026-09-20T11:00:00Z"
    );
    assert_eq!(
        named(&signals.dates, "jsonld_date:datePublished").value,
        "2026-09-01"
    );
    assert_eq!(
        named(&signals.dates, "jsonld_date:dateModified").value,
        "2026-09-20"
    );
    assert_eq!(named(&signals.dates, "time_element").confidence, "medium");

    let mut article = signals.article.clone();
    article.sort();
    let mut expected = vec![
        "article_element".to_owned(),
        "jsonld_type:NewsArticle".to_owned(),
        "og_type".to_owned(),
    ];
    expected.sort();
    assert_eq!(article, expected);
}

#[test]
fn a_page_without_markup_records_a_measured_absence() {
    let signals = signals_of(
        "<html><head><title>Plain</title></head><body><p>x</p></body></html>",
        BLOG_URL,
    );
    assert!(signals.author.is_empty());
    assert!(signals.dates.is_empty());
    assert!(signals.article.is_empty());
}

#[test]
fn a_malformed_jsonld_block_contributes_nothing_and_is_named_beside_it() {
    let html = r#"<html><head>
<script type="application/ld+json">{"@type": "Article", "author": </script>
</head><body><article><p>content</p></article></body></html>"#;
    let parsed = parse_html(html, BLOG_URL, &ParseOptions::default()).expect("parse_html");
    assert!(!parsed.jsonld_invalid.is_empty());
    assert!(!parsed
        .trust_signals
        .author
        .iter()
        .any(|item| item.signal.starts_with("jsonld_")));
    assert!(!parsed
        .trust_signals
        .dates
        .iter()
        .any(|item| item.signal.starts_with("jsonld_")));
    // The <article> element is real markup: malformed JSON-LD hides only its
    // own declarations.
    assert_eq!(
        parsed.trust_signals.article,
        vec!["article_element".to_owned()]
    );
}

#[test]
fn signals_inside_a_template_are_not_in_the_rendered_document() {
    let html = r#"<html><body>
<template><meta name="author" content="Ghost"><time datetime="2026-01-01">x</time></template>
<p>visible</p></body></html>"#;
    let signals = signals_of(html, BLOG_URL);
    assert!(signals.author.is_empty());
    assert!(signals.dates.is_empty());
}

#[test]
fn a_date_written_into_the_url_path_is_a_medium_confidence_convention() {
    let signals = signals_of(
        "<html><body><p>x</p></body></html>",
        "https://example.com/2024/05/report",
    );
    let url_dates: Vec<&TrustSignal> = signals
        .dates
        .iter()
        .filter(|item| item.signal == "url_path_date")
        .collect();
    assert_eq!(url_dates.len(), 1);
    assert_eq!(url_dates[0].confidence, "medium");
    assert_eq!(url_dates[0].value, "2024/05");
}

#[test]
fn the_signal_inventory_is_bounded_per_family() {
    let many: String = (0..TRUST_SIGNAL_CAP + 4)
        .map(|i| format!(r#"<span class="byline">by{i}</span>"#))
        .collect();
    let signals = signals_of(&format!("<html><body>{many}</body></html>"), BLOG_URL);
    assert_eq!(signals.author.len(), TRUST_SIGNAL_CAP);
}

#[test]
fn extract_trust_signals_is_callable_on_its_own_tree() {
    let jsonld = parse_html(RICH_PAGE, BLOG_URL, &ParseOptions::default())
        .expect("parse_html")
        .jsonld;
    let soup = parse_document(RICH_PAGE);
    let signals = extract_trust_signals(&soup, &jsonld, BLOG_URL).expect("extract_trust_signals");
    let names: Vec<&str> = signals
        .author
        .iter()
        .map(|item| item.signal.as_str())
        .collect();
    assert!(names.contains(&"meta_author"));
    assert!(names.contains(&"jsonld_author"));
}

//! Unit tests ported from the reference suite: the parts of the parser tests that need no crawl,
//! audit or store (those belong to later crates). Each test names its reference origin.

use spiderhead_parse::dom::parse_document;
use spiderhead_parse::models::ParsedPage;
use spiderhead_parse::parse_html;
use spiderhead_parse::parser::meta::{document_charset, meta_fragment_content};
use spiderhead_parse::parser::shared::{
    collapse_whitespace, default_headers, uses_ajax_crawling_scheme, DEFAULT_UA,
};
use spiderhead_parse::parser::tokenize::{
    document_base_url, document_position, invalid_head_elements, DocumentSource,
};
use spiderhead_parse::types::ParseOptions;

const PAGE: &str = "https://example.com/section/subsection/";

fn parse(html: &str, url: &str) -> ParsedPage {
    parse_html(html, url, &ParseOptions::default()).expect("parse_html")
}

fn first_link(html: &str, url: &str) -> String {
    parse(html, url).links[0].href.clone()
}

// ---- the reference suite ----

#[test]
fn both_shapes_of_the_scheme_are_recognised() {
    assert!(uses_ajax_crawling_scheme(
        "https://example.com/#!/products/1"
    ));
    assert!(uses_ajax_crawling_scheme(
        "https://example.com/?_escaped_fragment_=/products/1"
    ));
    assert!(uses_ajax_crawling_scheme(
        "https://example.com/page?_escaped_fragment_="
    ));
}

#[test]
fn an_ordinary_url_and_an_ordinary_anchor_are_not_the_scheme() {
    for url in [
        "https://example.com/products/1",
        "https://example.com/guide#installation",
        "https://example.com/?q=hello#top",
        "",
    ] {
        assert!(!uses_ajax_crawling_scheme(url), "{url}");
    }
}

#[test]
fn the_meta_fragment_declaration_is_kept_as_written() {
    let soup = parse_document(
        r#"<html><head><meta name="fragment" content="!"></head><body></body></html>"#,
    );
    assert_eq!(meta_fragment_content(&soup), "!");
    // from ...::test_a_page_without_the_tag_records_an_empty_declaration_not_a_guess
    assert_eq!(
        meta_fragment_content(&parse_document("<html><head></head><body></body></html>")),
        ""
    );
    // from ...::test_a_meta_fragment_inside_a_template_is_inert_and_is_not_a_declaration
    let inert = parse_document(
        r#"<html><head><template><meta name="fragment" content="!"></template></head></html>"#,
    );
    assert_eq!(meta_fragment_content(&inert), "");
}

#[test]
fn parse_html_returns_the_meta_fragment() {
    let page = parse(
        r#"<html><head><meta name="fragment" content="!"></head><body>x</body></html>"#,
        "https://example.com/",
    );
    assert_eq!(page.meta_fragment, "!");
}

// ---- the reference suite ----

#[test]
fn base_href_resolution_table() {
    let cases = [
        (
            r#"<html><head><base href="https://example.com/"></head><body><a href="catalog/">c</a></body></html>"#,
            PAGE,
            "https://example.com/catalog/",
        ),
        (
            r#"<html><body><a href="catalog/">c</a></body></html>"#,
            PAGE,
            "https://example.com/section/subsection/catalog/",
        ),
        (
            r#"<html><head><base href="/shop/"></head><body><a href="x">c</a></body></html>"#,
            PAGE,
            "https://example.com/shop/x",
        ),
        (
            r#"<html><head><base href="../"></head><body><a href="x">c</a></body></html>"#,
            PAGE,
            "https://example.com/section/x",
        ),
        (
            r#"<html><head><base href="https://example.com/shop"></head><body><a href="x">c</a></body></html>"#,
            PAGE,
            "https://example.com/x",
        ),
        (
            r#"<html><head><base target="_blank"><base href="/shop/"><base href="/other/"></head><body><a href="x">c</a></body></html>"#,
            "https://example.com/a/b/",
            "https://example.com/shop/x",
        ),
        (
            r#"<html><head><base target="_blank"></head><body><a href="catalog/">c</a></body></html>"#,
            PAGE,
            "https://example.com/section/subsection/catalog/",
        ),
    ];
    for (html, url, want) in cases {
        assert_eq!(first_link(html, url), want, "{html}");
    }
}

#[test]
fn external_flag_follows_the_page_host_not_the_base() {
    let page = parse(
        r#"<html><head><base href="https://cdn.other.com/"></head><body><a href="x">c</a></body></html>"#,
        "https://example.com/p/",
    );
    assert_eq!(page.links[0].href, "https://cdn.other.com/x");
    assert!(page.links[0].external);
}

#[test]
fn canonical_and_url_sources_resolve_against_the_base() {
    let page = parse(
        r#"<html><head><base href="https://example.com/"><link rel="canonical" href="catalog/"></head><body></body></html>"#,
        PAGE,
    );
    assert_eq!(
        page.canonical.as_deref(),
        Some("https://example.com/catalog/")
    );
    // from ...::test_url_sources_resolve_against_the_base
    let options = ParseOptions {
        url_sources: true,
        ..ParseOptions::default()
    };
    let with_sources = parse_html(
        r#"<html><head><base href="https://example.com/"></head><body><img src="img/a.png"></body></html>"#,
        PAGE,
        &options,
    )
    .expect("parse");
    let urls: Vec<String> = with_sources
        .url_sources
        .unwrap_or_default()
        .iter()
        .map(|s| s["url"].clone())
        .collect();
    assert!(urls.contains(&"https://example.com/img/a.png".to_owned()));
}

#[test]
fn document_base_url_accepts_raw_html_and_parsed_markup() {
    let html = r#"<html><head><base href="/shop/"></head></html>"#;
    assert_eq!(
        document_base_url(DocumentSource::Raw(html), PAGE),
        "https://example.com/shop/"
    );
    let soup = parse_document(html);
    assert_eq!(
        document_base_url(DocumentSource::Tree(&soup), PAGE),
        "https://example.com/shop/"
    );
}

#[test]
fn template_bases_are_not_live() {
    let html = concat!(
        "<html><head>",
        r#"<template><base href="https://template.invalid/"></template>"#,
        r#"<base href="https://example.com/shop/">"#,
        r#"</head><body><a href="x">c</a></body></html>"#
    );
    assert_eq!(
        document_base_url(DocumentSource::Raw(html), PAGE),
        "https://example.com/shop/"
    );
    assert_eq!(first_link(html, PAGE), "https://example.com/shop/x");
    // from ...::test_template_only_base_falls_back_to_final_url
    let only = concat!(
        "<html><head>",
        r#"<template><base href="https://template.invalid/"></template>"#,
        r#"</head><body><a href="x">c</a></body></html>"#
    );
    assert_eq!(document_base_url(DocumentSource::Raw(only), PAGE), PAGE);
    assert_eq!(
        first_link(only, PAGE),
        "https://example.com/section/subsection/x"
    );
}

#[test]
fn hreflang_extraction_ignores_template_base_and_alternates() {
    let html = concat!(
        "<html><head><template>",
        r#"<base href="https://template.invalid/assets/">"#,
        r#"<link rel="alternate" hreflang="ru" href="/ru-template">"#,
        "</template>",
        r#"<base href="https://example.com/root/">"#,
        r#"<link rel="alternate" hreflang="en" href="en">"#,
        "</head><body></body></html>"
    );
    let page = parse(html, PAGE);
    assert_eq!(page.hreflang.len(), 1);
    assert_eq!(page.hreflang[0].lang, "en");
    assert_eq!(page.hreflang[0].raw_href, "en");
    assert_eq!(page.hreflang[0].url, "https://example.com/root/en");
}

// ---- the reference suite ----

#[test]
fn double_escaped_title_and_description_are_decoded_once() {
    let title = "Nuts &amp;amp; Bolts &amp;amp; Screws &amp;amp; Washers &amp;amp; Rivets Store";
    let page = parse(
        &format!("<html><head><title>{title}</title></head><body><p>x</p></body></html>"),
        "https://example.com/page",
    );
    let got = page.title.unwrap_or_default();
    assert_eq!(
        got,
        "Nuts &amp; Bolts &amp; Screws &amp; Washers &amp; Rivets Store"
    );
    assert_eq!(got.chars().count(), 62);
    // from ...::test_double_escaped_description_is_decoded_only_once
    let desc = "Shop nuts &amp;amp; bolts &amp;amp; screws online today, fast shipping guaranteed.";
    let page = parse(
        &format!(
            r#"<html><head><meta name="description" content="{desc}"></head><body><p>x</p></body></html>"#
        ),
        "https://example.com/page",
    );
    assert_eq!(
        page.meta_description.unwrap_or_default().chars().count(),
        74
    );
}

#[test]
fn single_escaped_markup_is_unaffected() {
    let html = concat!(
        "<html><head><title>Tom &amp; Jerry &mdash; Caf&eacute;&nbsp;Shop</title>",
        r#"<meta name="description" content="Rock &amp; roll since 1990."></head>"#,
        "<body><p>x</p></body></html>"
    );
    let page = parse(html, "https://example.com/page");
    assert_eq!(page.title.as_deref(), Some("Tom & Jerry — Café Shop"));
    assert_eq!(
        page.meta_description.as_deref(),
        Some("Rock & roll since 1990.")
    );
}

#[test]
fn collapse_whitespace_does_not_decode_entities() {
    assert_eq!(
        collapse_whitespace("Tom &amp;amp; Jerry"),
        "Tom &amp;amp; Jerry"
    );
    assert_eq!(collapse_whitespace("  a   b  "), "a b");
    assert_eq!(collapse_whitespace(""), "");
}

// ---- the reference suite ----

#[test]
fn the_parser_uses_the_canon_user_agent() {
    assert_eq!(
        DEFAULT_UA,
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36"
    );
    let headers = default_headers();
    assert_eq!(headers[0], ("User-Agent", DEFAULT_UA));
    assert!(headers.iter().all(|(name, _)| *name != "Accept-Encoding"));
}

// ---- the reference suite ----

const CLEAN_HEAD: &str = "\n<html><head>\n<title>Clean</title>\n<link rel=\"canonical\" href=\"https://example.com/clean\">\n</head><body>hi</body></html>\n";
const BROKEN_HEAD: &str = "\n<html><head>\n<title>Broken</title>\n<script>ignore()</script>\n<div>oops</div>\n<link rel=\"canonical\" href=\"https://example.com/broken\">\n</head><body>hi</body></html>\n";

#[test]
fn clean_head_reports_nothing_outside_it() {
    let pos = parse(CLEAN_HEAD, "https://example.com/clean").position;
    assert_eq!(pos.canonical_outside_head, Some(false));
    assert_eq!(pos.title_outside_head, Some(false));
    assert!(pos.invalid_head_elements.is_empty());
    assert_eq!((pos.head_count, pos.body_count), (1, 1));
    assert!(!pos.head_not_first);
}

#[test]
fn canonical_after_a_stray_element_is_read_as_outside_head() {
    let page = parse(BROKEN_HEAD, "https://example.com/broken");
    assert_eq!(page.position.canonical_outside_head, Some(true));
    assert_eq!(page.position.invalid_head_elements, ["div"]);
    assert_eq!(
        page.canonical.as_deref(),
        Some("https://example.com/broken")
    );
}

#[test]
fn absent_element_is_neither_inside_nor_outside_head() {
    let pos = parse(
        "<html><head></head><body>no title, no canonical</body></html>",
        "https://example.com/x",
    )
    .position;
    assert_eq!(pos.title_outside_head, None);
    assert_eq!(pos.canonical_outside_head, None);
    assert_eq!(pos.meta_description_outside_head, None);
    assert_eq!(pos.directives_outside_head, None);
    assert_eq!(pos.hreflang_outside_head, None);
}

#[test]
fn directives_and_hreflang_outside_head() {
    let html = r#"<html><head><title>T</title><div>oops</div><meta name="robots" content="noindex"><link rel="alternate" hreflang="fr" href="/fr"></head><body>hi</body></html>"#;
    let pos = parse(html, "https://example.com/x").position;
    assert_eq!(pos.directives_outside_head, Some(true));
    assert_eq!(pos.hreflang_outside_head, Some(true));
    assert_eq!(pos.invalid_head_elements, ["div"]);
}

#[test]
fn valid_head_only_elements_never_close_head() {
    let html = r#"<html><head><base href="/"><title>T</title><style>a{color:red}</style><noscript><img src="x"></noscript><link rel="canonical" href="/c"></head><body>hi</body></html>"#;
    let pos = parse(html, "https://example.com/x").position;
    assert_eq!(pos.canonical_outside_head, Some(false));
    assert!(pos.invalid_head_elements.is_empty());
}

#[test]
fn head_and_body_counts() {
    let two_heads = r#"<html><head><title>T1</title></head><head><meta name="description" content="d"></head><body>a</body></html>"#;
    assert_eq!(
        parse(two_heads, "https://example.com/x")
            .position
            .head_count,
        2
    );
    let two_bodies = "<html><head><title>T</title></head><body>a</body><body>b</body></html>";
    assert_eq!(
        parse(two_bodies, "https://example.com/x")
            .position
            .body_count,
        2
    );
    // from ...::test_missing_head_or_body_is_zero_not_none
    assert_eq!(
        parse("<html><body>hi</body></html>", "https://example.com/x")
            .position
            .head_count,
        0
    );
    assert_eq!(
        parse(
            "<html><head><title>T</title></head></html>",
            "https://example.com/x"
        )
        .position
        .body_count,
        0
    );
}

#[test]
fn head_not_first() {
    let malformed =
        "<body><p>early</p></body><html><head><title>T</title></head><body><p>a</p></body></html>";
    assert!(
        parse(malformed, "https://example.com/x")
            .position
            .head_not_first
    );
    // from ...::test_head_not_first_is_false_when_there_is_no_head_to_misplace
    assert!(
        !parse("<html><body>hi</body></html>", "https://example.com/x")
            .position
            .head_not_first
    );
}

#[test]
fn invalid_head_elements_read_the_literal_source_span() {
    assert_eq!(
        invalid_head_elements("<head><title>T</title><p>x</p></head>"),
        ["p"]
    );
    assert!(invalid_head_elements("<head><title>T</title></head>").is_empty());
    assert!(invalid_head_elements("<body>no head tag here</body>").is_empty());
}

#[test]
fn look_alike_text_is_not_an_invalid_head_element() {
    let raw_text_head = concat!(
        "<head>\n",
        "  <script>const t = \"<div class='card'>fake</div>\";</script>\n",
        "  <style>.card::before { content: \"<div>\"; }</style>\n",
        "  <title>Has a literal &lt;div&gt; look-alike: <div></title>\n",
        "  <!-- a comment mentioning <div> -->\n",
        "  <meta name=\"x\" content=\"looks like <div> but is an attribute value\">\n",
        "</head>\n"
    );
    assert!(invalid_head_elements(raw_text_head).is_empty());
    assert!(
        invalid_head_elements(r#"<head><style>.x::before{content:"<div>"}</style></head>"#)
            .is_empty()
    );
    assert!(invalid_head_elements(
        "<head><title>Guide to &lt;div&gt; and <div> tags</title></head>"
    )
    .is_empty());
    assert!(invalid_head_elements("<head><!-- stray <div> mentioned here --></head>").is_empty());
    assert!(invalid_head_elements(
        r#"<head><meta name="d" content="a <div> in an attribute"></head>"#
    )
    .is_empty());
    assert!(invalid_head_elements(
        "<head><template><div>inert fragment content</div></template></head>"
    )
    .is_empty());
    // positive controls
    assert_eq!(
        invalid_head_elements("<head><title>T</title><div>real stray element</div></head>"),
        ["div"]
    );
    let combined = raw_text_head.replace("</head>", "<div>real</div></head>");
    assert_eq!(invalid_head_elements(&combined), ["div"]);
}

#[test]
fn duplicate_ids_are_bounded_dom_observations_not_template_markup() {
    let page = parse(
        r#"<div id="same"></div><p id="same"></p><template><b id="same"></b></template>"#,
        "https://example.com/",
    );
    assert_eq!(page.duplicate_ids.len(), 1);
    assert_eq!(
        (
            page.duplicate_ids[0].id.as_str(),
            page.duplicate_ids[0].count
        ),
        ("same", 2)
    );
}

// ---- the reference suite ----

#[test]
fn charset_detection() {
    let html = "<html><head>\n<title>Test page</title>\n<meta name=\"description\" content=\"Learn about charset encoding issues in old browsers.\">\n</head><body><h1>Hi</h1><p>content</p></body></html>";
    assert_eq!(parse(html, "https://example.com/page").charset, None);
    // from ...::test_charset_still_finds_real_meta_charset_attribute
    assert_eq!(
        document_charset(r#"<meta charset="utf-8">"#).as_deref(),
        Some("utf-8")
    );
    // from ...::test_charset_still_finds_http_equiv_content_type
    assert_eq!(
        document_charset(r#"<meta http-equiv="Content-Type" content="text/html; charset=utf-8">"#)
            .as_deref(),
        Some("utf-8")
    );
    // from ...::test_charset_prefers_real_meta_over_unrelated_mention_earlier_in_head
    let mixed = "<html><head>\n<meta name=\"description\" content=\"Uses charset weirdly in this description.\">\n<meta charset=\"utf-8\">\n</head><body>hi</body></html>";
    assert_eq!(document_charset(mixed).as_deref(), Some("utf-8"));
}

#[test]
fn directives_outside_head_needs_every_instance_outside() {
    let html = "<html><head>\n<meta name=\"robots\" content=\"noindex\">\n<div>oops</div>\n<meta name=\"robots\" content=\"index,follow\">\n</head><body>hi</body></html>";
    let soup = parse_document(html);
    assert_ne!(
        document_position(&soup, html).directives_outside_head,
        Some(true)
    );
    // from ...::test_directives_outside_head_true_when_all_instances_outside_head
    let outside =
        "<html><head></head><body>\n<meta name=\"robots\" content=\"noindex\">\n</body></html>";
    assert_eq!(
        document_position(&parse_document(outside), outside).directives_outside_head,
        Some(true)
    );
}

#[test]
fn only_http_links_are_ordinary_links() {
    let html = r#"<html><body><a href="sms:+15551234567">Text us</a><a href="skype:live.foo?call">Call on Skype</a><a href="mailto:a@b.com">Mail (control)</a></body></html>"#;
    assert!(parse(html, "https://example.com/page").links.is_empty());
    // from ...::test_whatsapp_scheme_link_never_reads_external_via_fake_hostname
    let wa = r#"<a href="whatsapp://send?phone=15551234567">WhatsApp</a>"#;
    assert!(parse(wa, "https://example.com/page").links.is_empty());
    // from ...::test_normal_http_links_still_extracted_with_correct_external_flag
    let normal = r#"<html><body><a href="https://other.example.org/x">external</a><a href="/internal">internal</a></body></html>"#;
    let links = parse(normal, "https://example.com/page").links;
    let by_raw = |raw: &str| links.iter().find(|l| l.raw_href == raw).map(|l| l.external);
    assert_eq!(by_raw("https://other.example.org/x"), Some(true));
    assert_eq!(by_raw("/internal"), Some(false));
}

// ---- the reference suite (parser-level cases) ----

#[test]
fn raw_rel_and_href_preserve_malformed_separators() {
    let base = "https://example.test/";
    let page = parse(
        r#"<html><head><link rel="canonical, alternate" href=" /preferred/ "></head></html>"#,
        base,
    );
    assert_eq!(page.canonical, None);
    assert_eq!(page.head_links.len(), 1);
    assert_eq!(page.head_links[0]["rel"], "canonical, alternate");
    assert_eq!(page.head_links[0]["href"], " /preferred/ ");
    assert_eq!(
        page.head_links[0]["target"],
        "https://example.test/preferred/"
    );
    assert!(!spiderhead_parse::parser::shared::rel_has(
        Some("canonical\u{a0}alternate"),
        "canonical"
    ));
    assert!(spiderhead_parse::parser::shared::rel_has(
        Some("CANONICAL\talternate"),
        "canonical"
    ));
}

#[test]
fn unknown_unicode_relation_keywords_are_not_canonicals() {
    for relation in [
        "canon\u{131}cal",
        "canon\u{130}cal",
        "CANON\u{131}CAL",
        "custom-relation",
    ] {
        let html = format!(
            r#"<html><head><link rel="{relation}" href="https://example.test/"></head></html>"#
        );
        let page = parse(&html, "https://example.test/");
        assert!(
            page.canonical.is_none() && page.head_links.is_empty(),
            "{relation}"
        );
    }
}

#[test]
fn canonical_alternate_version_attributes_are_kept() {
    for (attribute, value) in [
        ("hreflang", "en-GB"),
        ("lang", "en"),
        ("media", "screen"),
        ("type", "text/html"),
        ("hreflang", ""),
    ] {
        let html = format!(
            r#"<html><head><link rel="canonical" href="https://example.test/" {attribute}="{value}"></head></html>"#
        );
        let page = parse(&html, "https://example.test/");
        assert_eq!(page.canonical.as_deref(), Some("https://example.test/"));
        assert_eq!(page.head_links[0][attribute], value);
    }
}

#[test]
fn non_html_whitespace_does_not_create_hreflang_declarations() {
    let html = "<html><head><link rel=\"alternate\u{a0}custom\" hreflang=\"en\" href=\"https://example.test/\"></head></html>";
    assert!(parse(html, "https://example.test/").hreflang.is_empty());
}

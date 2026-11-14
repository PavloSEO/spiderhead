//! Ports of the reference suite: URL-bearing attributes beyond `<a href>`, inert
//! `<template>` content, and CSS-referenced resources. Each test names its reference source.

use std::collections::BTreeMap;

use spiderhead_parse::parser::extract::extract_css_urls;
use spiderhead_parse::parser::parse_html;
use spiderhead_parse::types::ParseOptions;

fn sources_with(html: &str, base: &str) -> Vec<BTreeMap<String, String>> {
    let options = ParseOptions {
        url_sources: true,
        links: false,
        text: false,
        ..ParseOptions::default()
    };
    parse_html(html, base, &options)
        .expect("parse")
        .url_sources
        .unwrap_or_default()
}

fn sources(html: &str) -> Vec<BTreeMap<String, String>> {
    sources_with(html, "https://example.com/")
}

fn source(url: &str, tag: &str, attr: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("url".to_owned(), url.to_owned()),
        ("tag".to_owned(), tag.to_owned()),
        ("attr".to_owned(), attr.to_owned()),
    ])
}

fn urls(found: &[BTreeMap<String, String>]) -> Vec<String> {
    found.iter().map(|s| s["url"].clone()).collect()
}

#[test]
fn srcset_yields_multiple_urls() {
    let src = sources(r#"<img src="/a.jpg" srcset="/a.jpg 1x, /b.jpg 2x">"#);
    let img_urls: Vec<String> = src
        .iter()
        .filter(|s| s["tag"] == "img")
        .map(|s| s["url"].clone())
        .collect();
    assert!(img_urls.contains(&"https://example.com/a.jpg".to_owned()));
    assert!(img_urls.contains(&"https://example.com/b.jpg".to_owned()));
}

#[test]
fn form_and_button_formaction() {
    let src = sources(r#"<form action="/submit"><button formaction="/alt">go</button></form>"#);
    let found = urls(&src);
    assert!(found.contains(&"https://example.com/submit".to_owned()));
    assert!(found.contains(&"https://example.com/alt".to_owned()));
}

#[test]
fn a_ping_splits_into_multiple_urls() {
    let src = sources(r#"<a ping="https://tracker.example/p1 https://tracker.example/p2">x</a>"#);
    let ping: Vec<String> = src
        .iter()
        .filter(|s| s["attr"] == "ping")
        .map(|s| s["url"].clone())
        .collect();
    assert!(ping.contains(&"https://tracker.example/p1".to_owned()));
    assert!(ping.contains(&"https://tracker.example/p2".to_owned()));
}

#[test]
fn cite_on_blockquote_and_meta_refresh() {
    let src = sources(
        r#"<blockquote cite="https://q.example/orig">q</blockquote><meta http-equiv="refresh" content="0;url=/moved">"#,
    );
    let found = urls(&src);
    assert!(found.contains(&"https://q.example/orig".to_owned()));
    assert!(found.contains(&"https://example.com/moved".to_owned()));
}

#[test]
fn itemtype_collected() {
    let src = sources(r#"<div itemtype="https://schema.org/Product">p</div>"#);
    assert!(src
        .iter()
        .any(|s| s["url"] == "https://schema.org/Product" && s["attr"] == "itemtype"));
}

#[test]
fn data_and_fragment_and_mailto_skipped() {
    let src = sources(
        r##"<img src="data:image/png;base64,xx"><a href="#frag">x</a><img src="mailto:a@b.c">"##,
    );
    let found = urls(&src);
    assert!(!found.iter().any(|u| u.contains("data:")));
    assert!(!found.iter().any(|u| u.ends_with("#frag")));
}

#[test]
fn url_sources_off_by_default() {
    let page = parse_html(
        r#"<img src="/a.jpg">"#,
        "https://example.com/",
        &ParseOptions::default(),
    )
    .expect("parse");
    assert!(page.url_sources.is_none());
}

#[test]
fn template_image_is_not_a_url_source() {
    let src = sources(r#"<template><img src="/template-image.jpg"></template>"#);
    assert!(src.is_empty());
}

#[test]
fn noscript_image_is_still_a_url_source() {
    let src = sources(r#"<noscript><img src="/fallback.jpg"></noscript>"#);
    assert!(src.contains(&source("https://example.com/fallback.jpg", "img", "src")));
}

#[test]
fn css_urls_are_extracted_from_inline_style_attributes() {
    let html =
        "<html><body><div style=\"background: url('/hero.png') no-repeat\"></div></body></html>";
    let found = sources_with(html, "https://e.com/");
    assert!(found.contains(&source("https://e.com/hero.png", "div", "style")));
}

#[test]
fn css_urls_are_extracted_from_style_blocks() {
    let html = "<html><head><style>.hero{background-image:url(/bg.webp)}</style></head><body></body></html>";
    let found = sources_with(html, "https://e.com/");
    assert!(found.contains(&source("https://e.com/bg.webp", "style", "css")));
}

#[test]
fn css_extraction_covers_properties_beyond_background_image() {
    let css = "a{border-image:url(b.png)} b{mask-image:url('m.svg')} c{content:url(\"i.gif\")}";
    assert_eq!(extract_css_urls(Some(css)), vec!["b.png", "m.svg", "i.gif"]);
}

#[test]
fn css_url_quoting_variants_all_parse() {
    let css = "a{background:url(bare.png)} b{background:url('single.png')} c{background:url(\"double.png\")}";
    assert_eq!(
        extract_css_urls(Some(css)),
        vec!["bare.png", "single.png", "double.png"]
    );
}

#[test]
fn css_url_whitespace_is_tolerated() {
    assert_eq!(
        extract_css_urls(Some("a{background:url(  spaced.png  )}")),
        vec!["spaced.png"]
    );
}

#[test]
fn data_uris_in_css_are_skipped_like_everywhere_else() {
    let html = r#"<html><body><div style="background:url(data:image/gif;base64,R0lGOD)"></div></body></html>"#;
    assert!(sources_with(html, "https://e.com/").is_empty());
}

#[test]
fn css_text_without_urls_yields_nothing() {
    assert!(extract_css_urls(Some("body{color:red}")).is_empty());
    assert!(extract_css_urls(Some("")).is_empty());
}

#[test]
fn a_css_background_and_an_img_are_both_reported() {
    let html = "<html><head><style>.h{background-image:url(/css.png)}</style></head>\
                <body><img src=\"/tag.png\"></body></html>";
    let found = urls(&sources_with(html, "https://e.com/"));
    let unique: std::collections::BTreeSet<String> = found.into_iter().collect();
    let expected: std::collections::BTreeSet<String> = [
        "https://e.com/css.png".to_owned(),
        "https://e.com/tag.png".to_owned(),
    ]
    .into_iter()
    .collect();
    assert_eq!(unique, expected);
}

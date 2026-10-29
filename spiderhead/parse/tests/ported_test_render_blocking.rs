//! Render-blocking head resources, parser part (issue #1022).
//! no audit, crawl or store. The rest is listed as deferred in the slice report.

use spiderhead_parse::parse_html;
use spiderhead_parse::types::ParseOptions;

const HEAD_HTML: &str = r#"<!doctype html><html><head>
<script src="/a.js"></script>
<script src="/b.js" defer></script>
<script type="module" src="/m.js"></script>
<link rel="stylesheet" href="/c.css">
<link rel="stylesheet" href="/print.css" media="print">
<template><script src="/t.js"></script></template>
</head><body><p>x</p></body></html>"#;

#[test]
fn parse_html_lists_only_blocking_head_resources() {
    let parsed = parse_html(
        HEAD_HTML,
        "https://example.com/page",
        &ParseOptions::default(),
    )
    .expect("parse_html");
    assert_eq!(
        parsed.render_blocking,
        vec![
            "https://example.com/a.js".to_owned(),
            "https://example.com/c.css".to_owned(),
        ]
    );
}

#[test]
fn raw_multitoken_stylesheet_rel_remains_render_blocking() {
    for rel in [
        "stylesheet preload",
        "stylesheet custom",
        "STYLESHEET\tpreload",
    ] {
        let html = format!(r#"<html><head><link rel="{rel}" href="/style.css"></head></html>"#);
        let parsed = parse_html(&html, "https://example.com/", &ParseOptions::default())
            .expect("parse_html");
        assert_eq!(
            parsed.render_blocking,
            vec!["https://example.com/style.css".to_owned()],
            "rel {rel:?}"
        );
    }
}

#[test]
fn parse_html_without_head_resources_is_empty() {
    let parsed = parse_html(
        "<html><head></head><body>x</body></html>",
        "https://example.com/",
        &ParseOptions::default(),
    )
    .expect("parse_html");
    assert!(parsed.render_blocking.is_empty());
}

#[test]
fn render_blocking_survives_disabled_metadata() {
    let options = ParseOptions {
        meta: false,
        ..ParseOptions::default()
    };
    let parsed = parse_html(HEAD_HTML, "https://example.com/page", &options).expect("parse_html");
    assert_eq!(
        parsed.render_blocking,
        vec![
            "https://example.com/a.js".to_owned(),
            "https://example.com/c.css".to_owned(),
        ]
    );
}

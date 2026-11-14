//! fetch or checks crate. The other cases in that file (redirects, robots, sitemap, hreflang
//! validation, optimizer, downloader, clusterer) are deferred; see the port report.

use std::collections::BTreeMap;

use serde_json::json;
use spiderhead_parse::models::{FormInfo, ParsedPage};
use spiderhead_parse::parse_html;
use spiderhead_parse::types::ParseOptions;

fn parse(html: &str, url: &str) -> ParsedPage {
    parse_html(html, url, &ParseOptions::default()).expect("parse_html")
}

const BASICS_HTML: &str = r#"<html><head><title>Hi</title>
<meta name="description" content="Desc">
<link rel="canonical" href="/canon">
<meta property="og:title" content="OG">
</head><body><h1>Head1</h1><h2>Head2</h2>
<a href="/in">internal</a>
<a href="https://other.tld/out" rel="nofollow">ext</a>
<a href="mailto:x@y.z">mail</a>
<p>hello world foo</p></body></html>"#;

#[test]
fn parse_html_basics() {
    let r = parse(BASICS_HTML, "https://site.tld/page");
    assert_eq!(r.title.as_deref(), Some("Hi"));
    assert_eq!(r.meta_description.as_deref(), Some("Desc"));
    assert_eq!(r.canonical.as_deref(), Some("https://site.tld/canon"));
    assert_eq!(r.headings.get("h1"), Some(&vec!["Head1".to_string()]));
    assert!(r.word_count >= 3);
}

#[test]
fn parse_html_links_classified() {
    let r = parse(BASICS_HTML, "https://site.tld/page");
    let hrefs: Vec<&str> = r.links.iter().map(|ln| ln.href.as_str()).collect();
    assert!(hrefs.contains(&"https://site.tld/in"));
    assert!(r.links.iter().any(|ln| ln.nofollow));
    assert!(!r.links.iter().any(|ln| ln.href.contains("mailto")));
}

#[test]
fn parse_html_options_off() {
    let options = ParseOptions {
        links: false,
        text: false,
        ..ParseOptions::default()
    };
    let r = parse_html(BASICS_HTML, "https://site.tld/page", &options).expect("parse_html");
    assert!(r.links.is_empty());
}

#[test]
fn template_link_is_not_extracted() {
    let html = r#"<html><body><template><a href="/template-link">gone</a></template><p>Real content paragraph with enough words to be meaningful for a test.</p></body></html>"#;
    let r = parse(html, "https://site.tld/page");
    assert!(r.links.is_empty());
}

#[test]
fn noscript_link_is_still_extracted() {
    let html = r#"<html><body><noscript><a href="/fallback-link">fb</a></noscript></body></html>"#;
    let r = parse(html, "https://site.tld/page");
    let hrefs: Vec<&str> = r.links.iter().map(|ln| ln.href.as_str()).collect();
    assert!(hrefs.contains(&"https://site.tld/fallback-link"));
}

#[test]
fn template_only_metadata_never_overrides_or_fabricates_live_values() {
    let html = concat!(
        "<html><head>",
        r#"<link rel="canonical" href="/real-canonical">"#,
        r#"<meta name="description" content="real description">"#,
        r#"<meta name="robots" content="index, follow">"#,
        r#"<meta property="og:title" content="real preview">"#,
        r#"<script type="application/ld+json">{"@type": "Article"}</script>"#,
        "<template>",
        r#"<link rel="canonical" href="/never-canonical">"#,
        r#"<meta name="description" content="template description">"#,
        r#"<meta name="robots" content="noindex">"#,
        r#"<meta property="og:title" content="template preview">"#,
        r#"<script type="application/ld+json">{"@type": "Product"}</script>"#,
        r#"<form method="post" action="/never-submitted"><input type="password"></form>"#,
        "</template>",
        "</head><body><p>Visible page</p></body></html>",
    );
    let r = parse(html, "https://example.com/page");
    assert_eq!(
        r.canonical.as_deref(),
        Some("https://example.com/real-canonical")
    );
    assert_eq!(r.meta_description.as_deref(), Some("real description"));
    assert_eq!(r.robots.as_deref(), Some("index, follow"));
    let og: BTreeMap<String, String> = [("og:title".to_string(), "real preview".to_string())]
        .into_iter()
        .collect();
    assert_eq!(r.og, og);
    assert_eq!(r.jsonld, vec![json!({"@type": "Article"})]);
    assert!(r.forms.is_empty());
}

#[test]
fn template_only_form_is_not_extracted() {
    let html = concat!(
        "<html><body>",
        "<template>",
        r#"<form method="post" action="http://collector.invalid/submit">"#,
        r#"<input type="password"></form>"#,
        "</template>",
        r#"<form method="get" action="/search"></form>"#,
        "</body></html>",
    );
    let r = parse(html, "https://example.com/login");
    assert_eq!(
        r.forms,
        vec![FormInfo {
            method: "get".to_string(),
            action: "https://example.com/search".to_string(),
            has_password: false,
        }]
    );
}

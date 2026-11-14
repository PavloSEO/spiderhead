//! Parser-side part of the reference suite.
//!
//! The reference test checks at runtime that the dict returned by `parse_html`
//! carries only keys its TypedDict declares. In Rust the return type is the
//! `ParsedPage` struct, so that property is enforced by the compiler and has no
//! runtime counterpart here; this file keeps the behavioural assertion on the
//! same sample page. The robots half and the MCP handler half are deferred.

use spiderhead_parse::parse_html;
use spiderhead_parse::types::ParseOptions;

const SAMPLE_HTML: &str = r#"
<html><head>
<title>Sample</title>
<meta name="description" content="A sample page">
<link rel="canonical" href="https://site.tld/page">
</head><body>
<h1>Hello</h1>
<a href="/other" rel="nofollow">Other</a>
</body></html>
"#;

#[test]
fn parse_html_only_returns_keys_parsed_page_declares() {
    let result = parse_html(
        SAMPLE_HTML,
        "https://site.tld/page",
        &ParseOptions::default(),
    )
    .expect("parse_html");
    assert_eq!(result.title.as_deref(), Some("Sample"));
    assert_eq!(result.meta_description.as_deref(), Some("A sample page"));
}

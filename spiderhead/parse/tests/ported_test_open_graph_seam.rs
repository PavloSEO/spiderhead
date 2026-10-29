//!
//! Only the test that needs nothing but `parse_html` is ported here. The collector half
//! (`crawl.collect._record_from_parsed`), the crawl, the scan artifact and the analyzer
//! frame belong to crates that are not ported yet (crawl, store `pages`, checks/audit).

use std::collections::BTreeMap;

use spiderhead_parse::parse_html;
use spiderhead_parse::types::ParseOptions;

#[test]
fn the_parser_keys_open_graph_by_its_full_property_name() {
    let html = concat!(
        r#"<meta property="og:title" content="X">"#,
        r#"<meta property="og:description" content="Y">"#,
        r#"<meta property="og:image" content="https://cdn.example/z.png">"#,
        r#"<meta property="og:url" content="https://example.com/w">"#,
    );
    let parsed =
        parse_html(html, "https://example.com/", &ParseOptions::default()).expect("parse_html");

    let expected: BTreeMap<String, String> = [
        ("og:title", "X"),
        ("og:description", "Y"),
        ("og:image", "https://cdn.example/z.png"),
        ("og:url", "https://example.com/w"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    assert_eq!(parsed.og, expected);
    for unprefixed in ["title", "description", "image", "url"] {
        assert!(
            !parsed.og.contains_key(unprefixed),
            "unprefixed key {unprefixed:?} must be absent"
        );
    }
}

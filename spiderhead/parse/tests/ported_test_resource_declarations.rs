//! Direct script/stylesheet declaration inventory, ported from the reference suite.
//!
//! The two `asset_weight` tests (`_discover_resources`) belong to the checks crate and are
//! deferred there; this file covers the parser-side inventory only.

use serde_json::{json, Value};
use spiderhead_parse::dom::parse_document;
use spiderhead_parse::parse_html;
use spiderhead_parse::parser::extract::extract_resource_declarations;
use spiderhead_parse::types::ParseOptions;

const PAGE: &str = "https://example.test/path/page";

fn parsed_with_cap(html: &str, cap: usize) -> spiderhead_parse::models::ParsedPage {
    let options = ParseOptions {
        resource_declarations: true,
        max_resource_declarations: Some(cap),
        ..ParseOptions::default()
    };
    parse_html(html, PAGE, &options).expect("parse_html")
}

fn declarations_json(parsed: &spiderhead_parse::models::ParsedPage) -> Value {
    serde_json::to_value(
        parsed
            .resource_declarations
            .as_ref()
            .expect("declarations present"),
    )
    .expect("serialise declarations")
}

#[test]
fn declarations_preserve_document_order_duplicates_and_raw_attribute_spelling() {
    let parsed = parsed_with_cap(
        r#"<script src=" /app.js "></script><link rel="preload StyleSheet alternate" href="styles/site.css"><script src=" /app.js "></script>"#,
        10,
    );
    assert_eq!(
        declarations_json(&parsed),
        json!([
            {
                "kind": "script",
                "url": "https://example.test/app.js",
                "raw_url": " /app.js ",
            },
            {
                "kind": "stylesheet",
                "url": "https://example.test/path/styles/site.css",
                "raw_url": "styles/site.css",
            },
            {
                "kind": "script",
                "url": "https://example.test/app.js",
                "raw_url": " /app.js ",
            },
        ])
    );
    assert_eq!(parsed.resource_declarations_omitted, Some(0));
}

#[test]
fn declarations_follow_base_skip_templates_and_keep_non_http_or_empty_values() {
    let parsed = parsed_with_cap(
        r#"<base href="https://cdn.example.test/assets/"><template><script src="hidden.js"></script></template><script src="module.js"></script><script src="data:text/javascript,x"></script><script src="ftp://example.test/file.js"></script><script src=""></script><link rel="notstylesheet" href="wrong.css"><link rel="StyleSheet" href="theme.css">"#,
        10,
    );
    assert_eq!(
        declarations_json(&parsed),
        json!([
            {
                "kind": "script",
                "url": "https://cdn.example.test/assets/module.js",
                "raw_url": "module.js",
            },
            {
                "kind": "script",
                "url": "data:text/javascript,x",
                "raw_url": "data:text/javascript,x",
            },
            {
                "kind": "script",
                "url": "ftp://example.test/file.js",
                "raw_url": "ftp://example.test/file.js",
            },
            {
                "kind": "stylesheet",
                "url": "https://cdn.example.test/assets/theme.css",
                "raw_url": "theme.css",
            },
        ])
    );
}

#[test]
fn declaration_cap_reports_every_omitted_valid_occurrence() {
    let parsed = parsed_with_cap(
        r#"<script src="one.js"></script><link rel="stylesheet" href="two.css"><script src="three.js"></script>"#,
        2,
    );
    let raws: Vec<&str> = parsed
        .resource_declarations
        .as_ref()
        .expect("declarations present")
        .iter()
        .map(|item| item.raw_url.as_str())
        .collect();
    assert_eq!(raws, ["one.js", "two.css"]);
    assert_eq!(parsed.resource_declarations_omitted, Some(1));
}

#[test]
fn declaration_opt_in_requires_a_finite_nonnegative_cap_and_is_off_by_default() {
    // Off by default: no declarations field at all.
    let default_off = parse_html(
        r#"<script src="app.js"></script>"#,
        "https://x.test/",
        &ParseOptions::default(),
    )
    .expect("parse_html");
    assert!(default_off.resource_declarations.is_none());

    // On without a cap: refused with the reference message's key term.
    let without_cap = ParseOptions {
        resource_declarations: true,
        ..ParseOptions::default()
    };
    let error = parse_html(
        r#"<script src="app.js"></script>"#,
        "https://x.test/",
        &without_cap,
    )
    .expect_err("cap is required");
    assert!(error.to_string().contains("max_resource_declarations"));

    // The reference `-1` case is unrepresentable: the cap is `Option<usize>`, so a negative cap
    // cannot be constructed. Only the missing-cap rejection is observable here.
}

#[test]
fn direct_helper_keeps_its_own_bounded_omission_count() {
    let soup = parse_document(r#"<script src="one.js"></script><script src="two.js"></script>"#);
    let (declarations, omitted) =
        extract_resource_declarations(&soup, "https://example.test/", Some(0));
    assert!(declarations.is_empty());
    assert_eq!(omitted, 2);
}

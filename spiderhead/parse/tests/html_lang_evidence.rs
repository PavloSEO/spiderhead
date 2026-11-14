//! Ports of the reference suite (epic #1002): the declared `<html lang>` is kept
//! as written, and an absent or empty attribute is distinguishable from a declared one.

use std::collections::BTreeMap;

use spiderhead_parse::parser::parse_html;
use spiderhead_parse::types::ParseOptions;

fn lang(html: &str) -> BTreeMap<String, String> {
    parse_html(html, "https://example.com/", &ParseOptions::default())
        .expect("parse")
        .html_lang
}

fn declared(declared: &str, primary: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("declared".to_owned(), declared.to_owned()),
        ("declared_primary".to_owned(), primary.to_owned()),
    ])
}

#[test]
fn declared_lang_is_kept_and_primary_subtag_derived() {
    assert_eq!(
        lang(r#"<html lang="en-US"><body>x</body></html>"#),
        declared("en-US", "en")
    );
}

#[test]
fn missing_lang_attribute_is_empty_not_declared() {
    assert_eq!(lang("<html><body>x</body></html>"), declared("", ""));
}

#[test]
fn empty_lang_attribute_is_empty_not_declared() {
    assert_eq!(
        lang(r#"<html lang=""><body>x</body></html>"#),
        declared("", "")
    );
}

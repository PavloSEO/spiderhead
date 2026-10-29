//!
//! The reference test serves pages from the shop fixture and parses them with the HTML
//! parser. The served bodies are captured once from the oracle fixture into
//! `tests/fixtures/shop_pagination/` (`<version>_page<n>.html`); these tests parse those
//! bytes and assert the same rel/target pairs as the reference test.

use std::collections::HashMap;
use std::path::PathBuf;

use spiderhead_parse::parse_html;
use spiderhead_parse::types::ParseOptions;

const BASE: &str = "https://example.test";
const SOURCE: &str = "/catalog/stulya/?page=2";

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/shop_pagination")
        .join(name)
}

fn body(version: &str, page: u32) -> String {
    let path = fixture_path(&format!("{version}_page{page}.html"));
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

/// reference: `{row["rel"]: row["target"] for row in parsed["head_links"]}` (last row wins).
fn annotations(html: &str, url: &str) -> HashMap<String, String> {
    let page = parse_html(html, url, &ParseOptions::default()).expect("parse_html");
    page.head_links
        .iter()
        .map(|row| (row["rel"].clone(), row["target"].clone()))
        .collect()
}

fn check_version(version: &str, next_page: u32) {
    let source_url = format!("{BASE}{SOURCE}");
    let annotations_source = annotations(&body(version, 2), &source_url);
    assert_eq!(
        annotations_source.get("next").map(String::as_str),
        Some(format!("{BASE}/catalog/stulya/?page={next_page}").as_str())
    );
    assert_eq!(
        annotations_source.get("prev").map(String::as_str),
        Some(format!("{BASE}/catalog/stulya/").as_str())
    );

    let target = format!("/catalog/stulya/?page={next_page}");
    let annotations_target = annotations(&body(version, next_page), &format!("{BASE}{target}"));
    assert_eq!(
        annotations_target.get("prev").map(String::as_str),
        Some(format!("{BASE}/catalog/stulya/?page={}", next_page - 1).as_str())
    );
}

#[test]
fn shop_serves_pagination_annotations_and_versioned_sequence_defect_v1() {
    check_version("v1", 4);
}

#[test]
fn shop_serves_pagination_annotations_and_versioned_sequence_defect_v2() {
    check_version("v2", 3);
}

#[test]
fn shop_serves_pagination_annotations_and_versioned_sequence_defect_v3() {
    check_version("v3", 3);
}

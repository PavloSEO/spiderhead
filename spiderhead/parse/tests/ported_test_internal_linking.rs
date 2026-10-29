//! Parser-level facts for link placement, ported from the reference suite.
//! Only the tests that need nothing but `parse_html` / `link_placement` are here; the crawl,
//! click-depth and storage tests belong to later crates (see the report).

use spiderhead_parse::dom::parse_document_lxml;
use spiderhead_parse::parse_html;
use spiderhead_parse::parser::content::{link_placement, MAX_PLACEMENT_ITEMS};
use spiderhead_parse::types::ParseOptions;

/// Same fixture as the reference `PLACEMENT_PAGE`.
const PLACEMENT_PAGE: &str = r#"
<html><body><main>
  <h1><a href="/">Home</a></h1>
  <h2>A section that is only text</h2>
  <h3><a href="/elsewhere">Somewhere else entirely</a></h3>
  <a href="/product-a"><img src="/a.jpg" alt=""></a>
  <a href="/product-b"><img src="/b.jpg" alt="Product B"></a>
  <a href="/product-c" aria-label="Product C"><img src="/c.jpg"></a>
  <a href="/product-d">Product D</a>
  <template><h2><a href="/never">Never rendered</a></h2></template>
</main></body></html>
"#;

const PAGE_URL: &str = "https://example.com/page";

fn placement(html: &str) -> spiderhead_parse::models::LinkPlacement {
    parse_html(html, PAGE_URL, &ParseOptions::default())
        .expect("parse_html")
        .link_placement
        .expect("links are on by default, so placement is recorded")
}

#[test]
fn a_link_wrapped_in_a_heading_is_recorded_with_the_nearest_heading_level() {
    let p = placement(PLACEMENT_PAGE);
    let got: Vec<(u8, &str)> = p
        .in_heading
        .iter()
        .map(|item| (item.level, item.anchor.as_str()))
        .collect();
    assert_eq!(got, vec![(1, "Home"), (3, "Somewhere else entirely")]);
    assert_eq!(p.in_heading_total, 2);
    assert_eq!(p.in_heading[0].destination, "https://example.com/");
}

#[test]
fn only_the_image_link_with_neither_anchor_text_nor_alt_is_recorded() {
    let p = placement(PLACEMENT_PAGE);
    let destinations: Vec<&str> = p
        .image_no_text
        .iter()
        .map(|item| item.destination.as_str())
        .collect();
    assert_eq!(destinations, vec!["https://example.com/product-a"]);
    assert_eq!(p.image_no_text_total, 1);
}

#[test]
fn a_text_link_with_no_image_is_never_an_image_link() {
    let p = placement("<html><body><a href=\"/x\"></a></body></html>");
    assert!(p.image_no_text.is_empty());
    assert!(p.in_heading.is_empty());
}

#[test]
fn a_heading_link_inside_a_template_is_not_in_the_rendered_document() {
    let p = placement(PLACEMENT_PAGE);
    assert!(p
        .in_heading
        .iter()
        .all(|item| item.destination != "https://example.com/never"));
}

#[test]
fn the_stored_lists_are_capped_and_the_total_still_says_how_many_there_were() {
    let many: String = (0..MAX_PLACEMENT_ITEMS + 7)
        .map(|n| format!("<a href=\"/n{n}\">n{n}</a>"))
        .collect();
    let p = placement(&format!("<html><body><h2>{many}</h2></body></html>"));
    assert_eq!(p.in_heading.len(), MAX_PLACEMENT_ITEMS);
    assert_eq!(p.in_heading_total, (MAX_PLACEMENT_ITEMS + 7) as u64);
}

#[test]
fn switching_link_parsing_off_records_no_placement_rather_than_an_empty_one() {
    let options = ParseOptions {
        links: false,
        ..ParseOptions::default()
    };
    let parsed = parse_html(PLACEMENT_PAGE, PAGE_URL, &options).expect("parse_html");
    assert!(parsed.link_placement.is_none());
}

#[test]
fn link_placement_is_callable_on_its_own_tree() {
    let soup = parse_document_lxml(PLACEMENT_PAGE);
    let p = link_placement(&soup, PAGE_URL, PAGE_URL);
    assert_eq!(p.in_heading_total, 2);
    assert_eq!(p.image_no_text_total, 1);
}

#[test]
fn placement_destinations_are_resolved_the_way_the_link_graph_resolves_them() {
    let html = concat!(
        r#"<html><head><base href="https://example.com/base/"></head><body>"#,
        r#"<h2><a href="child">Child</a></h2></body></html>"#
    );
    let parsed = parse_html(html, PAGE_URL, &ParseOptions::default()).expect("parse_html");
    let placement = parsed.link_placement.expect("placement");
    assert_eq!(placement.in_heading[0].destination, parsed.links[0].href);
}

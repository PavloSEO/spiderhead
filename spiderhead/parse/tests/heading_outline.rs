//! Ports of the pure parser tests in the reference suite (issue #632): the heading
//! outline as a document-order sequence with a page region per heading. The registry-check and
//! storage halves of that reference file need the checks and store crates and are listed in the
//! slice report instead.

use spiderhead_parse::dom::parse_document;
use spiderhead_parse::parse_html;
use spiderhead_parse::parser::content::heading_outline;
use spiderhead_parse::types::{ParseOptions, PositionRule};

// The live shape issue #632 was found on: an article template whose masthead carries the
// headings of other magazines, all of them before the article's H1.
const CHROME_HEAVY: &str = r#"
<html><body>
  <header class="site-header"><h2>Magazine One</h2><h2>Magazine Two</h2></header>
  <nav><h3>Sections</h3></nav>
  <aside><h4>Also read</h4></aside>
  <main><h1>The article itself</h1><h2>A real section</h2></main>
  <footer><h2>About the publisher</h2></footer>
</body></html>
"#;

fn levels_texts(outline: &[spiderhead_parse::models::HeadingRef]) -> Vec<(u8, String)> {
    outline
        .iter()
        .map(|item| (item.level, item.text.clone()))
        .collect()
}

fn pair(level: u8, text: &str) -> (u8, String) {
    (level, text.to_string())
}

fn parse(html: &str, url: &str) -> spiderhead_parse::models::ParsedPage {
    parse_html(html, url, &ParseOptions::default()).expect("parse_html")
}

#[test]
fn the_outline_is_document_order_not_grouped_by_level() {
    let parsed = parse(CHROME_HEAVY, "https://example.com/article");
    assert_eq!(
        levels_texts(&parsed.heading_outline),
        vec![
            pair(2, "Magazine One"),
            pair(2, "Magazine Two"),
            pair(3, "Sections"),
            pair(4, "Also read"),
            pair(1, "The article itself"),
            pair(2, "A real section"),
            pair(2, "About the publisher"),
        ]
    );
    // The grouped view is unchanged and still cannot answer the same question.
    assert_eq!(parsed.headings["h1"], vec!["The article itself"]);
}

#[test]
fn each_heading_carries_the_link_position_taxonomy_region() {
    let regions: Vec<String> = parse(CHROME_HEAVY, "https://x.test/")
        .heading_outline
        .iter()
        .map(|item| item.region.clone())
        .collect();
    assert_eq!(
        regions,
        vec!["header", "header", "nav", "sidebar", "content", "content", "footer"]
    );
}

#[test]
fn a_page_with_no_landmark_and_no_matching_rule_reports_no_region() {
    // Not "content": with the content root falling back to the whole <body>, "not chrome" would
    // be true of every heading on the page and would say nothing at all.
    let html = "<html><body><div class='wrap'><h2>Early</h2><h1>Subject</h1></div></body></html>";
    let outline = parse(html, "https://example.com/x").heading_outline;
    let regions: Vec<&str> = outline.iter().map(|item| item.region.as_str()).collect();
    assert_eq!(regions, vec!["", ""]);
}

#[test]
fn a_site_specific_rule_places_a_heading_the_default_rules_miss() {
    let html = "<html><body><div class='mega'><h2>Menu</h2></div><main><h1>Subject</h1></main></body></html>";
    let options = ParseOptions {
        link_position_rules: Some(vec![PositionRule {
            position: "nav".into(),
            selector: ".mega".into(),
        }]),
        ..ParseOptions::default()
    };
    let outline = parse_html(html, "https://example.com/x", &options)
        .expect("parse_html")
        .heading_outline;
    let pairs: Vec<(String, String)> = outline
        .iter()
        .map(|item| (item.text.clone(), item.region.clone()))
        .collect();
    assert_eq!(
        pairs,
        vec![
            ("Menu".to_string(), "nav".to_string()),
            ("Subject".to_string(), "content".to_string()),
        ]
    );
}

#[test]
fn inert_and_textless_headings_are_left_out_exactly_as_the_grouping_leaves_them() {
    let html = "<html><body><main>
    <template><h2>Never rendered</h2></template>
    <h2>   </h2>
    <h1>Only real heading</h1>
    </main></body></html>";
    let parsed = parse(html, "https://example.com/x");
    assert_eq!(
        levels_texts(&parsed.heading_outline),
        vec![pair(1, "Only real heading")]
    );
    assert!(!parsed.headings.contains_key("h2"));
}

#[test]
fn hidden_and_closed_dialog_headings_are_left_out_of_the_outline() {
    let html = "<html><body><main>
    <h1>Visible page</h1><h2>Visible section</h2>
    <dialog><h2>Call us</h2><h4>Phone form</h4></dialog>
    <section hidden><h2>Hidden promotion</h2><h4>Hidden detail</h4></section>
    <aside aria-hidden=\"true\"><h2>Invisible helper</h2></aside>
    </main></body></html>";
    assert_eq!(
        levels_texts(&parse(html, "https://example.com/x").heading_outline),
        vec![pair(1, "Visible page"), pair(2, "Visible section")]
    );
}

#[test]
fn outline_extraction_can_be_switched_off_with_the_headings_option() {
    let options = ParseOptions {
        headings: false,
        ..ParseOptions::default()
    };
    let parsed = parse_html(CHROME_HEAVY, "https://example.com/x", &options).expect("parse_html");
    assert!(parsed.heading_outline.is_empty());
}

#[test]
fn heading_outline_is_callable_on_its_own_tree() {
    let soup = parse_document(CHROME_HEAVY);
    let outline = heading_outline(&soup, None, None).expect("heading_outline");
    assert_eq!(outline.len(), 7);
}

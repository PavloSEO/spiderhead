//! Ports of the reference suite: a page whose JSON-LD does not compile is not a page
//! without JSON-LD. Each test names its reference origin.

use spiderhead_parse::parse_html;
use spiderhead_parse::schema_org::{check_schema, SchemaSource};
use spiderhead_parse::types::ParseOptions;

// A stray comment, which JSON does not allow, voids the whole block.
const COMMENTED: &str = r##"<html><head><script type="application/ld+json">
{"@context": "https://schema.org", "@type": "FurnitureStore",
 "logo": "https://example.com/logo.png", /* put the logo path here */
 "telephone": "+7 000 000"}
</script></head><body>x</body></html>"##;

const TRAILING_COMMA: &str = r##"<html><head><script type="application/ld+json">
{"@context": "https://schema.org", "@type": "Organization", "name": "Example",}
</script></head><body>x</body></html>"##;

const NO_MARKUP: &str = "<html><head></head><body>x</body></html>";

const VALID: &str = r##"<html><head><script type="application/ld+json">
{"@context": "https://schema.org", "@type": "Organization", "name": "Example"}
</script></head><body>x</body></html>"##;

/// reference `_report`: the schema findings for one page's HTML.
fn report(html: &str) -> Vec<String> {
    check_schema(SchemaSource::Html(html), None, 10.0, None)
        .expect("check_schema")
        .findings
        .unwrap_or_default()
}

fn parse(html: &str) -> spiderhead_parse::models::ParsedPage {
    parse_html(html, "https://example.com/", &ParseOptions::default()).expect("parse_html")
}

// --- the schema findings -------------------------------------------------------------------

#[test]
fn an_invalid_block_is_not_reported_as_no_markup() {
    let findings = report(COMMENTED);
    assert!(!findings
        .iter()
        .any(|line| line.contains("contains no JSON-LD blocks")));
    assert!(findings
        .iter()
        .any(|line| line.contains("cannot be parsed")));
}

#[test]
fn an_empty_page_still_reports_no_markup() {
    assert!(report(NO_MARKUP)
        .iter()
        .any(|line| line == "The page contains no JSON-LD blocks"));
}

#[test]
fn a_valid_block_reports_neither() {
    let findings = report(VALID);
    assert!(!findings.iter().any(|line| line.contains("JSON-LD")));
}

#[test]
fn the_cause_is_named_for_comments() {
    assert!(report(COMMENTED)
        .iter()
        .any(|line| line.contains("permits no comments")));
}

#[test]
fn the_cause_is_named_for_a_trailing_comma() {
    assert!(report(TRAILING_COMMA)
        .iter()
        .any(|line| line.contains("permits no trailing comma")));
}

// --- the parse tool ------------------------------------------------------------------------

#[test]
fn parse_reports_the_invalid_block_rather_than_dropping_it() {
    let parsed = parse(COMMENTED);
    assert!(parsed.jsonld.is_empty());
    assert_eq!(parsed.jsonld_invalid.len(), 1);
    let invalid = &parsed.jsonld_invalid[0];
    assert_eq!(invalid.index, 1);
    assert!(!invalid.error.is_empty());
    assert!(invalid.excerpt.contains("FurnitureStore"));
}

#[test]
fn parse_keeps_jsonld_to_blocks_that_parsed() {
    let parsed = parse(VALID);
    assert_eq!(parsed.jsonld.len(), 1);
    assert!(parsed.jsonld_invalid.is_empty());
}

#[test]
fn parse_reports_an_empty_block() {
    let html = r#"<html><head><script type="application/ld+json"></script></head></html>"#;
    assert_eq!(parse(html).jsonld_invalid[0].error, "block is empty");
}

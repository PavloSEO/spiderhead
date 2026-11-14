//! Ports of the pure parser tests in the reference suite.
//!
//! The three crawl-driven cases (fired check, skipped check, clean page through a native crawl)
//! need the crawl, sf-import and checks crates; they are listed in the slice report instead.

use spiderhead_parse::parser::parse_html;
use spiderhead_parse::types::ParseOptions;

const PAGE: &str = r#"<html><head><title>{title}</title></head><body>
<main><h1>{title}</h1><p>Enough body text to be a real page. {extra}</p>
{links}</main></body></html>"#;

const BROKEN_LINKS: &str = r##"
<a href="https://example.com/real">A real link</a>
<a href="http://[invalid">Truncated IPv6-ish href</a>
<a href="mailto:hi@example.com">A mail contact</a>
<a href="#anchor">In-page anchor</a>
<a name="bookmark">No href at all</a>"##;

const CLEAN_LINKS: &str = r#"<a href="https://example.com/real">A real link</a>"#;

/// reference `_page(title, links)`: the template with the title repeated 40 times as filler text.
fn page(title: &str, links: &str) -> String {
    let extra = vec![title; 40].join(" ");
    PAGE.replace("{title}", title)
        .replace("{extra}", &extra)
        .replace("{links}", links)
}

fn recording() -> ParseOptions {
    ParseOptions {
        invalid_link_targets: true,
        ..ParseOptions::default()
    }
}

#[test]
fn invalid_targets_are_recorded_with_their_reason_when_switched_on() {
    let parsed = parse_html(
        &page("Broken", BROKEN_LINKS),
        "https://example.com/broken",
        &recording(),
    )
    .unwrap();
    let invalid = parsed
        .invalid_link_targets
        .as_ref()
        .expect("invalid targets are recorded when switched on");
    let pairs: Vec<(&str, &str)> = invalid
        .iter()
        .map(|target| (target.raw_href.as_str(), target.reason.as_str()))
        .collect();
    assert_eq!(
        pairs,
        vec![
            ("http://[invalid", "unparsable_syntax"),
            ("mailto:hi@example.com", "unsupported_scheme"),
        ]
    );
    // Valid links are unaffected, and an in-page anchor or a missing href is absence, not a
    // defect -- neither is recorded.
    let hrefs: Vec<&str> = parsed.links.iter().map(|link| link.href.as_str()).collect();
    assert_eq!(hrefs, vec!["https://example.com/real"]);
}

#[test]
fn invalid_targets_are_absent_unless_the_crawl_asks_for_them() {
    let parsed = parse_html(
        &page("Broken", BROKEN_LINKS),
        "https://example.com/broken",
        &ParseOptions::default(),
    )
    .unwrap();
    assert!(parsed.invalid_link_targets.is_none());
}

#[test]
fn a_clean_page_records_an_empty_list_not_an_absence() {
    let parsed = parse_html(
        &page("Clean", CLEAN_LINKS),
        "https://example.com/clean",
        &recording(),
    )
    .unwrap();
    assert_eq!(parsed.invalid_link_targets.as_ref().map(Vec::len), Some(0));
}

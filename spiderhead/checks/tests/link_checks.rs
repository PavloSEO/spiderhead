//! Ported link-check tests (`oracle-baseline`).
//!
//! The reference originals build the context through a fake native crawl; the
//! record objects (`invalid_link_targets`, `link_placement`, outlink counts)
//! are authored directly here — the check reads exactly those record fields.
//! Crawl-side recording tests (the reference suite lines
//! 83-108) and the `link_placement`/`parse_html` parser tests belong to the
//! crawl/parse slices.

use serde_json::{json, Value};
use spiderhead_checks::audit::rules::links;
use spiderhead_checks::audit::{AuditContext, Page};

fn page(url: &str, rec: serde_json::Map<String, Value>) -> Page {
    Page {
        url: url.to_owned(),
        status_code: Some(200),
        content_type: "text/html".into(),
        indexability: "Indexable".into(),
        rec,
        ..Page::default()
    }
}

fn ctx(pages: Vec<Page>, fields: &[&str]) -> AuditContext {
    let mut ctx = AuditContext::new(pages);
    ctx.fields = fields.iter().map(|f| f.to_string()).collect();
    ctx
}

fn fired<'a>(ctx: &'a AuditContext, check: &str) -> Vec<&'a str> {
    ctx.issues
        .iter()
        .filter(|i| i.check == check)
        .filter_map(|i| i.target_url.as_deref())
        .collect()
}

fn details<'a>(
    ctx: &'a AuditContext,
    check: &str,
    url: &str,
) -> &'a serde_json::Map<String, Value> {
    &ctx.issues
        .iter()
        .find(|i| i.check == check && i.target_url.as_deref() == Some(url))
        .unwrap()
        .details
}

fn skip_reason<'a>(ctx: &'a AuditContext, check: &str) -> Option<&'a str> {
    ctx.skipped
        .iter()
        .find(|s| s.id == check)
        .map(|s| s.reason.as_str())
}

#[test]
fn more_external_than_internal_is_not_a_dead_end() {
    let mut c = ctx(
        vec![page(
            "https://example.com/cited",
            [
                ("outlinks".into(), json!(10)),
                ("external_outlinks".into(), json!(15)),
            ]
            .into_iter()
            .collect(),
        )],
        &[],
    );
    links::check_links_extra(&mut c);
    assert!(fired(&c, "NO_INTERNAL_OUTLINKS").is_empty());
}

#[test]
fn a_page_with_no_internal_links_is_still_flagged() {
    let mut c = ctx(
        vec![page(
            "https://example.com/dead-end",
            [
                ("outlinks".into(), json!(0)),
                ("external_outlinks".into(), json!(15)),
            ]
            .into_iter()
            .collect(),
        )],
        &[],
    );
    links::check_links_extra(&mut c);
    assert_eq!(
        fired(&c, "NO_INTERNAL_OUTLINKS"),
        ["https://example.com/dead-end"]
    );
}

#[test]
fn invalid_link_target_fires_for_exactly_the_broken_page() {
    let mut c = ctx(
        vec![
            page(
                "https://example.com/broken",
                [(
                    "invalid_link_targets".into(),
                    json!([
                        {"raw_href": "http://[invalid", "reason": "unparsable_syntax"},
                        {"raw_href": "mailto:hi@example.com", "reason": "unsupported_scheme"},
                    ]),
                )]
                .into_iter()
                .collect(),
            ),
            page(
                "https://example.com/clean",
                [("invalid_link_targets".into(), json!([]))]
                    .into_iter()
                    .collect(),
            ),
        ],
        &["invalid_link_targets"],
    );
    links::check_invalid_link_targets(&mut c);
    assert_eq!(
        fired(&c, "INVALID_LINK_TARGET"),
        ["https://example.com/broken"]
    );
    let d = details(&c, "INVALID_LINK_TARGET", "https://example.com/broken");
    assert_eq!(
        *d,
        serde_json::from_value(json!({
            "count": 2,
            "first_targets": [
                {"raw_href": "http://[invalid", "reason": "unparsable_syntax"},
                {"raw_href": "mailto:hi@example.com", "reason": "unsupported_scheme"},
            ],
        }))
        .unwrap()
    );
}

#[test]
fn invalid_link_target_skips_honestly_when_recording_was_off() {
    let mut c = ctx(
        vec![page(
            "https://example.com/broken",
            [("invalid_link_targets".into(), Value::Null)]
                .into_iter()
                .collect(),
        )],
        &["invalid_link_targets"],
    );
    links::check_invalid_link_targets(&mut c);
    assert!(fired(&c, "INVALID_LINK_TARGET").is_empty());
    assert_eq!(
        skip_reason(&c, "INVALID_LINK_TARGET"),
        Some("invalid link target recording was off for this crawl (discovery.record_invalid_targets)")
    );
}

#[test]
fn invalid_link_target_measured_clean_page_fires_nothing() {
    let mut c = ctx(
        vec![page(
            "https://example.com/clean",
            [("invalid_link_targets".into(), json!([]))]
                .into_iter()
                .collect(),
        )],
        &["invalid_link_targets"],
    );
    links::check_invalid_link_targets(&mut c);
    assert!(fired(&c, "INVALID_LINK_TARGET").is_empty());
    assert!(skip_reason(&c, "INVALID_LINK_TARGET").is_none());
}

#[test]
fn link_inside_heading_fires_on_the_linked_h1_and_not_on_the_plain_one() {
    let placement = |items: Value| {
        json!({
            "in_heading": items,
            "in_heading_total": items.as_array().map_or(0, Vec::len),
            "image_no_text": [],
            "image_no_text_total": 0,
        })
    };
    let mut c = ctx(
        vec![
            page(
                "https://example.com/bad",
                [(
                    "link_placement".into(),
                    placement(json!([{
                        "level": 1,
                        "anchor": "Home",
                        "destination": "https://example.com/",
                    }])),
                )]
                .into_iter()
                .collect(),
            ),
            page(
                "https://example.com/clean",
                [("link_placement".into(), placement(json!([])))]
                    .into_iter()
                    .collect(),
            ),
        ],
        &["link_placement"],
    );
    links::check_link_placement(&mut c);
    assert_eq!(
        fired(&c, "LINK_INSIDE_HEADING"),
        ["https://example.com/bad"]
    );
    let d = details(&c, "LINK_INSIDE_HEADING", "https://example.com/bad");
    assert_eq!(d["count"], json!(1));
    assert_eq!(d["levels"], json!(["h1"]));
    assert_eq!(
        d["first_links"][0]["destination"],
        json!("https://example.com/")
    );
}

#[test]
fn image_link_without_text_fires_on_the_empty_alt_and_not_on_the_described_one() {
    let mut c = ctx(
        vec![
            page(
                "https://example.com/bad",
                [(
                    "link_placement".into(),
                    json!({
                        "in_heading": [],
                        "in_heading_total": 0,
                        "image_no_text": [{"destination": "https://example.com/product"}],
                        "image_no_text_total": 1,
                    }),
                )]
                .into_iter()
                .collect(),
            ),
            page(
                "https://example.com/clean",
                [(
                    "link_placement".into(),
                    json!({
                        "in_heading": [],
                        "in_heading_total": 0,
                        "image_no_text": [],
                        "image_no_text_total": 0,
                    }),
                )]
                .into_iter()
                .collect(),
            ),
        ],
        &["link_placement"],
    );
    links::check_link_placement(&mut c);
    assert_eq!(
        fired(&c, "IMAGE_LINK_WITHOUT_TEXT"),
        ["https://example.com/bad"]
    );
    let d = details(&c, "IMAGE_LINK_WITHOUT_TEXT", "https://example.com/bad");
    assert_eq!(
        d["first_destinations"],
        json!(["https://example.com/product"])
    );
}

#[test]
fn one_finding_per_page_rather_than_one_per_link() {
    let mut c = ctx(
        vec![page(
            "https://example.com/bad",
            [(
                "link_placement".into(),
                json!({
                    "in_heading": [
                        {"level": 1, "anchor": "a", "destination": "https://example.com/1"},
                        {"level": 2, "anchor": "b", "destination": "https://example.com/2"},
                    ],
                    "in_heading_total": 2,
                    "image_no_text": [],
                    "image_no_text_total": 0,
                }),
            )]
            .into_iter()
            .collect(),
        )],
        &["link_placement"],
    );
    links::check_link_placement(&mut c);
    assert_eq!(
        c.issues
            .iter()
            .filter(|i| i.check == "LINK_INSIDE_HEADING")
            .count(),
        1
    );
    assert_eq!(
        details(&c, "LINK_INSIDE_HEADING", "https://example.com/bad")["count"],
        json!(2)
    );
}

#[test]
fn placement_checks_skip_honestly_on_a_plain_sf_export() {
    let mut c = ctx(
        vec![page("https://example.com/ok", Default::default())],
        &[],
    );
    links::check_link_placement(&mut c);
    for check in ["LINK_INSIDE_HEADING", "IMAGE_LINK_WITHOUT_TEXT"] {
        assert!(
            skip_reason(&c, check)
                .unwrap_or("")
                .contains("native crawl only"),
            "{check}"
        );
    }
}

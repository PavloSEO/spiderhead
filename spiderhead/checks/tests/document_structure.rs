//! Ported document-structure tests (`oracle-baseline`).
//!
//! The reference originals either drive `AuditContext` over an export-shaped
//! frame directly (the reference suite), or reach the check
//! through a fake native crawl (the reference suite,
//! the reference suite) — the check reads only the authored record
//! fields, which is what the ports build here. Parser-side tests
//! (`invalid_head_elements`, `mobile_alternate_broken_count`, `parse_html`
//! position facts) and retained-body markup tests belong to the parse and
//! crawl/store slices.

use serde_json::{json, Map, Value};
use spiderhead_checks::audit::rules::technical;
use spiderhead_checks::audit::{AuditContext, Page};

fn page(url: &str, mut rec: Map<String, Value>) -> Page {
    rec.entry("content_type")
        .or_insert_with(|| json!("text/html"));
    Page {
        url: url.to_owned(),
        status_code: Some(200),
        content_type: "text/html".into(),
        indexability: "Indexable".into(),
        rec,
        ..Page::default()
    }
}

/// A well-formed page record, like `_page` in the reference test file.
fn sound(url: &str) -> Page {
    page(
        url,
        Map::from_iter([
            ("doctype".into(), json!("<!DOCTYPE html>")),
            ("meta_charset".into(), json!("utf-8")),
            (
                "viewport".into(),
                json!("width=device-width, initial-scale=1"),
            ),
            ("head_count".into(), json!(1)),
            ("body_count".into(), json!(1)),
            ("head_not_first".into(), json!(false)),
            ("invalid_head_elements".into(), json!("")),
        ]),
    )
}

/// Every field the structure checks consult; `ctx.fields` gates the honest
/// skip branches exactly like `find_column` on the export frame does.
const STRUCTURE_FIELDS: &[&str] = &[
    "doctype",
    "meta_charset",
    "viewport",
    "mobile_alternate_broken",
    "title_outside_head",
    "meta_description_outside_head",
    "canonical_outside_head",
    "directives_outside_head",
    "hreflang_outside_head",
    "head_count",
    "body_count",
    "head_not_first",
    "invalid_head_elements",
    "duplicate_ids",
];

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

fn skipped_ids(ctx: &AuditContext) -> Vec<&str> {
    ctx.skipped.iter().map(|s| s.id.as_str()).collect()
}

fn details<'a>(ctx: &'a AuditContext, check: &str, url: &str) -> &'a Map<String, Value> {
    &ctx.issues
        .iter()
        .find(|i| i.check == check && i.target_url.as_deref() == Some(url))
        .unwrap()
        .details
}

const URL: &str = "https://example.test/page";

#[test]
fn a_well_formed_document_is_clean_on_every_structure_check() {
    let mut ctx = ctx(vec![sound(URL)], STRUCTURE_FIELDS);
    technical::check_doctype(&mut ctx);
    technical::check_document_skeleton(&mut ctx);
    technical::check_charset(&mut ctx);
    technical::check_viewport(&mut ctx);
    assert!(ctx.issues.is_empty());
    for check in [
        "MISSING_DOCTYPE",
        "INVALID_DOCTYPE",
        "HEAD_MISSING",
        "BODY_MISSING",
        "MISSING_CHARSET",
        "INVALID_CHARSET",
        "VIEWPORT_MISSING",
        "INVALID_VIEWPORT",
    ] {
        assert!(!skipped_ids(&ctx).contains(&check), "{check}");
    }
}

#[test]
fn a_missing_element_is_caught_by_its_own_check_alone() {
    let mut thin = sound(URL);
    thin.rec.insert("doctype".into(), json!(""));
    thin.rec.insert("head_count".into(), json!(0));
    thin.rec.insert("body_count".into(), json!(0));
    let mut ctx = ctx(
        vec![thin, sound("https://example.test/ok")],
        STRUCTURE_FIELDS,
    );
    technical::check_doctype(&mut ctx);
    technical::check_document_skeleton(&mut ctx);
    technical::check_charset(&mut ctx);
    technical::check_viewport(&mut ctx);
    assert_eq!(fired(&ctx, "MISSING_DOCTYPE"), [URL]);
    assert_eq!(fired(&ctx, "HEAD_MISSING"), [URL]);
    assert_eq!(fired(&ctx, "BODY_MISSING"), [URL]);
    for check in [
        "INVALID_DOCTYPE",
        "MISSING_CHARSET",
        "INVALID_CHARSET",
        "VIEWPORT_MISSING",
    ] {
        assert!(!fired(&ctx, check).contains(&URL), "{check}");
    }
}

#[test]
fn a_legacy_doctype_is_invalid_not_missing() {
    let mut legacy = sound(URL);
    legacy.rec.insert(
        "doctype".into(),
        json!("<!DOCTYPE HTML PUBLIC \"-//W3C//DTD HTML 4.01//EN\">"),
    );
    let mut ctx = ctx(
        vec![legacy, sound("https://example.test/ok")],
        STRUCTURE_FIELDS,
    );
    technical::check_doctype(&mut ctx);
    assert_eq!(fired(&ctx, "INVALID_DOCTYPE"), [URL]);
    assert!(fired(&ctx, "MISSING_DOCTYPE").is_empty());
}

#[test]
fn an_invalid_charset_is_a_declared_but_legacy_encoding() {
    let mut legacy = sound(URL);
    legacy
        .rec
        .insert("meta_charset".into(), json!("windows-1251"));
    let mut ctx = ctx(
        vec![legacy, sound("https://example.test/ok")],
        STRUCTURE_FIELDS,
    );
    technical::check_charset(&mut ctx);
    assert_eq!(fired(&ctx, "INVALID_CHARSET"), [URL]);
    let d = details(&ctx, "INVALID_CHARSET", URL);
    assert_eq!(d["charset"], json!("windows-1251"));
    assert_eq!(d["declared_by"], json!("meta"));
    assert!(fired(&ctx, "MISSING_CHARSET").is_empty());
}

#[test]
fn a_header_charset_is_judged_when_no_meta_declaration_exists() {
    let mut header = sound(URL);
    header.rec.insert("meta_charset".into(), json!(""));
    header.content_type = "text/html; charset=iso-8859-1".into();
    let mut ctx = ctx(
        vec![header, sound("https://example.test/ok")],
        STRUCTURE_FIELDS,
    );
    technical::check_charset(&mut ctx);
    assert!(fired(&ctx, "MISSING_CHARSET").is_empty());
    assert_eq!(fired(&ctx, "INVALID_CHARSET"), [URL]);
    assert_eq!(
        details(&ctx, "INVALID_CHARSET", URL)["declared_by"],
        json!("header")
    );
}

#[test]
fn an_unusable_viewport_is_invalid_not_missing() {
    let mut zoomed = sound("https://example.test/zoomed");
    zoomed
        .rec
        .insert("viewport".into(), json!("initial-scale=0.5"));
    let mut no_metrics = sound("https://example.test/no-metrics");
    no_metrics
        .rec
        .insert("viewport".into(), json!("user-scalable=no"));
    let mut blank = sound("https://example.test/blank");
    blank.rec.insert("viewport".into(), json!(""));
    let mut ctx = ctx(
        vec![zoomed, no_metrics, blank, sound("https://example.test/ok")],
        STRUCTURE_FIELDS,
    );
    technical::check_viewport(&mut ctx);
    let mut invalid = fired(&ctx, "INVALID_VIEWPORT");
    invalid.sort_unstable();
    assert_eq!(
        invalid,
        [
            "https://example.test/no-metrics",
            "https://example.test/zoomed"
        ]
    );
    assert_eq!(
        fired(&ctx, "VIEWPORT_MISSING"),
        ["https://example.test/blank"]
    );
}

#[test]
fn missing_columns_skip_instead_of_passing() {
    let mut ctx = ctx(vec![sound(URL)], &[]);
    technical::check_doctype(&mut ctx);
    technical::check_document_skeleton(&mut ctx);
    technical::check_charset(&mut ctx);
    technical::check_viewport(&mut ctx);
    let skipped = skipped_ids(&ctx);
    for check in [
        "MISSING_DOCTYPE",
        "INVALID_DOCTYPE",
        "HEAD_MISSING",
        "BODY_MISSING",
        "MISSING_CHARSET",
        "INVALID_CHARSET",
        "VIEWPORT_MISSING",
        "INVALID_VIEWPORT",
    ] {
        assert!(skipped.contains(&check), "{check}");
    }
    assert!(ctx.issues.is_empty());
}

#[test]
fn two_body_tags_are_one_finding_not_one_per_element() {
    let mut page = sound("https://example.com/two-bodies");
    page.rec.insert("body_count".into(), json!(2));
    let mut ctx = ctx(vec![page], STRUCTURE_FIELDS);
    technical::check_document_skeleton(&mut ctx);
    assert_eq!(
        fired(&ctx, "BODY_MULTIPLE"),
        ["https://example.com/two-bodies"]
    );
}

#[test]
fn head_missing_fires_and_body_missing_does_not_double_fire_head_multiple() {
    let mut page = sound("https://example.com/no-head");
    page.rec.insert("head_count".into(), json!(0));
    let mut ctx = ctx(vec![page], STRUCTURE_FIELDS);
    technical::check_document_skeleton(&mut ctx);
    assert_eq!(fired(&ctx, "HEAD_MISSING"), ["https://example.com/no-head"]);
    assert!(fired(&ctx, "HEAD_MULTIPLE").is_empty());
    assert!(fired(&ctx, "BODY_MISSING").is_empty());
}

#[test]
fn position_checks_skip_honestly_on_a_plain_sf_export() {
    let mut ctx = ctx(vec![sound("https://example.com/plain")], &[]);
    technical::check_element_position(&mut ctx);
    technical::check_document_skeleton(&mut ctx);
    let skipped = skipped_ids(&ctx);
    for check in [
        "TITLE_OUTSIDE_HEAD",
        "DESC_OUTSIDE_HEAD",
        "CANONICAL_OUTSIDE_HEAD",
        "DIRECTIVES_OUTSIDE_HEAD",
        "HREFLANG_OUTSIDE_HEAD",
        "HEAD_MISSING",
        "HEAD_MULTIPLE",
        "BODY_MISSING",
        "BODY_MULTIPLE",
        "INVALID_HEAD_ELEMENT",
        "HEAD_NOT_FIRST",
    ] {
        assert!(skipped.contains(&check), "{check}");
    }
    assert!(ctx.issues.is_empty());
}

#[test]
fn duplicate_ids_and_declared_mime_are_observed() {
    let mut dup = sound("https://example.com/duplicate");
    dup.rec
        .insert("duplicate_ids".into(), json!([{"id": "x", "count": 2}]));
    let mislabeled = page("https://example.com/mislabeled.pdf", Map::new());
    let mut ctx = ctx(vec![dup, mislabeled], STRUCTURE_FIELDS);
    technical::check_html_structure(&mut ctx);
    let dups = ctx
        .issues
        .iter()
        .filter(|i| i.check == "DUPLICATE_ID")
        .collect::<Vec<_>>();
    assert_eq!(dups.len(), 1);
    assert_eq!(
        dups[0].target_url.as_deref(),
        Some("https://example.com/duplicate")
    );
    assert_eq!(dups[0].details["id"], json!("x"));
    assert_eq!(dups[0].details["count"], json!(2));
    let mime = ctx
        .issues
        .iter()
        .filter(|i| i.check == "DECLARED_MIME_MISMATCH")
        .collect::<Vec<_>>();
    assert_eq!(mime.len(), 1);
    assert_eq!(
        mime[0].target_url.as_deref(),
        Some("https://example.com/mislabeled.pdf")
    );
    assert_eq!(mime[0].details["declared_content_type"], json!("text/html"));
    assert_eq!(
        mime[0].details["extension_content_type"],
        json!("application/pdf")
    );
}

#[test]
fn clean_ids_and_matching_mime_stay_silent() {
    let clean = page("https://example.com/clean.html", Map::new());
    let mut ctx = ctx(vec![clean], STRUCTURE_FIELDS);
    technical::check_html_structure(&mut ctx);
    assert!(fired(&ctx, "DUPLICATE_ID").is_empty());
    assert!(fired(&ctx, "DECLARED_MIME_MISMATCH").is_empty());
}

#[test]
fn mobile_alternate_fires_only_for_the_hrefless_declaration() {
    let mut broken = sound("https://example.com/broken");
    broken
        .rec
        .insert("mobile_alternate_broken".into(), json!(1));
    let mut valid = sound("https://example.com/valid");
    valid.rec.insert("mobile_alternate_broken".into(), json!(0));
    let mut ctx = ctx(vec![broken, valid], STRUCTURE_FIELDS);
    technical::check_mobile_alternate(&mut ctx);
    assert_eq!(
        fired(&ctx, "MOBILE_ALTERNATE_LINK"),
        ["https://example.com/broken"]
    );
}

#[test]
fn mobile_alternate_skips_honestly_on_a_plain_sf_export() {
    let mut ctx = ctx(vec![sound("https://example.com/plain")], &[]);
    technical::check_mobile_alternate(&mut ctx);
    assert!(skipped_ids(&ctx).contains(&"MOBILE_ALTERNATE_LINK"));
    assert!(fired(&ctx, "MOBILE_ALTERNATE_LINK").is_empty());
}

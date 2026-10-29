//! Ported AMP-pairing tests (`oracle-baseline` the reference suite).
//!
//! The reference originals write an `internal_all.csv` and run `run_rules`; the
//! same rows are built in memory — a `page()` record field per CSV column —
//! and the check runs directly. The two `extract_amphtml` parser tests are
//! covered by the parse crate's own ported suite.

use serde_json::{json, Value};
use spiderhead_checks::audit::rules::technical;
use spiderhead_checks::audit::{AuditContext, Page};

const DESKTOP: &str = "https://example.test/article";
const AMP: &str = "https://example.test/article/amp";

const AMP_CODES: [&str; 5] = [
    "AMP_NON_200",
    "AMP_MISSING_CANONICAL",
    "AMP_MISSING_RETURN_LINK",
    "AMP_NON_INDEXABLE_CANONICAL",
    "AMP_INDEXABLE",
];

/// `_row(url, status, indexable, canonical, amphtml)` from the reference file.
fn row(url: &str, status: i64, indexable: bool, canonical: &str, amphtml: &str) -> Page {
    Page {
        url: url.to_owned(),
        status_code: Some(status),
        content_type: "text/html".into(),
        indexability: if indexable {
            "Indexable".into()
        } else {
            "Non-Indexable".into()
        },
        rec: [
            ("canonical".to_owned(), Value::from(canonical)),
            ("amphtml".to_owned(), Value::from(amphtml)),
        ]
        .into_iter()
        .collect(),
        ..Page::default()
    }
}

fn ctx(rows: Vec<Page>) -> AuditContext {
    let mut ctx = AuditContext::new(rows);
    // The "amphtml Link Element" column is present in every `_run` export.
    ctx.fields.insert("amphtml".into());
    ctx
}

fn found(ctx: &AuditContext) -> std::collections::BTreeSet<&str> {
    ctx.issues
        .iter()
        .map(|i| i.check.as_str())
        .filter(|c| AMP_CODES.contains(c))
        .collect()
}

fn skipped(ctx: &AuditContext) -> Vec<(&str, &str)> {
    ctx.skipped
        .iter()
        .filter(|s| AMP_CODES.contains(&s.id.as_str()))
        .map(|s| (s.id.as_str(), s.reason.as_str()))
        .collect()
}

#[test]
fn healthy_pair_reports_only_the_indexable_notice() {
    let c = ctx(vec![
        row(DESKTOP, 200, true, "", AMP),
        row(AMP, 200, true, DESKTOP, ""),
    ]);
    let mut c = c;
    technical::check_amp_pairing(&mut c);
    let f = found(&c);
    assert!(f.contains("AMP_INDEXABLE"));
    assert!(!f.contains("AMP_NON_200"));
    assert!(!f.contains("AMP_MISSING_RETURN_LINK"));
    assert!(!f.contains("AMP_NON_INDEXABLE_CANONICAL"));
}

#[test]
fn amp_page_not_200_is_reported_and_its_body_is_not_judged() {
    let mut c = ctx(vec![
        row(DESKTOP, 200, true, "", AMP),
        row(AMP, 404, true, "", ""),
    ]);
    technical::check_amp_pairing(&mut c);
    let f = found(&c);
    assert_eq!(f.iter().copied().collect::<Vec<_>>(), ["AMP_NON_200"]);
}

#[test]
fn missing_canonical_on_the_amp_page() {
    let mut c = ctx(vec![
        row(DESKTOP, 200, true, "", AMP),
        row(AMP, 200, false, "", ""),
    ]);
    technical::check_amp_pairing(&mut c);
    assert!(found(&c).contains("AMP_MISSING_CANONICAL"));
}

#[test]
fn canonical_that_names_another_page_is_a_missing_return_link() {
    let mut c = ctx(vec![
        row(DESKTOP, 200, true, "", AMP),
        row(AMP, 200, false, "https://example.test/elsewhere", ""),
    ]);
    technical::check_amp_pairing(&mut c);
    let f = found(&c);
    assert!(f.contains("AMP_MISSING_RETURN_LINK"));
    assert!(!f.contains("AMP_MISSING_CANONICAL"));
}

#[test]
fn canonical_to_a_non_indexable_page() {
    let mut c = ctx(vec![
        row(DESKTOP, 200, true, "", AMP),
        row(AMP, 200, true, "https://example.test/hidden", ""),
        row("https://example.test/hidden", 200, false, "", ""),
    ]);
    technical::check_amp_pairing(&mut c);
    assert!(found(&c).contains("AMP_NON_INDEXABLE_CANONICAL"));
}

#[test]
fn uncaptured_amp_target_is_a_named_skip_not_a_finding() {
    let mut c = ctx(vec![row(DESKTOP, 200, true, "", AMP)]);
    technical::check_amp_pairing(&mut c);
    assert!(found(&c).is_empty());
    let s = skipped(&c);
    assert_eq!(s.len(), 5);
    assert!(s.iter().all(|(_, r)| r.contains("not captured")));
}

#[test]
fn no_amphtml_column_skips_every_code() {
    let mut c = AuditContext::new(vec![row(DESKTOP, 200, true, "", "")]);
    technical::check_amp_pairing(&mut c);
    assert!(found(&c).is_empty());
    assert_eq!(skipped(&c).len(), 5);
}

#[test]
fn one_amp_page_shared_by_two_desktop_pages_is_reported_once() {
    let mut c = ctx(vec![
        row(DESKTOP, 200, true, "", AMP),
        row("https://example.test/article-2", 200, true, "", AMP),
        row(AMP, 500, true, "", ""),
    ]);
    technical::check_amp_pairing(&mut c);
    let non_200: Vec<_> = c
        .issues
        .iter()
        .filter(|i| i.check == "AMP_NON_200")
        .collect();
    assert_eq!(non_200.len(), 1);
    assert_eq!(
        non_200[0].details["desktop_urls"],
        json!([DESKTOP, "https://example.test/article-2"])
    );
}

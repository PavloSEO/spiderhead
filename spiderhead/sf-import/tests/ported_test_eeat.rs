//! Ported trust-signal (EEAT) tests from the reference suite.
//!
//! The reference tests reach these checks two ways -- through SF exports
//! (`run_audit` over `load_exports`) and through a synthetic native crawl
//! (`build_evidence` frames) -- but both end in `run_eeat` over an
//! `AuditContext`, so here the exports are built in memory and the assertions
//! transfer as-is. The summary assertions read `ctx.trust_evidence` directly,
//! which is the same map the audit summary publishes. Deferred with reasons:
//! - the seven parser-fact tests (`parse_html` / `extract_trust_signals`)
//!   belong to the parse crate, outside this crate's test scope;
//! - the storage tests (trust-evidence persistence through import/export)
//!   belong to the store crate;
//! - the aggregate half of
//!   `test_a_partial_crawl_qualifies_every_not_discovered_state` (the
//!   `crawl_scope` "partial crawl" entry) lands with the audit-runner wave;
//!   the `run_eeat` half is asserted here.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};
use spiderhead_formats::models::Issue;
use spiderhead_sf_import::context::AuditContext;
use spiderhead_sf_import::eeat::{check_trust_pages, run_eeat};
use spiderhead_sf_import::loader::LoadedExports;
use spiderhead_sf_import::table::Table;

fn s(v: &str) -> Value {
    Value::String(v.to_owned())
}

fn cols(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| (*s).to_owned()).collect()
}

fn insert(e: &mut LoadedExports, key: &str, names: &[&str], rows: Vec<Vec<Value>>) {
    e.frames
        .insert(key.to_owned(), Table::new(cols(names), rows));
}

/// `AuditContext` over in-memory exports, then `run_eeat` — the checks the
/// reference `run_audit` runs for the trust pass.
fn eeat_audit(exports: LoadedExports, config: Value) -> AuditContext {
    let mut ctx = AuditContext::new(exports, config, None, false).unwrap();
    run_eeat(&mut ctx).unwrap();
    ctx
}

fn fired(ctx: &AuditContext, check: &str) -> Vec<Issue> {
    ctx.issues_deduped_sorted()
        .into_iter()
        .filter(|i| i.check == check)
        .collect()
}

/// `_fired` / `_export_fired`: findings keyed by target URL.
fn fired_map(ctx: &AuditContext, check: &str) -> BTreeMap<String, Issue> {
    fired(ctx, check)
        .into_iter()
        .map(|i| (i.target_url.clone().unwrap_or_default(), i))
        .collect()
}

fn skip_reason(ctx: &AuditContext, check: &str) -> Option<String> {
    ctx.skipped
        .iter()
        .find(|sk| sk.id == check)
        .map(|sk| sk.reason.clone())
}

/// The `trust_pages` map of the run's trust evidence.
fn trust_pages(ctx: &AuditContext) -> &Map<String, Value> {
    ctx.trust_evidence
        .get("trust_pages")
        .and_then(Value::as_object)
        .unwrap()
}

const EXPORT_COLS: &[&str] = &[
    "Address",
    "Content Type",
    "Status Code",
    "Status",
    "Indexability",
    "Title 1",
    "External Outlinks",
];

const INLINK_COLS: &[&str] = &[
    "Type",
    "Source",
    "Destination",
    "Anchor Text",
    "Status Code",
    "Follow",
    "Link Position",
];

/// `_export_row`.
fn export_row(url: &str, indexability: &str, title: &str, outlinks: i64) -> Vec<Value> {
    vec![
        s(url),
        s("text/html"),
        Value::from(200),
        s("OK"),
        s(indexability),
        s(title),
        Value::from(outlinks),
    ]
}

/// One classified inlink row of the bulk `All Inlinks` export.
fn link_row(source: &str, destination: &str, anchor: &str, position: &str) -> Vec<Value> {
    vec![
        s("Hyperlink"),
        s(source),
        s(destination),
        s(anchor),
        Value::from(200),
        s("true"),
        s(position),
    ]
}

// ----------------------------------------------------------- native-crawl shape

const NATIVE_COLS: &[&str] = &[
    "Address",
    "Content Type",
    "Status Code",
    "Status",
    "Indexability",
    "Title 1",
    "Trust Signals",
    "External Outlinks",
];

/// The parser's measured-absence object.
fn empty_signals() -> Value {
    json!({"author": [], "dates": [], "article": []})
}

/// The parser's object for an `<article>` page that declared nothing else.
fn article_signals() -> Value {
    json!({"author": [], "dates": [], "article": ["article_element"]})
}

/// The parser's object for `_DATED_POST`: itemprop author and a `<time>`.
fn dated_signals() -> Value {
    json!({
        "author": [{"signal": "microdata_author", "confidence": "high", "value": "Jane"}],
        "dates": [{"signal": "time_element", "confidence": "medium", "value": "2026-09-01"}],
        "article": ["article_element"],
    })
}

/// One native-crawl page row with the parser's trust-signal object.
fn native_row(
    url: &str,
    status: i64,
    indexability: &str,
    signals: Value,
    outlinks: i64,
) -> Vec<Value> {
    vec![
        s(url),
        s("text/html"),
        Value::from(status),
        s("OK"),
        s(indexability),
        s(""),
        signals,
        Value::from(outlinks),
    ]
}

/// `_site()`: every page the synthetic crawl produced, plus the classified
/// link rows `build_evidence` records for it. `include_terms` mirrors the
/// `exclude_patterns: [/terms]` variant — the home-page anchor to /terms
/// stays in the link evidence either way.
fn native_site(include_terms: bool) -> (Vec<Vec<Value>>, Vec<Vec<Value>>) {
    let mut rows = vec![
        native_row("https://example.com/", 200, "Indexable", empty_signals(), 0),
        native_row(
            "https://example.com/about",
            200,
            "Indexable",
            empty_signals(),
            0,
        ),
        native_row(
            "https://example.com/contact",
            200,
            "Non-Indexable",
            empty_signals(),
            0,
        ),
        native_row(
            "https://example.com/privacy",
            404,
            "Non-Indexable",
            empty_signals(),
            0,
        ),
    ];
    if include_terms {
        rows.push(native_row(
            "https://example.com/terms",
            200,
            "Indexable",
            empty_signals(),
            0,
        ));
    }
    rows.push(native_row(
        "https://example.com/blog/undated",
        200,
        "Indexable",
        article_signals(),
        0,
    ));
    rows.push(native_row(
        "https://example.com/blog/dated",
        200,
        "Indexable",
        dated_signals(),
        1,
    ));
    rows.push(native_row(
        "https://example.com/blog/cited",
        200,
        "Indexable",
        article_signals(),
        1,
    ));
    rows.push(native_row(
        "https://example.com/health/symptom-checker",
        200,
        "Indexable",
        empty_signals(),
        0,
    ));
    let links = vec![
        link_row(
            "https://example.com/",
            "https://example.com/about",
            "About us",
            "Content",
        ),
        link_row(
            "https://example.com/",
            "https://example.com/contact",
            "Contact us",
            "Content",
        ),
        link_row(
            "https://example.com/",
            "https://example.com/privacy",
            "Privacy policy",
            "Content",
        ),
        link_row(
            "https://example.com/",
            "https://example.com/terms",
            "Terms of service",
            "Content",
        ),
        link_row(
            "https://example.com/",
            "https://example.com/blog/undated",
            "u",
            "Content",
        ),
        link_row(
            "https://example.com/",
            "https://example.com/blog/dated",
            "d",
            "Content",
        ),
        link_row(
            "https://example.com/",
            "https://example.com/blog/cited",
            "c",
            "Content",
        ),
        link_row(
            "https://example.com/",
            "https://example.com/health/symptom-checker",
            "h",
            "Content",
        ),
        link_row(
            "https://example.com/blog/dated",
            "https://source.example/x",
            "src",
            "Footer",
        ),
        link_row(
            "https://example.com/blog/cited",
            "https://source.example/y",
            "src",
            "Content",
        ),
    ];
    (rows, links)
}

fn native_audit(rows: Vec<Vec<Value>>, links: Vec<Vec<Value>>) -> AuditContext {
    let mut e = LoadedExports::default();
    insert(&mut e, "internal_all", NATIVE_COLS, rows);
    insert(&mut e, "all_inlinks", INLINK_COLS, links);
    eeat_audit(e, json!({}))
}

// ----------------------------------------------------- the checks over exports

#[test]
fn an_export_without_signal_columns_skips_with_its_reason() {
    let mut e = LoadedExports::default();
    insert(
        &mut e,
        "internal_all",
        EXPORT_COLS,
        vec![export_row(
            "https://example.com/blog/a",
            "Indexable",
            "T",
            0,
        )],
    );
    let ctx = eeat_audit(e, json!({}));
    let skipped: Vec<&str> = ctx.skipped.iter().map(|sk| sk.id.as_str()).collect();
    assert!(skipped.contains(&"NO_AUTHOR_BYLINE"), "{skipped:?}");
    assert!(skipped.contains(&"NO_CONTENT_DATES"), "{skipped:?}");
}

#[test]
fn a_custom_extraction_string_is_a_medium_confidence_author_signal() {
    // The one thing an export can carry: a Custom Extraction the operator
    // named "Trust Signals". A non-empty cell is author evidence of unknown
    // family -- it can never be a date signal, which is exactly what the
    // fixture asserts.
    let mut e = LoadedExports::default();
    insert(
        &mut e,
        "internal_all",
        &[
            "Address",
            "Content Type",
            "Status Code",
            "Status",
            "Indexability",
            "Title 1",
            "External Outlinks",
            "Trust Signals",
        ],
        vec![
            {
                let mut row = export_row("https://example.com/blog/a", "Indexable", "T", 0);
                row.push(s("a named person"));
                row
            },
            {
                let mut row = export_row("https://example.com/blog/b", "Indexable", "T", 0);
                row.push(s("a named person"));
                row
            },
        ],
    );
    let ctx = eeat_audit(e, json!({}));
    let no_author = fired_map(&ctx, "NO_AUTHOR_BYLINE");
    assert!(!no_author.contains_key("https://example.com/blog/a"));
    let dates = fired_map(&ctx, "NO_CONTENT_DATES");
    assert!(dates.contains_key("https://example.com/blog/a"));
    let issue = &dates["https://example.com/blog/a"];
    let authors = issue
        .evidence
        .get("author_signals")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(authors[0].get("signal").unwrap(), &json!("custom_column"));
    assert_eq!(authors[0].get("confidence").unwrap(), &json!("medium"));

    // An empty cell is unmeasured, not a measured absence.
    let mut e = LoadedExports::default();
    insert(
        &mut e,
        "internal_all",
        &[
            "Address",
            "Content Type",
            "Status Code",
            "Status",
            "Indexability",
            "Title 1",
            "External Outlinks",
            "Trust Signals",
        ],
        vec![{
            let mut row = export_row("https://example.com/blog/a", "Indexable", "T", 0);
            row.push(Value::Null);
            row
        }],
    );
    let ctx = eeat_audit(e, json!({}));
    let skipped: Vec<&str> = ctx.skipped.iter().map(|sk| sk.id.as_str()).collect();
    assert!(skipped.contains(&"NO_AUTHOR_BYLINE"), "{skipped:?}");
    assert!(!fired_map(&ctx, "NO_AUTHOR_BYLINE").contains_key("https://example.com/blog/a"));
}

#[test]
fn export_trust_pages_report_discovered_and_index_states() {
    let mut e = LoadedExports::default();
    insert(
        &mut e,
        "internal_all",
        EXPORT_COLS,
        vec![
            export_row("https://example.com/", "Indexable", "T", 0),
            export_row("https://example.com/about", "Indexable", "T", 0),
            export_row("https://example.com/contact", "Indexable", "T", 0),
            export_row("https://example.com/privacy", "Indexable", "T", 0),
            export_row("https://example.com/terms", "Indexable", "T", 0),
        ],
    );
    let ctx = eeat_audit(e, json!({}));
    let fired_checks: Vec<String> = ctx
        .issues_deduped_sorted()
        .iter()
        .map(|i| i.check.clone())
        .collect();
    assert!(!fired_checks.contains(&"MISSING_ABOUT_PAGE".to_owned()));
    assert!(!fired_checks.contains(&"MISSING_CONTACT_PAGE".to_owned()));
    assert!(!fired_checks.contains(&"MISSING_PRIVACY_POLICY".to_owned()));
    assert!(!fired_checks.contains(&"MISSING_TERMS_PAGE".to_owned()));
    assert_eq!(
        trust_pages(&ctx)
            .get("about")
            .unwrap()
            .get("state")
            .unwrap(),
        &json!("found_indexable")
    );

    let mut e = LoadedExports::default();
    insert(
        &mut e,
        "internal_all",
        EXPORT_COLS,
        vec![
            export_row("https://example.com/", "Non-Indexable", "T", 0),
            export_row("https://example.com/privacy", "Non-Indexable", "T", 0),
        ],
    );
    let ctx = eeat_audit(e, json!({}));
    let fired = fired_map(&ctx, "MISSING_PRIVACY_POLICY");
    assert!(fired.contains_key("https://example.com/privacy"));
    assert_eq!(
        fired["https://example.com/privacy"]
            .details
            .get("state")
            .unwrap(),
        &json!("found_non_indexable")
    );
    // Terms was never in the crawled set: not_discovered, a state that does
    // not claim the site lacks the page.
    let terms = &fired_map(&ctx, "MISSING_TERMS_PAGE")
        .values()
        .next()
        .unwrap()
        .clone();
    assert_eq!(
        terms.details.get("state").unwrap(),
        &json!("not_discovered")
    );
    assert_eq!(
        trust_pages(&ctx)
            .get("terms")
            .unwrap()
            .get("state")
            .unwrap(),
        &json!("not_discovered")
    );
}

#[test]
fn export_trust_pages_discovered_only_by_anchor_stay_distinct() {
    let mut e = LoadedExports::default();
    insert(
        &mut e,
        "internal_all",
        EXPORT_COLS,
        vec![export_row("https://example.com/", "Indexable", "T", 0)],
    );
    insert(
        &mut e,
        "all_inlinks",
        INLINK_COLS,
        vec![link_row(
            "https://example.com/",
            "https://example.com/contact-us",
            "Contact us",
            "Content",
        )],
    );
    let ctx = eeat_audit(e, json!({}));
    let issue = &fired_map(&ctx, "MISSING_CONTACT_PAGE")
        .values()
        .next()
        .unwrap()
        .clone();
    assert_eq!(
        issue.details.get("state").unwrap(),
        &json!("discovered_not_crawled")
    );
    assert_eq!(
        issue.details.get("discovered_not_crawled").unwrap(),
        &json!(["https://example.com/contact-us"])
    );
    // The anchor was seen; the page never was — no target URL to point at.
    assert_eq!(issue.target_url, None);
}

#[test]
fn export_citations_read_positions_and_outlink_counts() {
    let cited = "https://example.com/blog/cited";
    let footer_only = "https://example.com/blog/footer";
    let none = "https://example.com/blog/none";
    let mut e = LoadedExports::default();
    insert(
        &mut e,
        "internal_all",
        EXPORT_COLS,
        vec![
            export_row(cited, "Indexable", "T", 1),
            export_row(footer_only, "Indexable", "T", 1),
            export_row(none, "Indexable", "T", 0),
        ],
    );
    insert(
        &mut e,
        "all_inlinks",
        INLINK_COLS,
        vec![
            link_row(cited, "https://source.example/a", "src", "Content"),
            link_row(footer_only, "https://source.example/b", "src", "Footer"),
        ],
    );
    let ctx = eeat_audit(e, json!({}));
    let fired = fired_map(&ctx, "FEW_CITATIONS");
    assert!(!fired.contains_key(cited));
    assert!(fired.contains_key(footer_only));
    assert!(fired.contains_key(none));
}

#[test]
fn export_ymyl_flag_matches_whole_words_and_quotes_them() {
    let mut e = LoadedExports::default();
    insert(
        &mut e,
        "internal_all",
        EXPORT_COLS,
        vec![
            export_row(
                "https://example.com/finance/mortgage-rates",
                "Indexable",
                "T",
                0,
            ),
            export_row(
                "https://example.com/blog/financed-the-van",
                "Indexable",
                "T",
                0,
            ),
        ],
    );
    let ctx = eeat_audit(e, json!({}));
    let fired = fired_map(&ctx, "YMYL_REVIEW_CANDIDATE");
    assert!(fired.contains_key("https://example.com/finance/mortgage-rates"));
    // "financed" is not the word "finance" -- whole-word matching keeps a
    // partial hit out of the review queue.
    assert!(!fired.contains_key("https://example.com/blog/financed-the-van"));
    let issue = &fired["https://example.com/finance/mortgage-rates"];
    let matched = issue.details.get("matched").unwrap();
    let finance: Vec<String> = matched
        .get("finance")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    assert!(finance.contains(&"finance".to_owned()) && finance.contains(&"mortgage".to_owned()));
    assert!(finance.len() == 2);
    assert!(issue
        .details
        .get("rationale")
        .and_then(Value::as_str)
        .unwrap()
        .contains("not a YMYL classification"));
}

// (the run_eeat half; the aggregate `crawl_scope` half lands with the
// audit-runner wave)
#[test]
fn a_partial_crawl_qualifies_every_not_discovered_state() {
    let mut e = LoadedExports::default();
    insert(
        &mut e,
        "internal_all",
        EXPORT_COLS,
        vec![export_row("https://example.com/", "Indexable", "T", 0)],
    );
    let ctx = eeat_audit(e, json!({}));
    assert_eq!(
        trust_pages(&ctx)
            .get("about")
            .unwrap()
            .get("state")
            .unwrap(),
        &json!("not_discovered")
    );
}

#[test]
fn partially_positioned_external_links_cannot_prove_absent_citations() {
    let article = "https://example.com/blog/article";
    let mut e = LoadedExports::default();
    insert(
        &mut e,
        "internal_all",
        EXPORT_COLS,
        vec![export_row(article, "Indexable", "T", 2)],
    );
    insert(
        &mut e,
        "all_inlinks",
        INLINK_COLS,
        vec![
            link_row(article, "https://source.example/a", "src", "Footer"),
            link_row(article, "https://source.example/b", "src", ""),
        ],
    );
    let ctx = eeat_audit(e, json!({}));
    assert!(fired_map(&ctx, "FEW_CITATIONS").is_empty());
    let reason = skip_reason(&ctx, "FEW_CITATIONS").unwrap();
    assert!(reason.contains("unmeasured"), "{reason}");
}

// -------------------------------------------------- the checks over a native crawl

#[test]
fn attribution_fires_only_on_in_scope_pages_that_declared_nothing() {
    let (rows, links) = native_site(true);
    let ctx = native_audit(rows, links);
    let no_author = fired_map(&ctx, "NO_AUTHOR_BYLINE");
    let dates = fired_map(&ctx, "NO_CONTENT_DATES");
    let undated = "https://example.com/blog/undated";
    let dated = "https://example.com/blog/dated";
    assert!(no_author.contains_key(undated));
    assert!(!no_author.contains_key(dated));
    assert!(dates.contains_key(undated));
    assert!(!dates.contains_key(dated));
    // A finding states why the page entered scope and which carriers it read --
    // a reviewer sees "no declared markup", never a trust verdict.
    let issue = &no_author[undated];
    assert_eq!(
        issue.details.get("scope").unwrap(),
        &json!("declared:article_element")
    );
    assert_eq!(issue.details.get("declared_signals").unwrap(), &json!(0));
    // A page with no article marker and no content-path URL never enters scope.
    assert!(!no_author.contains_key("https://example.com/"));
    assert!(!dates.contains_key("https://example.com/about"));
}

#[test]
fn an_unparsed_body_is_unmeasured_not_a_measured_absence() {
    // The article page's body exceeds the fetch cap: a real 200 HTML page
    // whose trust signals were never parsed -- unmeasured, not a measured
    // absence.
    let mut e = LoadedExports::default();
    insert(
        &mut e,
        "internal_all",
        NATIVE_COLS,
        vec![
            native_row("https://example.com/", 200, "Indexable", empty_signals(), 0),
            native_row(
                "https://example.com/blog/undated",
                200,
                "Indexable",
                Value::Null,
                0,
            ),
        ],
    );
    insert(
        &mut e,
        "all_inlinks",
        INLINK_COLS,
        vec![link_row(
            "https://example.com/",
            "https://example.com/blog/undated",
            "u",
            "Content",
        )],
    );
    let ctx = eeat_audit(e, json!({}));
    assert!(!fired_map(&ctx, "NO_AUTHOR_BYLINE").contains_key("https://example.com/blog/undated"));
    let reason = skip_reason(&ctx, "NO_AUTHOR_BYLINE").unwrap();
    assert!(reason.contains("never measured"), "{reason}");
}

#[test]
fn trust_page_states_keep_missing_distinct_from_undiscovered() {
    let (rows, links) = native_site(false);
    let ctx = native_audit(rows, links);
    let trust = trust_pages(&ctx);
    assert_eq!(
        trust.get("about").unwrap().get("state").unwrap(),
        &json!("found_indexable")
    );
    assert!(fired(&ctx, "MISSING_ABOUT_PAGE").is_empty());
    let contact = &fired_map(&ctx, "MISSING_CONTACT_PAGE")["https://example.com/contact"];
    assert_eq!(
        contact.details.get("state").unwrap(),
        &json!("found_non_indexable")
    );
    assert_eq!(
        trust_pages(&ctx)
            .get("contact")
            .unwrap()
            .get("state")
            .unwrap(),
        &json!("found_non_indexable")
    );
    assert!(fired_map(&ctx, "MISSING_PRIVACY_POLICY").contains_key("https://example.com/privacy"));
    assert_eq!(
        trust_pages(&ctx)
            .get("privacy")
            .unwrap()
            .get("state")
            .unwrap(),
        &json!("found_error")
    );
    // Linked but never fetched is its own state -- the crawl saw the anchor,
    // not the page, and the finding says exactly that.
    let terms = &fired_map(&ctx, "MISSING_TERMS_PAGE")
        .values()
        .next()
        .unwrap()
        .clone();
    assert_eq!(
        terms.details.get("state").unwrap(),
        &json!("discovered_not_crawled")
    );
    assert_eq!(
        terms.details.get("discovered_not_crawled").unwrap(),
        &json!(["https://example.com/terms"])
    );
    assert_eq!(
        trust_pages(&ctx)
            .get("terms")
            .unwrap()
            .get("state")
            .unwrap(),
        &json!("discovered_not_crawled")
    );
}

#[test]
fn a_trust_page_nobody_linked_is_not_discovered_not_missing() {
    let mut e = LoadedExports::default();
    insert(
        &mut e,
        "internal_all",
        NATIVE_COLS,
        vec![native_row(
            "https://example.com/",
            200,
            "Indexable",
            empty_signals(),
            0,
        )],
    );
    let ctx = eeat_audit(e, json!({}));
    let issue = &fired_map(&ctx, "MISSING_ABOUT_PAGE")
        .values()
        .next()
        .unwrap()
        .clone();
    assert_eq!(
        issue.details.get("state").unwrap(),
        &json!("not_discovered")
    );
    assert_eq!(issue.target_url, None);
    assert_eq!(
        trust_pages(&ctx)
            .get("about")
            .unwrap()
            .get("state")
            .unwrap(),
        &json!("not_discovered")
    );
}

#[test]
fn citations_require_a_content_position_or_a_countable_absence() {
    let (rows, links) = native_site(true);
    let ctx = native_audit(rows, links);
    let fired = fired_map(&ctx, "FEW_CITATIONS");
    // A footer-only external link is positioned but not content: measured "none".
    assert!(fired.contains_key("https://example.com/blog/dated"));
    // A content-position external link is the only carrier that reads as a citation.
    assert!(!fired.contains_key("https://example.com/blog/cited"));
    // No external links at all is the other measurable fact: zero outlinks.
    assert!(fired.contains_key("https://example.com/blog/undated"));
    let summary = ctx.trust_evidence.get("citations").unwrap();
    assert_eq!(summary.get("applicable_pages").unwrap(), &json!(3));
    assert_eq!(summary.get("with_content_citations").unwrap(), &json!(1));
}

#[test]
fn citations_without_position_evidence_are_unmeasured_not_absent() {
    // No link positions recorded: the external link has no classified
    // position, so citations are unmeasured -- the check names the population
    // rather than claiming no citations exist.
    let linked = "https://example.com/blog/linked";
    let mut e = LoadedExports::default();
    insert(
        &mut e,
        "internal_all",
        NATIVE_COLS,
        vec![
            native_row("https://example.com/", 200, "Indexable", empty_signals(), 0),
            native_row(linked, 200, "Indexable", dated_signals(), 1),
        ],
    );
    insert(
        &mut e,
        "all_inlinks",
        INLINK_COLS,
        vec![
            link_row("https://example.com/", linked, "l", ""),
            link_row(linked, "https://source.example/x", "src", ""),
        ],
    );
    let ctx = eeat_audit(e, json!({}));
    assert!(!fired_map(&ctx, "FEW_CITATIONS").contains_key(linked));
    let reason = skip_reason(&ctx, "FEW_CITATIONS").unwrap();
    assert!(reason.contains("unmeasured"), "{reason}");
    assert_eq!(
        ctx.trust_evidence
            .get("citations")
            .unwrap()
            .get("unmeasured_pages")
            .unwrap(),
        &json!(1)
    );
}

#[test]
fn ymyl_is_a_review_candidate_never_a_classification() {
    let (rows, links) = native_site(true);
    let ctx = native_audit(rows, links);
    let fired = fired_map(&ctx, "YMYL_REVIEW_CANDIDATE");
    let health = "https://example.com/health/symptom-checker";
    assert!(fired.contains_key(health));
    assert!(!fired.contains_key("https://example.com/blog/dated"));
    let issue = &fired[health];
    let matched = issue.details.get("matched").unwrap();
    let hits: Vec<String> = matched
        .get("health")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    assert!(hits.contains(&"health".to_owned()) && hits.contains(&"symptom".to_owned()));
    assert!(hits.len() == 2);
    assert!(issue
        .details
        .get("rationale")
        .and_then(Value::as_str)
        .unwrap()
        .contains("not a YMYL classification"));
    assert_eq!(issue.severity, "notice");
}

#[test]
fn the_summary_block_reports_scope_and_anchor_evidence() {
    let (rows, links) = native_site(true);
    let ctx = native_audit(rows, links);
    let scope = ctx.trust_evidence.get("scope").unwrap();
    assert!(
        scope.get("indexable_html_pages").unwrap().as_i64().unwrap() >= 4,
        "{scope}"
    );
    assert!(!scope
        .get("basis")
        .and_then(Value::as_str)
        .unwrap()
        .is_empty());
    assert_eq!(
        ctx.trust_evidence.get("anchor_evidence").unwrap(),
        &json!("all_inlinks")
    );
}

#[test]
fn a_crawl_with_no_article_scope_says_the_check_did_not_apply() {
    let mut e = LoadedExports::default();
    insert(
        &mut e,
        "internal_all",
        NATIVE_COLS,
        vec![native_row(
            "https://example.com/",
            200,
            "Indexable",
            empty_signals(),
            0,
        )],
    );
    let ctx = eeat_audit(e, json!({}));
    assert!(skip_reason(&ctx, "NO_AUTHOR_BYLINE")
        .unwrap()
        .contains("did not apply"));
    assert!(skip_reason(&ctx, "NO_CONTENT_DATES")
        .unwrap()
        .contains("did not apply"));
    assert!(skip_reason(&ctx, "FEW_CITATIONS")
        .unwrap()
        .contains("did not apply"));
}

// ------------------------------------------------- the trust-page accumulator

/// One bare evidence-link row: anchor and destination only, like the stored
/// link records the accumulator reads.
fn bare_link(destination: &str, anchor: &str) -> Vec<Value> {
    vec![s("Hyperlink"), s(""), s(destination), s(anchor)]
}

fn trust_accumulator(disk_backed: bool) {
    let urls = [
        "https://example.test/contact/a".to_owned(),
        "https://example.test/contact/b".to_owned(),
        "https://example.test/contact/a/".to_owned(),
    ];
    let unknown: Vec<String> = (0..12)
        .map(|i| format!("https://example.test/unfetched/{i:02}"))
        .collect();

    let mut e = LoadedExports::default();
    insert(
        &mut e,
        "internal_all",
        &["Address", "Status Code", "Content Type", "Indexability"],
        vec![
            vec![
                s(&urls[0]),
                Value::from(301),
                s("text/html"),
                s("Non-Indexable"),
            ],
            vec![
                s(&urls[1]),
                Value::from(200),
                s("text/html"),
                s("Indexable"),
            ],
            vec![
                s(&urls[2]),
                Value::from(200),
                s("text/html"),
                s("Indexable"),
            ],
        ],
    );
    // Repeated anchors to one already-crawled URL, many anchors to pages the
    // crawl never fetched (in reverse order, so the five-URL cap has to evict
    // the lexicographically largest), and one link pointing off-site.
    let mut links: Vec<Vec<Value>> = Vec::new();
    for _ in 0..20 {
        links.push(bare_link(&urls[0], "Contact"));
    }
    for url in unknown.iter().rev() {
        links.push(bare_link(url, "About"));
    }
    links.push(bare_link("https://other.test/about", "About"));
    insert(&mut e, "all_inlinks", INLINK_COLS, links);

    let mut ctx = AuditContext::new(e, json!({}), None, disk_backed).unwrap();
    check_trust_pages(&mut ctx).unwrap();

    let trust = trust_pages(&ctx);
    let contact = trust.get("contact").unwrap();
    assert_eq!(contact.get("url").unwrap(), &json!(urls[2]));
    assert_eq!(contact.get("state").unwrap(), &json!("found_indexable"));
    assert_eq!(
        contact.get("evidence").unwrap(),
        &json!(["anchor", "url_path"])
    );
    let about = trust.get("about").unwrap();
    assert_eq!(
        about.get("state").unwrap(),
        &json!("discovered_not_crawled")
    );
    assert_eq!(
        about.get("discovered_not_crawled").unwrap(),
        &json!(unknown[..5])
    );
    let mut checks: Vec<String> = ctx
        .issues_deduped_sorted()
        .iter()
        .map(|i| i.check.clone())
        .collect();
    checks.sort_unstable();
    checks.dedup();
    assert_eq!(
        checks,
        vec![
            "MISSING_ABOUT_PAGE",
            "MISSING_PRIVACY_POLICY",
            "MISSING_TERMS_PAGE"
        ]
    );
}

#[test]
fn trust_accumulator_preserves_grouped_variant_order_and_url_examples() {
    trust_accumulator(false);
    trust_accumulator(true);
}

//! Ported inlink tests from the reference suite,
//! the reference suite, the reference suite,
//! the reference suite, the reference suite,
//! the reference suite and
//! the reference suite.
//!
//! The reference tests reach these checks through `run_audit`; here the exports
//! are built in memory and `run_inlinks` runs the same fixed order, so the
//! assertions transfer as-is. Deferred with reasons:
//! - the `build_tasks` half of `test_redirect_group_uses_crawled_identity...`
//!   (tasks live in the `reports` crate, not yet wired in this crate's tests);
//! - the `MIXED_CONTENT` issue assertion of
//!   `test_insecure_subresource_defers_to_the_native_mixed_content_export`
//!   (emitted by the indexation rules pass, not by the inlink layer) — the
//!   skip itself is asserted;
//! - the pure `compute_link_scores` / `shortest_paths_from_seed` cases, which
//!   `tests/graph_modules_oracle.rs` already covers.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use spiderhead_formats::models::Issue;
use spiderhead_sf_import::context::AuditContext;
use spiderhead_sf_import::inlinks::run_inlinks;
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

/// `AuditContext::new` over in-memory exports, then `run_inlinks` — the same
/// order the reference `run_audit` passes the inlink checks in.
fn audit(exports: LoadedExports, config: Value) -> AuditContext {
    let mut ctx = AuditContext::new(exports, config, None, false).unwrap();
    run_inlinks(&mut ctx).unwrap();
    ctx
}

fn fired(ctx: &AuditContext, check: &str) -> Vec<Issue> {
    ctx.issues_deduped_sorted()
        .into_iter()
        .filter(|i| i.check == check)
        .collect()
}

/// `_fired(res, check)`: findings keyed by target URL.
fn fired_map(ctx: &AuditContext, check: &str) -> BTreeMap<String, Issue> {
    fired(ctx, check)
        .into_iter()
        .map(|i| (i.target_url.clone().unwrap_or_default(), i))
        .collect()
}

/// `_skip_reason(res, check)`.
fn skip_reason(ctx: &AuditContext, check: &str) -> Option<String> {
    ctx.skipped
        .iter()
        .find(|sk| sk.id == check)
        .map(|sk| sk.reason.clone())
}

const INTERNAL_COLS: &[&str] = &[
    "Address",
    "Content Type",
    "Status Code",
    "Status",
    "Indexability",
];

fn page_row(url: &str) -> Vec<Value> {
    vec![
        s(url),
        s("text/html"),
        Value::from(200),
        s("OK"),
        s("Indexable"),
    ]
}

// ---------------------------------------------------------------- the reference suite

#[test]
fn broken_internal_link_localized() {
    let mut e = LoadedExports::default();
    insert(
        &mut e,
        "internal_all",
        INTERNAL_COLS,
        vec![
            page_row("https://example.com/"),
            page_row("https://example.com/page-a"),
            vec![
                s("https://example.com/old-page"),
                s("text/html"),
                Value::from(404),
                s("Not Found"),
                s("Non-Indexable"),
            ],
        ],
    );
    insert(
        &mut e,
        "inlinks_4xx",
        &[
            "Type",
            "Source",
            "Destination",
            "Anchor Text",
            "Alt Text",
            "Status Code",
            "Follow",
            "Target",
            "Rel",
            "Link Position",
            "Link Path",
        ],
        vec![
            vec![
                s("Hyperlink"),
                s("https://example.com/"),
                s("https://example.com/old-page"),
                s("Old page"),
                Value::Null,
                Value::from(404),
                s("true"),
                Value::Null,
                Value::Null,
                s("Content"),
                s("/html/body/main/article/p[3]/a"),
            ],
            vec![
                s("Hyperlink"),
                s("https://example.com/page-a"),
                s("https://example.com/old-page"),
                s("see details"),
                Value::Null,
                Value::from(404),
                s("true"),
                Value::Null,
                Value::Null,
                s("Footer"),
                s("/html/body/footer/nav/a[2]"),
            ],
        ],
    );
    let ctx = audit(e, json!({}));
    let issues = fired(&ctx, "BROKEN_INTERNAL_LINK");
    assert_eq!(issues.len(), 1);
    let issue = &issues[0];
    assert_eq!(
        issue.target_url.as_deref(),
        Some("https://example.com/old-page")
    );
    assert_eq!(issue.status_code, Some(404));
    assert_eq!(issue.occurrences_count, 2);
    assert_eq!(issue.severity, "critical");

    let sources: Vec<_> = issue
        .locations
        .iter()
        .filter_map(|l| l.get("source_url").and_then(Value::as_str))
        .collect();
    assert!(sources.contains(&"https://example.com/"));
    assert!(sources.contains(&"https://example.com/page-a"));
    let positions: Vec<_> = issue
        .locations
        .iter()
        .filter_map(|l| l.get("link_position").and_then(Value::as_str))
        .collect();
    assert!(positions.contains(&"Content") && positions.contains(&"Footer"));
    let paths: Vec<_> = issue
        .locations
        .iter()
        .filter_map(|l| l.get("link_path").and_then(Value::as_str))
        .collect();
    assert!(paths.contains(&"/html/body/main/article/p[3]/a"));
    let home = issue
        .locations
        .iter()
        .find(|l| l.get("source_url").and_then(Value::as_str) == Some("https://example.com/"))
        .unwrap();
    assert_eq!(home.get("anchor").and_then(Value::as_str), Some("Old page"));
    assert_eq!(home.get("follow"), Some(&Value::Bool(true)));
}

#[test]
fn position_breakdown_in_details() {
    let mut e = LoadedExports::default();
    insert(
        &mut e,
        "internal_all",
        INTERNAL_COLS,
        vec![
            page_row("https://example.com/"),
            page_row("https://example.com/page-a"),
        ],
    );
    insert(
        &mut e,
        "inlinks_4xx",
        &[
            "Type",
            "Source",
            "Destination",
            "Anchor Text",
            "Status Code",
            "Follow",
            "Link Position",
            "Link Path",
        ],
        vec![
            vec![
                s("Hyperlink"),
                s("https://example.com/"),
                s("https://example.com/old-page"),
                s("a"),
                Value::from(404),
                s("true"),
                s("Content"),
                s("/a"),
            ],
            vec![
                s("Hyperlink"),
                s("https://example.com/page-a"),
                s("https://example.com/old-page"),
                s("b"),
                Value::from(404),
                s("true"),
                s("Footer"),
                s("/b"),
            ],
        ],
    );
    let ctx = audit(e, json!({}));
    let issue = &fired(&ctx, "BROKEN_INTERNAL_LINK")[0];
    assert_eq!(
        issue.details.get("link_position_breakdown"),
        Some(&json!({"Content": 1, "Footer": 1}))
    );
}

#[test]
fn occurrences_count_is_unique_sources() {
    let mut e = LoadedExports::default();
    insert(
        &mut e,
        "internal_all",
        INTERNAL_COLS,
        vec![
            page_row("https://example.com/"),
            page_row("https://example.com/page-a"),
        ],
    );
    let link = |src: &str, anchor: &str, pos: &str, path: &str| {
        vec![
            s("Hyperlink"),
            s(src),
            s("https://example.com/old-page"),
            s(anchor),
            Value::from(404),
            s("true"),
            s(pos),
            s(path),
        ]
    };
    insert(
        &mut e,
        "inlinks_4xx",
        &[
            "Type",
            "Source",
            "Destination",
            "Anchor Text",
            "Status Code",
            "Follow",
            "Link Position",
            "Link Path",
        ],
        vec![
            link("https://example.com/", "a", "Content", "/a"),
            link("https://example.com/", "b", "Footer", "/b"),
            link("https://example.com/page-a", "c", "Content", "/c"),
        ],
    );
    let ctx = audit(e, json!({}));
    let issue = &fired(&ctx, "BROKEN_INTERNAL_LINK")[0];
    assert_eq!(issue.occurrences_count, 2); // unique sources
    assert_eq!(issue.locations.len(), 3); // but all link instances kept
}

fn grouped_destinations_ctx(destinations: &[&str]) -> AuditContext {
    let mut e = LoadedExports::default();
    insert(
        &mut e,
        "internal_all",
        INTERNAL_COLS,
        vec![
            page_row("https://example.com/a"),
            page_row("https://example.com/b"),
        ],
    );
    let rows: Vec<Vec<Value>> = destinations
        .iter()
        .enumerate()
        .map(|(i, dest)| {
            let src = format!("https://example.com/{}", ['a', 'b'][i]);
            vec![
                s("Hyperlink"),
                s(&src),
                s(dest),
                s("x"),
                Value::from(404),
                s("true"),
                s("Content"),
                s("/x"),
            ]
        })
        .collect();
    insert(
        &mut e,
        "inlinks_4xx",
        &[
            "Type",
            "Source",
            "Destination",
            "Anchor Text",
            "Status Code",
            "Follow",
            "Link Position",
            "Link Path",
        ],
        rows,
    );
    audit(e, json!({}))
}

#[test]
fn broken_link_destinations_grouped_by_norm_url() {
    let ctx =
        grouped_destinations_ctx(&["https://Example.com/broken", "https://example.com/broken"]);
    let issues = fired(&ctx, "BROKEN_INTERNAL_LINK");
    assert_eq!(issues.len(), 1);
    let issue = &issues[0];
    assert_eq!(issue.occurrences_count, 2);
    let sources: Vec<_> = issue
        .locations
        .iter()
        .filter_map(|l| l.get("source_url").and_then(Value::as_str))
        .collect();
    assert!(
        sources.contains(&"https://example.com/a") && sources.contains(&"https://example.com/b")
    );
    // both raw spellings stay visible in evidence
    let mut raws: Vec<String> = issue
        .evidence
        .get("raw_destinations")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    raws.sort();
    assert_eq!(
        raws,
        ["https://Example.com/broken", "https://example.com/broken"]
    );
}

#[test]
fn broken_link_trailing_slash_variant_merges() {
    let ctx =
        grouped_destinations_ctx(&["https://example.com/broken", "https://example.com/broken/"]);
    let issues = fired(&ctx, "BROKEN_INTERNAL_LINK");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].occurrences_count, 2);
}

#[test]
fn broken_link_distinct_destinations_stay_separate() {
    let ctx = grouped_destinations_ctx(&[
        "https://example.com/broken-a",
        "https://example.com/broken-b",
    ]);
    assert_eq!(fired(&ctx, "BROKEN_INTERNAL_LINK").len(), 2);
}

// -------------------------------------------------------- the reference suite

/// The first half of
/// `test_redirect_group_uses_crawled_identity_independent_of_inlink_row_order`;
/// the `build_tasks` half needs the reports crate.
fn redirect_group_by_order(destinations: [&str; 2]) {
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
            "Redirect URL",
        ],
        vec![vec![
            s("https://example.test/old"),
            s("text/html"),
            Value::from(301),
            s("Moved"),
            s("Non-Indexable"),
            s("https://example.test/new"),
        ]],
    );
    let rows: Vec<Vec<Value>> = ["https://example.test/a", "https://example.test/b"]
        .iter()
        .zip(destinations)
        .map(|(src, dest)| {
            vec![
                s(src),
                s(dest),
                s("x"),
                Value::from(301),
                s("true"),
                s("Content"),
                s("/a"),
            ]
        })
        .collect();
    insert(
        &mut e,
        "inlinks_3xx",
        &[
            "Source",
            "Destination",
            "Anchor Text",
            "Status Code",
            "Follow",
            "Link Position",
            "Link Path",
        ],
        rows,
    );
    let ctx = audit(e, json!({}));
    let issue = fired(&ctx, "INTERNAL_LINK_TO_REDIRECT")
        .into_iter()
        .next()
        .expect("INTERNAL_LINK_TO_REDIRECT finding");
    assert_eq!(
        issue.target_url.as_deref(),
        Some("https://example.test/old")
    );
    assert_eq!(
        issue.details.get("final_url"),
        Some(&json!("https://example.test/new"))
    );
    assert_eq!(
        issue.evidence.get("raw_destinations"),
        Some(&json!([destinations[0], destinations[1]]))
    );
}

#[test]
fn redirect_group_crawled_identity_order_caps_first() {
    redirect_group_by_order(["https://Example.test/old", "https://example.test/old"]);
}

#[test]
fn redirect_group_crawled_identity_order_lowercase_first() {
    redirect_group_by_order(["https://example.test/old", "https://Example.test/old"]);
}

fn hrefless_ctx(rows: Vec<Vec<Value>>) -> AuditContext {
    let mut e = LoadedExports::default();
    insert(
        &mut e,
        "internal_all",
        INTERNAL_COLS,
        vec![page_row("https://example.test/en")],
    );
    insert(
        &mut e,
        "all_hreflang",
        &["Source", "Destination", "Hreflang"],
        rows,
    );
    audit(e, json!({}))
}

#[test]
fn hrefless_invalid_hreflang_is_checked_without_target_findings() {
    let ctx = hrefless_ctx(vec![vec![
        s("https://example.test/en"),
        Value::Null,
        s("xx-badcode"),
    ]]);
    assert!(fired(&ctx, "HREFLANG_INVALID_CODE").len() == 1);
    assert!(fired(&ctx, "HREFLANG_BROKEN_TARGET").is_empty());
    assert!(fired(&ctx, "HREFLANG_MISSING_RETURN_LINK").is_empty());
}

#[test]
fn hrefless_duplicate_language_is_checked_without_target_findings() {
    let ctx = hrefless_ctx(vec![
        vec![s("https://example.test/en"), Value::Null, s("en")],
        vec![s("https://example.test/en"), Value::Null, s("EN")],
    ]);
    assert!(fired(&ctx, "HREFLANG_MULTIPLE_ENTRIES").len() == 1);
    assert!(fired(&ctx, "HREFLANG_NOT_CANONICAL").is_empty());
}

// ------------------------------------------------- the reference suite

const GRAPH_INTERNAL_COLS: &[&str] = &[
    "Address",
    "Content Type",
    "Status Code",
    "Status",
    "Indexability",
    "Crawl Depth",
];
const GRAPH_INLINK_COLS: &[&str] = &["Source", "Destination", "Type", "Follow"];

fn graph_exports(internal_rows: Vec<Vec<Value>>, inlink_rows: Vec<Vec<Value>>) -> LoadedExports {
    let mut e = LoadedExports::default();
    insert(&mut e, "internal_all", GRAPH_INTERNAL_COLS, internal_rows);
    insert(&mut e, "all_inlinks", GRAPH_INLINK_COLS, inlink_rows);
    e
}

fn graph_row(url: &str, depth: i64) -> Vec<Value> {
    vec![
        s(url),
        s("text/html"),
        Value::from(200),
        s("OK"),
        s("Indexable"),
        Value::from(depth),
    ]
}

fn edge(src: &str, dest: &str, follow: &str) -> Vec<Value> {
    vec![s(src), s(dest), s("Hyperlink"), s(follow)]
}

#[test]
fn composition_and_path_checks_skip_without_all_inlinks() {
    let mut e = LoadedExports::default();
    insert(
        &mut e,
        "internal_all",
        GRAPH_INTERNAL_COLS,
        vec![graph_row("https://example.com/", 0)],
    );
    let ctx = audit(e, json!({}));
    for check_id in [
        "ONLY_NOFOLLOW_INLINKS",
        "ONLY_NONINDEXABLE_SOURCE_INLINKS",
        "DEEP_DISCOVERY_PATH",
        "INSECURE_SUBRESOURCE",
    ] {
        let reason = skip_reason(&ctx, check_id).unwrap_or_default();
        assert!(reason.contains("all_inlinks"), "{check_id}: {reason}");
    }
}

#[test]
fn a_page_reached_only_by_nofollow_is_flagged() {
    let e = graph_exports(
        vec![
            graph_row("https://example.com/", 0),
            graph_row("https://example.com/a", 1),
            graph_row("https://example.com/b", 1),
            graph_row("https://example.com/target", 2),
        ],
        vec![
            edge(
                "https://example.com/a",
                "https://example.com/target",
                "false",
            ),
            edge(
                "https://example.com/b",
                "https://example.com/target",
                "false",
            ),
        ],
    );
    let ctx = audit(e, json!({}));
    let fired_map = fired_map(&ctx, "ONLY_NOFOLLOW_INLINKS");
    assert_eq!(fired_map.len(), 1);
    assert!(fired_map.contains_key("https://example.com/target"));
}

#[test]
fn one_follow_inlink_clears_only_nofollow() {
    let e = graph_exports(
        vec![
            graph_row("https://example.com/a", 1),
            graph_row("https://example.com/b", 1),
            graph_row("https://example.com/target", 2),
        ],
        vec![
            edge(
                "https://example.com/a",
                "https://example.com/target",
                "false",
            ),
            edge(
                "https://example.com/b",
                "https://example.com/target",
                "true",
            ),
        ],
    );
    let ctx = audit(e, json!({}));
    assert!(fired(&ctx, "ONLY_NOFOLLOW_INLINKS").is_empty());
}

#[test]
fn nofollow_fragment_destination_still_flags_the_page() {
    let e = graph_exports(
        vec![
            graph_row("https://example.test/", 0),
            graph_row("https://example.test/target", 1),
        ],
        vec![edge(
            "https://example.test/",
            "https://example.test/target#details",
            "false",
        )],
    );
    let ctx = audit(e, json!({}));
    let fired_map = fired_map(&ctx, "ONLY_NOFOLLOW_INLINKS");
    assert_eq!(fired_map.len(), 1);
    assert!(fired_map.contains_key("https://example.test/target"));
}

#[test]
fn a_followed_different_fragment_clears_the_finding() {
    let e = graph_exports(
        vec![
            graph_row("https://example.test/", 0),
            graph_row("https://example.test/other", 1),
            graph_row("https://example.test/target", 1),
        ],
        vec![
            edge(
                "https://example.test/",
                "https://example.test/target#details",
                "false",
            ),
            edge(
                "https://example.test/other",
                "https://example.test/target#top",
                "true",
            ),
        ],
    );
    let ctx = audit(e, json!({}));
    assert!(fired(&ctx, "ONLY_NOFOLLOW_INLINKS").is_empty());
}

#[test]
fn a_page_linked_only_from_noindex_sources_is_flagged() {
    let non_indexable = |url: &str, depth: i64| {
        vec![
            s(url),
            s("text/html"),
            Value::from(200),
            s("OK"),
            s("Non-Indexable"),
            Value::from(depth),
        ]
    };
    let e = graph_exports(
        vec![
            non_indexable("https://example.com/a", 1),
            non_indexable("https://example.com/b", 1),
            graph_row("https://example.com/target", 2),
        ],
        vec![
            edge(
                "https://example.com/a",
                "https://example.com/target",
                "true",
            ),
            edge(
                "https://example.com/b",
                "https://example.com/target",
                "true",
            ),
        ],
    );
    let ctx = audit(e, json!({}));
    let fired_map = fired_map(&ctx, "ONLY_NONINDEXABLE_SOURCE_INLINKS");
    assert_eq!(fired_map.len(), 1);
    assert!(fired_map.contains_key("https://example.com/target"));
}

#[test]
fn deep_discovery_path_reports_the_actual_route() {
    let chain: Vec<String> = std::iter::once("https://example.com/".to_owned())
        .chain((0..6).map(|i| format!("https://example.com/l{i}")))
        .collect();
    let mut internal: Vec<Vec<Value>> = vec![graph_row(&chain[0], 0)];
    let mut links: Vec<Vec<Value>> = Vec::new();
    for i in 1..chain.len() {
        internal.push(graph_row(&chain[i], i as i64));
        links.push(edge(&chain[i - 1], &chain[i], "true"));
    }
    let ctx = audit(graph_exports(internal, links), json!({}));
    let fired_map = fired_map(&ctx, "DEEP_DISCOVERY_PATH");
    let deepest = chain.last().unwrap();
    let issue = fired_map.get(deepest).expect("deepest page flagged");
    assert_eq!(issue.details.get("path"), Some(&json!(chain)));
    assert_eq!(issue.details.get("hops"), Some(&json!(6)));
}

#[test]
fn long_deep_discovery_path_is_bounded() {
    let chain: Vec<String> = std::iter::once("https://example.com/".to_owned())
        .chain((0..40).map(|i| format!("https://example.com/l{i}")))
        .collect();
    let internal: Vec<Vec<Value>> = chain
        .iter()
        .enumerate()
        .map(|(i, url)| graph_row(url, i as i64))
        .collect();
    let links: Vec<Vec<Value>> = (1..chain.len())
        .map(|i| edge(&chain[i - 1], &chain[i], "true"))
        .collect();
    let ctx = audit(graph_exports(internal, links), json!({}));
    let details = &fired_map(&ctx, "DEEP_DISCOVERY_PATH")[chain.last().unwrap()].details;
    assert_eq!(details.get("hops"), Some(&json!(40)));
    assert_eq!(details.get("path_truncated"), Some(&json!(true)));
    assert_eq!(details.get("path_start"), Some(&json!([chain[0]])));
    assert_eq!(details.get("path_end"), Some(&json!([chain[40]])));
}

#[test]
fn insecure_subresource_fires_on_an_http_image_from_https() {
    let e = graph_exports(
        vec![graph_row("https://example.com/", 0)],
        vec![
            vec![
                s("https://example.com/"),
                s("http://cdn.example.com/logo.png"),
                s("Image"),
                s(""),
            ],
            edge("https://example.com/", "https://example.com/about", "true"),
        ],
    );
    let ctx = audit(e, json!({}));
    let fired_map = fired_map(&ctx, "INSECURE_SUBRESOURCE");
    assert_eq!(fired_map.len(), 1);
    let issue = fired_map.get("https://example.com/").unwrap();
    assert_eq!(
        issue.details.get("resources"),
        Some(&json!(["http://cdn.example.com/logo.png"]))
    );
}

// (the MIXED_CONTENT issue itself belongs to the indexation rules pass and is
// asserted there; here only the deferral skip is in scope)
#[test]
fn insecure_subresource_defers_to_the_native_mixed_content_export() {
    let mut e = LoadedExports::default();
    insert(
        &mut e,
        "internal_all",
        GRAPH_INTERNAL_COLS,
        vec![graph_row("https://example.com/", 0)],
    );
    insert(
        &mut e,
        "all_inlinks",
        GRAPH_INLINK_COLS,
        vec![vec![
            s("https://example.com/"),
            s("http://cdn.example.com/logo.png"),
            s("Image"),
            s(""),
        ]],
    );
    insert(
        &mut e,
        "security_mixed",
        &["Address"],
        vec![vec![s("https://example.com/")]],
    );
    let ctx = audit(e, json!({}));
    assert!(skip_reason(&ctx, "INSECURE_SUBRESOURCE").is_some());
}

// ----------------------------------------------------------- the reference suite

#[test]
fn link_score_skips_without_the_all_inlinks_export() {
    let mut e = LoadedExports::default();
    insert(
        &mut e,
        "internal_all",
        GRAPH_INTERNAL_COLS,
        vec![graph_row("https://example.com/", 0)],
    );
    let ctx = audit(e, json!({}));
    assert!(skip_reason(&ctx, "LOW_LINK_SCORE")
        .unwrap_or_default()
        .contains("all_inlinks"));
}

fn nofollow_floor_exports() -> LoadedExports {
    // Six "core" pages interlink with follow links; "nofollowed" is linked
    // from every one of them, but always nofollow — so the graph computation
    // scores it at the site's unlinked-page floor.
    let cores: Vec<String> = (0..6)
        .map(|i| format!("https://example.com/core{i}"))
        .collect();
    let mut internal = vec![graph_row("https://example.com/", 0)];
    let mut links: Vec<Vec<Value>> = Vec::new();
    for url in &cores {
        internal.push(graph_row(url, 1));
        links.push(edge("https://example.com/", url, "true"));
    }
    for source in &cores {
        for dest in &cores {
            if source != dest {
                links.push(edge(source, dest, "true"));
            }
        }
        links.push(edge(source, "https://example.com/nofollowed", "false"));
    }
    internal.push(graph_row("https://example.com/nofollowed", 2));
    graph_exports(internal, links)
}

#[test]
fn a_page_reached_only_by_nofollow_links_scores_low() {
    let ctx = audit(nofollow_floor_exports(), json!({}));
    let low: Vec<String> = fired(&ctx, "LOW_LINK_SCORE")
        .iter()
        .filter_map(|i| i.target_url.clone())
        .collect();
    assert!(low.contains(&"https://example.com/nofollowed".to_owned()));
    assert!(!low.contains(&"https://example.com/core0".to_owned()));
}

#[test]
fn a_threshold_change_changes_the_result() {
    let ctx = audit(
        nofollow_floor_exports(),
        json!({"thresholds": {"link_score_low_ratio": 0.01}}),
    );
    let low: Vec<String> = fired(&ctx, "LOW_LINK_SCORE")
        .iter()
        .filter_map(|i| i.target_url.clone())
        .collect();
    assert!(!low.contains(&"https://example.com/nofollowed".to_owned()));
}

// ------------------------------------------- the reference suite

const PAG_INTERNAL_COLS: &[&str] = &[
    "Address",
    "Content Type",
    "Status Code",
    "Status",
    "Indexability",
    "rel=\"next\" 1",
    "Inlinks",
    "Crawl Depth",
];
const PAG_INLINK_COLS: &[&str] = &[
    "Type",
    "Source",
    "Destination",
    "Anchor Text",
    "Status Code",
    "Follow",
];

const PAGE_1: &str = "https://example.com/blog/page/1";
const PAGE_2: &str = "https://example.com/blog/page/2";
const PAGE_3: &str = "https://example.com/blog/page/3";

fn pag_page_row(url: &str, rel_next: &str) -> Vec<Value> {
    vec![
        s(url),
        s("text/html"),
        Value::from(200),
        s("OK"),
        s("Indexable"),
        s(rel_next),
        Value::from(3),
        Value::from(1),
    ]
}

fn pag_link(link_type: &str, src: &str, dest: &str, anchor: &str, follow: &str) -> Vec<Value> {
    vec![
        s(link_type),
        s(src),
        s(dest),
        s(anchor),
        Value::from(200),
        s(follow),
    ]
}

/// `_run(tmp_path, pages, inlinks, inlink_cols)`.
fn pag_run(
    pages: Vec<Vec<Value>>,
    inlinks: Option<(Vec<Vec<Value>>, Vec<String>)>,
) -> AuditContext {
    let mut e = LoadedExports::default();
    insert(&mut e, "internal_all", PAG_INTERNAL_COLS, pages);
    if let Some((rows, inlink_cols)) = inlinks {
        e.frames
            .insert("all_inlinks".into(), Table::new(inlink_cols, rows));
    }
    audit(e, json!({}))
}

fn standard_inlink_cols() -> Vec<String> {
    PAG_INLINK_COLS.iter().map(|c| (*c).to_owned()).collect()
}

#[test]
fn two_different_rel_next_urls_on_one_page_fire() {
    let ctx = pag_run(
        vec![
            pag_page_row(PAGE_1, PAGE_2),
            pag_page_row(PAGE_2, ""),
            pag_page_row(PAGE_3, ""),
        ],
        Some((
            vec![
                pag_link("Rel Next", PAGE_1, PAGE_2, "", "true"),
                pag_link("Rel Next", PAGE_1, PAGE_3, "", "true"),
                pag_link("Hyperlink", PAGE_1, PAGE_2, "Page 2", "true"),
                pag_link("Hyperlink", PAGE_1, PAGE_3, "Page 3", "true"),
            ],
            standard_inlink_cols(),
        )),
    );
    let fired_map = fired_map(&ctx, "PAGINATION_MULTIPLE");
    assert_eq!(fired_map.len(), 1);
    let details = &fired_map[PAGE_1].details;
    assert_eq!(details.get("relation"), Some(&json!("rel=\"next\"")));
    assert_eq!(details.get("urls"), Some(&json!([PAGE_2, PAGE_3])));
}

#[test]
fn one_declaration_per_relation_stays_silent() {
    let ctx = pag_run(
        vec![
            pag_page_row(PAGE_1, PAGE_2),
            pag_page_row(PAGE_2, PAGE_3),
            pag_page_row(PAGE_3, ""),
        ],
        Some((
            vec![
                pag_link("Rel Next", PAGE_1, PAGE_2, "", "true"),
                pag_link("Rel Next", PAGE_2, PAGE_3, "", "true"),
                pag_link("Rel Prev", PAGE_2, PAGE_1, "", "true"),
                pag_link("Hyperlink", PAGE_1, PAGE_2, "Page 2", "true"),
                pag_link("Hyperlink", PAGE_2, PAGE_3, "Page 3", "true"),
                pag_link("Hyperlink", PAGE_2, PAGE_1, "Page 1", "true"),
            ],
            standard_inlink_cols(),
        )),
    );
    assert!(fired(&ctx, "PAGINATION_MULTIPLE").is_empty());
}

#[test]
fn the_same_successor_declared_twice_is_one_successor() {
    let ctx = pag_run(
        vec![pag_page_row(PAGE_1, PAGE_2), pag_page_row(PAGE_2, "")],
        Some((
            vec![
                pag_link("Rel Next", PAGE_1, PAGE_2, "", "true"),
                pag_link("Rel Next", PAGE_1, PAGE_2, "", "true"),
                pag_link("Hyperlink", PAGE_1, PAGE_2, "Page 2", "true"),
            ],
            standard_inlink_cols(),
        )),
    );
    assert!(fired(&ctx, "PAGINATION_MULTIPLE").is_empty());
}

#[test]
fn one_successor_spelled_two_ways_is_one_successor() {
    let ctx = pag_run(
        vec![pag_page_row(PAGE_1, PAGE_2), pag_page_row(PAGE_2, "")],
        Some((
            vec![
                pag_link("Rel Next", PAGE_1, PAGE_2, "", "true"),
                pag_link("Rel Next", PAGE_1, &format!("{PAGE_2}/"), "", "true"),
                pag_link("Hyperlink", PAGE_1, PAGE_2, "Page 2", "true"),
            ],
            standard_inlink_cols(),
        )),
    );
    assert!(fired(&ctx, "PAGINATION_MULTIPLE").is_empty());
    // The same identity on the anchor side, so neither spelling reads as
    // un-anchored either.
    assert!(fired(&ctx, "PAGINATION_URL_NOT_IN_ANCHOR").is_empty());
}

#[test]
fn the_reported_url_is_the_spelling_the_page_used() {
    let ctx = pag_run(
        vec![
            pag_page_row(PAGE_1, PAGE_2),
            pag_page_row(PAGE_2, ""),
            pag_page_row(PAGE_3, ""),
        ],
        Some((
            vec![
                pag_link("Rel Next", PAGE_1, &format!("{PAGE_2}/"), "", "true"),
                pag_link("Rel Next", PAGE_1, PAGE_3, "", "true"),
                pag_link("Hyperlink", PAGE_1, PAGE_2, "Page 2", "true"),
                pag_link("Hyperlink", PAGE_1, PAGE_3, "Page 3", "true"),
            ],
            standard_inlink_cols(),
        )),
    );
    let details = &fired_map(&ctx, "PAGINATION_MULTIPLE")[PAGE_1].details;
    assert_eq!(
        details.get("urls"),
        Some(&json!([format!("{PAGE_2}/"), PAGE_3]))
    );
}

#[test]
fn a_declared_url_with_no_anchor_on_the_same_page_fires() {
    let ctx = pag_run(
        vec![pag_page_row(PAGE_1, PAGE_2), pag_page_row(PAGE_2, "")],
        Some((
            vec![
                pag_link("Rel Next", PAGE_1, PAGE_2, "", "true"),
                pag_link(
                    "Hyperlink",
                    PAGE_1,
                    "https://example.com/about",
                    "About",
                    "true",
                ),
            ],
            standard_inlink_cols(),
        )),
    );
    let fired_map = fired_map(&ctx, "PAGINATION_URL_NOT_IN_ANCHOR");
    assert_eq!(fired_map.len(), 1);
    assert_eq!(
        fired_map[PAGE_1].details.get("declared_without_an_anchor"),
        Some(&json!([{"relation": "rel=\"next\"", "url": PAGE_2}]))
    );
}

#[test]
fn an_anchor_to_the_declared_url_keeps_it_silent() {
    let ctx = pag_run(
        vec![pag_page_row(PAGE_1, PAGE_2), pag_page_row(PAGE_2, "")],
        Some((
            vec![
                pag_link("Rel Next", PAGE_1, PAGE_2, "", "true"),
                pag_link("Hyperlink", PAGE_1, PAGE_2, "Next", "true"),
            ],
            standard_inlink_cols(),
        )),
    );
    assert!(fired(&ctx, "PAGINATION_URL_NOT_IN_ANCHOR").is_empty());
}

#[test]
fn a_nofollow_anchor_is_still_an_anchor() {
    let ctx = pag_run(
        vec![pag_page_row(PAGE_1, PAGE_2), pag_page_row(PAGE_2, "")],
        Some((
            vec![
                pag_link("Rel Next", PAGE_1, PAGE_2, "", "true"),
                pag_link("Hyperlink", PAGE_1, PAGE_2, "Next", "false"),
            ],
            standard_inlink_cols(),
        )),
    );
    assert!(fired(&ctx, "PAGINATION_URL_NOT_IN_ANCHOR").is_empty());
}

#[test]
fn a_trailing_slash_difference_is_not_a_missing_anchor() {
    let ctx = pag_run(
        vec![
            pag_page_row(PAGE_1, &format!("{PAGE_2}/")),
            pag_page_row(PAGE_2, ""),
        ],
        Some((
            vec![
                pag_link("Rel Next", PAGE_1, &format!("{PAGE_2}/"), "", "true"),
                pag_link("Hyperlink", PAGE_1, PAGE_2, "Next", "true"),
            ],
            standard_inlink_cols(),
        )),
    );
    assert!(fired(&ctx, "PAGINATION_URL_NOT_IN_ANCHOR").is_empty());
}

#[test]
fn a_second_rel_next_column_fires_multiple_without_all_inlinks() {
    let mut e = LoadedExports::default();
    let mut names: Vec<String> = PAG_INTERNAL_COLS.iter().map(|c| (*c).to_owned()).collect();
    names.push("rel=\"next\" 2".to_owned());
    let mut row1 = pag_page_row(PAGE_1, PAGE_2);
    row1.push(s(PAGE_3));
    let mut row2 = pag_page_row(PAGE_2, "");
    row2.push(s(""));
    let mut row3 = pag_page_row(PAGE_3, "");
    row3.push(s(""));
    e.frames.insert(
        "internal_all".into(),
        Table::new(names, vec![row1, row2, row3]),
    );
    let ctx = audit(e, json!({}));
    let fired_map = fired_map(&ctx, "PAGINATION_MULTIPLE");
    assert_eq!(fired_map.len(), 1);
    assert_eq!(
        fired_map[PAGE_1].details.get("relation"),
        Some(&json!("rel=\"next\""))
    );
    assert_eq!(
        fired_map[PAGE_1].details.get("urls"),
        Some(&json!([PAGE_2, PAGE_3]))
    );
    // PAGINATION_URL_NOT_IN_ANCHOR still needs the anchors only All Inlinks has.
    assert!(skip_reason(&ctx, "PAGINATION_URL_NOT_IN_ANCHOR")
        .unwrap_or_default()
        .contains("no all_inlinks export"));
    assert!(skip_reason(&ctx, "PAGINATION_MULTIPLE").is_none());
}

#[test]
fn a_second_rel_next_column_the_same_url_stays_silent() {
    let mut e = LoadedExports::default();
    let mut names: Vec<String> = PAG_INTERNAL_COLS.iter().map(|c| (*c).to_owned()).collect();
    names.push("rel=\"next\" 2".to_owned());
    let mut row1 = pag_page_row(PAGE_1, PAGE_2);
    row1.push(s(PAGE_2));
    let mut row2 = pag_page_row(PAGE_2, "");
    row2.push(s(""));
    e.frames
        .insert("internal_all".into(), Table::new(names, vec![row1, row2]));
    let ctx = audit(e, json!({}));
    assert!(fired(&ctx, "PAGINATION_MULTIPLE").is_empty());
}

#[test]
fn without_the_export_both_declare_themselves_absent() {
    let ctx = pag_run(
        vec![pag_page_row(PAGE_1, PAGE_2), pag_page_row(PAGE_2, "")],
        None,
    );
    for check in ["PAGINATION_MULTIPLE", "PAGINATION_URL_NOT_IN_ANCHOR"] {
        assert!(
            skip_reason(&ctx, check)
                .unwrap_or_default()
                .contains("no all_inlinks export"),
            "{check}"
        );
    }
}

#[test]
fn an_export_with_no_type_column_declares_itself_absent() {
    let ctx = pag_run(
        vec![pag_page_row(PAGE_1, PAGE_2), pag_page_row(PAGE_2, "")],
        Some((
            vec![pag_link("Hyperlink", PAGE_1, PAGE_2, "Next", "true")
                .into_iter()
                .skip(1)
                .collect()],
            [
                "Source",
                "Destination",
                "Anchor Text",
                "Status Code",
                "Follow",
            ]
            .iter()
            .map(|c| (*c).to_owned())
            .collect(),
        )),
    );
    for check in ["PAGINATION_MULTIPLE", "PAGINATION_URL_NOT_IN_ANCHOR"] {
        assert!(
            skip_reason(&ctx, check)
                .unwrap_or_default()
                .contains("carries no link type"),
            "{check}"
        );
    }
}

#[test]
fn an_export_with_no_pagination_rows_declares_itself_absent() {
    let ctx = pag_run(
        vec![pag_page_row(PAGE_1, ""), pag_page_row(PAGE_2, "")],
        Some((
            vec![pag_link("Hyperlink", PAGE_1, PAGE_2, "Next", "true")],
            standard_inlink_cols(),
        )),
    );
    for check in ["PAGINATION_MULTIPLE", "PAGINATION_URL_NOT_IN_ANCHOR"] {
        assert!(
            skip_reason(&ctx, check)
                .unwrap_or_default()
                .contains("no rel=\"next\"/rel=\"prev\" rows"),
            "{check}"
        );
    }
}

#[test]
fn an_export_with_no_hyperlink_rows_does_not_report_every_declaration() {
    let ctx = pag_run(
        vec![pag_page_row(PAGE_1, PAGE_2), pag_page_row(PAGE_2, "")],
        Some((
            vec![pag_link("Rel Next", PAGE_1, PAGE_2, "", "true")],
            standard_inlink_cols(),
        )),
    );
    assert!(fired(&ctx, "PAGINATION_URL_NOT_IN_ANCHOR").is_empty());
    assert!(skip_reason(&ctx, "PAGINATION_URL_NOT_IN_ANCHOR")
        .unwrap_or_default()
        .contains("no Hyperlink rows"));
}

// -------------------------------------------- the reference suite

const REC_INTERNAL_COLS: &[&str] = &[
    "Address",
    "Content Type",
    "Status Code",
    "Status",
    "Indexability",
];
const HREFLANG_COLS: &[&str] = &["Source", "Destination", "Hreflang"];

fn rec_audit(internal_urls: &[&str], hreflang_rows: Vec<Vec<Value>>) -> AuditContext {
    let mut e = LoadedExports::default();
    insert(
        &mut e,
        "internal_all",
        REC_INTERNAL_COLS,
        internal_urls.iter().map(|u| page_row(u)).collect(),
    );
    insert(&mut e, "all_hreflang", HREFLANG_COLS, hreflang_rows);
    audit(e, json!({}))
}

fn href_row(src: &str, dest: &str, code: &str) -> Vec<Value> {
    vec![s(src), s(dest), s(code)]
}

#[test]
fn a_one_way_annotation_flags_the_page_that_never_returns_it() {
    let ctx = rec_audit(
        &["https://example.com/en", "https://example.com/fr"],
        vec![href_row(
            "https://example.com/en",
            "https://example.com/fr",
            "fr",
        )],
    );
    let fired_map = fired_map(&ctx, "HREFLANG_MISSING_RETURN_LINK");
    assert_eq!(fired_map.len(), 1);
    assert_eq!(
        fired_map["https://example.com/fr"]
            .details
            .get("expected_return_to"),
        Some(&json!(["https://example.com/en"]))
    );
}

#[test]
fn a_reciprocated_pair_does_not_fire() {
    let ctx = rec_audit(
        &["https://example.com/en", "https://example.com/fr"],
        vec![
            href_row("https://example.com/en", "https://example.com/fr", "fr"),
            href_row("https://example.com/fr", "https://example.com/en", "en"),
        ],
    );
    assert!(fired(&ctx, "HREFLANG_MISSING_RETURN_LINK").is_empty());
}

#[test]
fn a_target_outside_the_crawl_is_not_faulted() {
    let ctx = rec_audit(
        &["https://example.com/en"],
        vec![href_row(
            "https://example.com/en",
            "https://example.com/de",
            "de",
        )],
    );
    assert!(fired(&ctx, "HREFLANG_MISSING_RETURN_LINK").is_empty());
}

#[test]
fn a_one_way_pair_that_differs_only_by_path_case_still_fires() {
    let ctx = rec_audit(
        &["https://example.test/en", "https://example.test/EN"],
        vec![href_row(
            "https://example.test/en",
            "https://example.test/EN",
            "en",
        )],
    );
    let fired_map = fired_map(&ctx, "HREFLANG_MISSING_RETURN_LINK");
    assert_eq!(fired_map.len(), 1);
    assert!(fired_map.contains_key("https://example.test/EN"));
}

#[test]
fn skips_without_the_all_hreflang_export() {
    let mut e = LoadedExports::default();
    insert(
        &mut e,
        "internal_all",
        REC_INTERNAL_COLS,
        vec![page_row("https://example.com/")],
    );
    let ctx = audit(e, json!({}));
    assert!(skip_reason(&ctx, "HREFLANG_MISSING_RETURN_LINK")
        .unwrap_or_default()
        .contains("all_hreflang"));
}

// --------------------------- the reference suite

const EN: &str = "https://example.com/en";
const FR: &str = "https://example.com/fr";
const DE: &str = "https://example.com/de";
const CONF_CHECK: &str = "HREFLANG_INCONSISTENT_CONFIRMATION";

#[test]
fn a_counterpart_confirming_a_different_code_is_reported_against_the_declaring_page() {
    let ctx = rec_audit(
        &[EN, FR],
        vec![
            href_row(EN, EN, "en"),
            href_row(EN, FR, "fr"),
            href_row(FR, FR, "de"),
            href_row(FR, EN, "en"),
        ],
    );
    let fired_map = fired_map(&ctx, CONF_CHECK);
    assert_eq!(fired_map.len(), 1);
    assert_eq!(
        fired_map[EN].details.get("inconsistent"),
        Some(&json!([{"counterpart": FR, "declared_here": "fr", "confirmed_there": ["de"]}]))
    );
}

#[test]
fn a_consistent_set_with_x_default_stays_silent() {
    let ctx = rec_audit(
        &[EN, FR],
        vec![
            href_row(EN, EN, "en"),
            href_row(EN, EN, "x-default"),
            href_row(EN, FR, "fr"),
            href_row(FR, FR, "fr"),
            href_row(FR, EN, "en"),
            href_row(FR, EN, "x-default"),
        ],
    );
    assert!(fired(&ctx, CONF_CHECK).is_empty());
}

#[test]
fn case_differences_alone_are_not_an_inconsistency() {
    let ctx = rec_audit(
        &[EN, FR],
        vec![
            href_row(EN, EN, "en-GB"),
            href_row(EN, FR, "fr-FR"),
            href_row(FR, FR, "fr-fr"),
            href_row(FR, EN, "en-gb"),
        ],
    );
    assert!(fired(&ctx, CONF_CHECK).is_empty());
}

#[test]
fn a_region_variant_is_reported() {
    let ctx = rec_audit(
        &[EN, FR],
        vec![
            href_row(EN, EN, "en-GB"),
            href_row(EN, FR, "fr"),
            href_row(FR, FR, "fr-CA"),
            href_row(FR, EN, "en-GB"),
        ],
    );
    let fired_map = fired_map(&ctx, CONF_CHECK);
    assert_eq!(fired_map.len(), 1);
    assert_eq!(
        fired_map[EN].details["inconsistent"][0].get("confirmed_there"),
        Some(&json!(["fr-ca"]))
    );
}

#[test]
fn a_counterpart_with_no_self_reference_is_not_faulted() {
    let ctx = rec_audit(
        &[EN, FR],
        vec![
            href_row(EN, EN, "en"),
            href_row(EN, FR, "fr"),
            href_row(FR, EN, "en"),
        ],
    );
    assert!(fired(&ctx, CONF_CHECK).is_empty());
    assert!(fired_map(&ctx, "HREFLANG_MISSING_SELF_REFERENCE").contains_key(FR));
}

#[test]
fn a_target_outside_the_crawl_is_left_alone() {
    let ctx = rec_audit(&[EN], vec![href_row(EN, EN, "en"), href_row(EN, DE, "de")]);
    assert!(fired(&ctx, CONF_CHECK).is_empty());
}

#[test]
fn a_self_confirmation_outside_the_crawl_is_left_alone() {
    let ctx = rec_audit(
        &[EN],
        vec![
            href_row(EN, EN, "en"),
            href_row(EN, DE, "de"),
            href_row(DE, DE, "fr"),
        ],
    );
    assert!(fired(&ctx, CONF_CHECK).is_empty());
}

#[test]
fn every_disagreeing_pair_on_one_page_is_reported_once() {
    let ctx = rec_audit(
        &[EN, FR, DE],
        vec![
            href_row(EN, EN, "en"),
            href_row(EN, FR, "fr"),
            href_row(EN, DE, "de"),
            href_row(FR, FR, "fr-CA"),
            href_row(DE, DE, "de-AT"),
            href_row(FR, EN, "en"),
            href_row(DE, EN, "en"),
        ],
    );
    let fired_map = fired_map(&ctx, CONF_CHECK);
    assert_eq!(fired_map.len(), 1);
    assert_eq!(fired_map[EN].occurrences_count, 2);
    let counterparts: Vec<&str> = fired_map[EN].details["inconsistent"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e.get("counterpart").and_then(Value::as_str))
        .collect();
    assert_eq!(counterparts, [FR, DE]);
}

#[test]
fn the_check_skips_by_name_without_the_hreflang_export() {
    let mut e = LoadedExports::default();
    insert(
        &mut e,
        "internal_all",
        REC_INTERNAL_COLS,
        vec![page_row(EN), page_row(FR)],
    );
    let ctx = audit(e, json!({}));
    assert!(skip_reason(&ctx, CONF_CHECK)
        .unwrap_or_default()
        .contains("all_hreflang"));
    assert!(fired(&ctx, CONF_CHECK).is_empty());
}

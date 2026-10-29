//! Ported pagination-check tests (`oracle-baseline`).
//!
//! The reference originals drive `run_audit` over CSV exports; the same rows are
//! built in memory as pages with `rel_next`/`inlinks`/`crawl_depth` record
//! fields, which is exactly the export path `pagination_records` reads, and
//! `all_inlinks` export rows as typed `ctx.exports` records.

use serde_json::{json, Map, Value};
use spiderhead_checks::audit::rules::pagination;
use spiderhead_checks::audit::{AuditContext, Page};

fn page(url: &str) -> Page {
    Page {
        url: url.to_owned(),
        status_code: Some(200),
        content_type: "text/html".into(),
        indexability: "Indexable".into(),
        ..Page::default()
    }
}

/// `_chain` from the reference files: rel="next" edges over `urls` in order,
/// Inlinks=3, Crawl Depth=1.
fn chain(urls: &[&str]) -> Vec<Page> {
    urls.iter()
        .enumerate()
        .map(|(i, url)| {
            let mut p = page(url);
            if i + 1 < urls.len() {
                p.rec.insert("rel_next".into(), Value::from(urls[i + 1]));
            }
            p.rec.insert("inlinks".into(), json!(3));
            p.rec.insert("crawl_depth".into(), json!(1));
            p
        })
        .collect()
}

fn numbered(numbers: &[i64], prefix: &str) -> Vec<String> {
    numbers.iter().map(|n| format!("{prefix}{n}")).collect()
}

fn fired<'a>(ctx: &'a AuditContext, check: &str) -> Vec<&'a str> {
    ctx.issues
        .iter()
        .filter(|i| i.check == check)
        .filter_map(|i| i.target_url.as_deref())
        .collect()
}

fn details<'a>(ctx: &'a AuditContext, check: &str, url: &str) -> &'a Map<String, Value> {
    &ctx.issues
        .iter()
        .find(|i| i.check == check && i.target_url.as_deref() == Some(url))
        .unwrap_or_else(|| panic!("{check} finding on {url} missing"))
        .details
}

fn skip_reason<'a>(ctx: &'a AuditContext, check: &str) -> Option<&'a str> {
    ctx.skipped
        .iter()
        .find(|s| s.id == check)
        .map(|s| s.reason.as_str())
}

#[test]
fn a_21_page_cycle_fires_under_the_default_hop_cap() {
    let urls: Vec<String> = (1..=21)
        .map(|n| format!("https://example.com/page/{n}"))
        .collect();
    let mut pages = chain(&urls.iter().map(String::as_str).collect::<Vec<_>>());
    pages[20]
        .rec
        .insert("rel_next".into(), json!("https://example.com/page/1"));
    let mut ctx = AuditContext::new(pages);
    pagination::check_pagination_series(&mut ctx);
    let loops = fired(&ctx, "PAGINATION_LOOP");
    assert_eq!(loops, vec!["https://example.com/page/1"]);
    assert_eq!(
        details(&ctx, "PAGINATION_LOOP", "https://example.com/page/1")["series"]
            .as_array()
            .map(Vec::len),
        Some(21)
    );
}

#[test]
fn a_short_loop_still_fires_unchanged() {
    // `_chain(PAGE_1, PAGE_2, PAGE_1)`: the last row re-declares PAGE_1 and
    // points at itself, so PAGE_1's own rel="next" forms the loop.
    let mut ctx = AuditContext::new(chain(&[
        "https://example.com/page/1",
        "https://example.com/page/2",
        "https://example.com/page/1",
    ]));
    pagination::check_pagination_series(&mut ctx);
    assert_eq!(
        fired(&ctx, "PAGINATION_LOOP"),
        ["https://example.com/page/1"]
    );
}

#[test]
fn a_terminating_series_longer_than_the_old_cap_stays_silent() {
    let urls: Vec<String> = (1..=25)
        .map(|n| format!("https://example.com/page/{n}"))
        .collect();
    let mut ctx = AuditContext::new(chain(&urls.iter().map(String::as_str).collect::<Vec<_>>()));
    pagination::check_pagination_series(&mut ctx);
    assert!(fired(&ctx, "PAGINATION_LOOP").is_empty());
}

#[test]
fn the_walk_visits_each_page_once_across_the_whole_pass() {
    // The reference test counts dict lookups to pin linearity; the Rust walk is
    // bounded by the same graph structure, so the property is asserted as the
    // result over a chain that quadratic traversal would still finish on.
    let mut map = std::collections::HashMap::new();
    let mut order = Vec::new();
    for i in 0..2_000 {
        let a = format!("https://example.com/p{i}");
        let b = format!("https://example.com/p{}", i + 1);
        map.insert(a.clone(), b);
        order.push(a);
    }
    let (loops, in_loop) = pagination::pagination_loops(&map, &order);
    assert!(loops.is_empty() && in_loop.is_empty());
}

#[test]
fn a_cycle_is_still_found_when_a_long_tail_leads_into_it() {
    let mut map = std::collections::HashMap::new();
    let mut order = Vec::new();
    for i in 0..50 {
        let a = format!("https://example.com/t{i}");
        let b = format!("https://example.com/t{}", i + 1);
        map.insert(a.clone(), b);
        order.push(a);
    }
    map.insert(
        "https://example.com/t50".into(),
        "https://example.com/c0".into(),
    );
    order.push("https://example.com/t50".into());
    for i in 0..30 {
        let a = format!("https://example.com/c{i}");
        let b = format!("https://example.com/c{}", (i + 1) % 30);
        map.insert(a.clone(), b);
        order.push(a);
    }
    let (loops, in_loop) = pagination::pagination_loops(&map, &order);
    assert_eq!(loops.len(), 1);
    let (start, path, loops_to) = &loops[0];
    assert_eq!(start, "https://example.com/t0");
    assert_eq!(loops_to, "https://example.com/c0");
    assert_eq!(path.len(), 81);
    assert!(in_loop.contains("https://example.com/c0"));
}

fn sequence_ctx(urls: &[String]) -> AuditContext {
    AuditContext::new(chain(&urls.iter().map(String::as_str).collect::<Vec<_>>()))
}

/// Several independent `_chain` runs: each series' last page keeps no
/// rel="next", so the chains do not glue into one.
fn multi_series_ctx(series: &[&[String]]) -> AuditContext {
    let mut pages = Vec::new();
    for urls in series {
        pages.extend(chain(&urls.iter().map(String::as_str).collect::<Vec<_>>()));
    }
    AuditContext::new(pages)
}

#[test]
fn a_run_that_skips_pages_fires_and_names_the_break() {
    let urls = numbered(&[1, 2, 3, 7], "https://example.com/blog/page/");
    let mut ctx = sequence_ctx(&urls);
    pagination::check_pagination_sequence(&mut ctx);
    assert_eq!(fired(&ctx, "PAGINATION_SEQUENCE_ERROR"), [urls[0].as_str()]);
    let d = details(&ctx, "PAGINATION_SEQUENCE_ERROR", &urls[0]);
    assert_eq!(d["page_numbers"], json!([1, 2, 3, 7]));
    assert_eq!(
        d["breaks"],
        json!([{"from": urls[2], "to": urls[3], "from_page": 3, "to_page": 7}])
    );
}

#[test]
fn a_query_parameter_series_is_read_the_same_way() {
    let urls: Vec<String> = (1..=3)
        .map(|n| format!("https://example.com/catalog?page={n}"))
        .collect();
    let urls = vec![
        urls[0].clone(),
        urls[1].clone(),
        "https://example.com/catalog?page=5".into(),
    ];
    let mut ctx = sequence_ctx(&urls);
    pagination::check_pagination_sequence(&mut ctx);
    assert_eq!(fired(&ctx, "PAGINATION_SEQUENCE_ERROR"), [urls[0].as_str()]);
}

#[test]
fn an_ordered_run_stays_silent() {
    let urls = numbered(&[1, 2, 3, 4], "https://example.com/blog/page/");
    let mut ctx = sequence_ctx(&urls);
    pagination::check_pagination_sequence(&mut ctx);
    assert!(fired(&ctx, "PAGINATION_SEQUENCE_ERROR").is_empty());
    assert!(skip_reason(&ctx, "PAGINATION_SEQUENCE_ERROR").is_none());
}

#[test]
fn a_series_that_starts_at_four_is_not_an_error() {
    let urls = numbered(&[4, 5, 6], "https://example.com/blog/page/");
    let mut ctx = sequence_ctx(&urls);
    pagination::check_pagination_sequence(&mut ctx);
    assert!(fired(&ctx, "PAGINATION_SEQUENCE_ERROR").is_empty());
}

#[test]
fn a_consistent_stride_stays_silent() {
    let urls = numbered(&[0, 10, 20], "https://example.com/catalog?page=");
    let mut ctx = sequence_ctx(&urls);
    pagination::check_pagination_sequence(&mut ctx);
    assert!(fired(&ctx, "PAGINATION_SEQUENCE_ERROR").is_empty());
    assert!(skip_reason(&ctx, "PAGINATION_SEQUENCE_ERROR").is_none());
}

#[test]
fn a_stride_that_breaks_is_named_at_the_break() {
    let urls = numbered(&[1, 3, 5, 8, 10], "https://example.com/catalog?page=");
    let mut ctx = sequence_ctx(&urls);
    pagination::check_pagination_sequence(&mut ctx);
    let d = details(&ctx, "PAGINATION_SEQUENCE_ERROR", &urls[0]);
    assert_eq!(
        d["breaks"],
        json!([{"from": urls[2], "to": urls[3], "from_page": 5, "to_page": 8}])
    );
}

#[test]
fn an_irregular_offset_scheme_is_left_unevaluated() {
    let urls = numbered(&[0, 10, 25, 30], "https://example.com/catalog?page=");
    let mut ctx = sequence_ctx(&urls);
    pagination::check_pagination_sequence(&mut ctx);
    assert!(fired(&ctx, "PAGINATION_SEQUENCE_ERROR").is_empty());
    let reason = skip_reason(&ctx, "PAGINATION_SEQUENCE_ERROR").unwrap_or("");
    assert!(reason.contains("neither step by one"), "{reason}");
    assert!(!reason.contains("state no page number"), "{reason}");
}

#[test]
fn a_lone_step_is_not_a_stride() {
    let urls = numbered(&[1, 3, 6], "https://example.com/catalog?page=");
    let mut ctx = sequence_ctx(&urls);
    pagination::check_pagination_sequence(&mut ctx);
    assert!(fired(&ctx, "PAGINATION_SEQUENCE_ERROR").is_empty());
    assert!(skip_reason(&ctx, "PAGINATION_SEQUENCE_ERROR")
        .unwrap_or("")
        .contains("neither step by one"));
}

#[test]
fn a_series_whose_urls_state_no_page_number_declares_itself_unjudged() {
    let urls: Vec<String> = (2024..=2026)
        .map(|n| format!("https://example.com/catalog/{n}"))
        .collect();
    let mut ctx = sequence_ctx(&urls);
    pagination::check_pagination_sequence(&mut ctx);
    assert!(fired(&ctx, "PAGINATION_SEQUENCE_ERROR").is_empty());
    assert!(skip_reason(&ctx, "PAGINATION_SEQUENCE_ERROR")
        .unwrap_or("")
        .contains("3 of its 3 URLs state no page number"));
}

#[test]
fn one_unnumbered_url_leaves_the_whole_series_unjudged() {
    let urls = vec![
        "https://example.com/blog/page/1".to_owned(),
        "https://example.com/blog/latest".to_owned(),
        "https://example.com/blog/page/9".to_owned(),
    ];
    let mut ctx = sequence_ctx(&urls);
    pagination::check_pagination_sequence(&mut ctx);
    assert!(fired(&ctx, "PAGINATION_SEQUENCE_ERROR").is_empty());
    assert!(skip_reason(&ctx, "PAGINATION_SEQUENCE_ERROR")
        .unwrap_or("")
        .contains("1 of its 3 URLs states no page number"));
}

#[test]
fn a_two_page_series_is_not_a_run() {
    let urls = numbered(&[1, 6], "https://example.com/blog/page/");
    let mut ctx = sequence_ctx(&urls);
    pagination::check_pagination_sequence(&mut ctx);
    assert!(fired(&ctx, "PAGINATION_SEQUENCE_ERROR").is_empty());
    let reason = skip_reason(&ctx, "PAGINATION_SEQUENCE_ERROR").unwrap_or("");
    assert!(reason.contains("only 2 pages long"), "{reason}");
    assert!(!reason.contains("no page number"), "{reason}");
}

#[test]
fn a_cycling_series_is_left_to_pagination_loop() {
    let urls = numbered(&[1, 2, 7], "https://example.com/blog/page/");
    let mut pages = chain(&urls.iter().map(String::as_str).collect::<Vec<_>>());
    pages[2].rec.insert("rel_next".into(), json!(urls[1]));
    let mut ctx = AuditContext::new(pages);
    pagination::check_pagination_series(&mut ctx);
    pagination::check_pagination_sequence(&mut ctx);
    assert!(fired(&ctx, "PAGINATION_SEQUENCE_ERROR").is_empty());
    assert!(!fired(&ctx, "PAGINATION_LOOP").is_empty());
    let reason = skip_reason(&ctx, "PAGINATION_SEQUENCE_ERROR").unwrap_or("");
    assert!(
        reason.contains("cycles back on itself") && reason.contains("PAGINATION_LOOP"),
        "{reason}"
    );
    assert!(!reason.contains("no page number"), "{reason}");
}

#[test]
fn a_crawl_whose_every_chain_cycles_has_no_series_to_walk() {
    let (a, b) = (
        "https://example.com/blog/page/1",
        "https://example.com/blog/page/2",
    );
    let mut pa = page(a);
    pa.rec.insert("rel_next".into(), json!(b));
    let mut pb = page(b);
    pb.rec.insert("rel_next".into(), json!(a));
    let mut ctx = AuditContext::new(vec![pa, pb]);
    pagination::check_pagination_sequence(&mut ctx);
    assert!(fired(&ctx, "PAGINATION_SEQUENCE_ERROR").is_empty());
    let reason = skip_reason(&ctx, "PAGINATION_SEQUENCE_ERROR").unwrap_or("");
    assert!(
        reason.contains("cycles back on itself") && reason.contains("first page to walk from"),
        "{reason}"
    );
    assert!(!reason.contains("no page number"), "{reason}");
}

#[test]
fn an_unjudged_series_is_named_even_when_another_one_was_judged() {
    let news = numbered(&[1, 2, 3], "https://example.com/news/page/");
    let blog = [
        "https://example.com/blog/".to_owned(),
        "https://example.com/blog/page/2/".to_owned(),
        "https://example.com/blog/page/3/".to_owned(),
    ];
    let mut ctx = multi_series_ctx(&[&news, &blog]);
    pagination::check_pagination_sequence(&mut ctx);
    assert!(fired(&ctx, "PAGINATION_SEQUENCE_ERROR").is_empty());
    let reason = skip_reason(&ctx, "PAGINATION_SEQUENCE_ERROR").unwrap_or("");
    assert!(
        reason.starts_with("1 of 2 rel=\"next\" series could not be judged"),
        "{reason}"
    );
    assert!(reason.contains("https://example.com/blog/"), "{reason}");
}

#[test]
fn each_unjudged_cause_is_counted_and_named_on_its_own() {
    let stride = numbered(&[0, 10, 25, 30], "https://example.com/catalog?page=");
    let short = numbered(&[1, 6], "https://example.com/tags/page/");
    let unnumbered = vec![
        "https://example.com/blog/".to_owned(),
        "https://example.com/blog/page/2/".to_owned(),
        "https://example.com/blog/page/3/".to_owned(),
    ];
    let mut ctx = multi_series_ctx(&[&stride, &short, &unnumbered]);
    pagination::check_pagination_sequence(&mut ctx);
    let reason = skip_reason(&ctx, "PAGINATION_SEQUENCE_ERROR").unwrap_or("");
    assert!(
        reason.starts_with("3 of 3 rel=\"next\" series could not be judged"),
        "{reason}"
    );
    for (clause, example) in [
        ("neither step by one", &stride[0]),
        ("only 2 pages long", &short[0]),
        ("1 of its 3 URLs states no page number", &unnumbered[0]),
    ] {
        assert!(reason.contains(clause), "{reason}");
        assert!(reason.contains(example.as_str()), "{reason}");
    }
}

#[test]
fn a_finding_and_an_unjudged_series_report_as_a_finding() {
    let news = numbered(&[1, 2, 3, 7], "https://example.com/news/page/");
    let blog = [
        "https://example.com/blog/".to_owned(),
        "https://example.com/blog/page/2/".to_owned(),
        "https://example.com/blog/page/3/".to_owned(),
    ];
    let mut ctx = multi_series_ctx(&[&news, &blog]);
    pagination::check_pagination_sequence(&mut ctx);
    assert_eq!(fired(&ctx, "PAGINATION_SEQUENCE_ERROR"), [news[0].as_str()]);
    assert!(skip_reason(&ctx, "PAGINATION_SEQUENCE_ERROR").is_none());
}

#[test]
fn a_crawl_whose_series_are_all_judgeable_says_nothing() {
    let blog = numbered(&[1, 2, 3], "https://example.com/blog/page/");
    let other = numbered(&[1, 2, 3], "https://example.com/n/page/");
    let mut ctx = multi_series_ctx(&[&blog, &other]);
    pagination::check_pagination_sequence(&mut ctx);
    assert!(fired(&ctx, "PAGINATION_SEQUENCE_ERROR").is_empty());
    assert!(skip_reason(&ctx, "PAGINATION_SEQUENCE_ERROR").is_none());
}

#[test]
fn without_the_column_the_check_declares_itself_absent() {
    let mut ctx = AuditContext::new(vec![page("https://example.com/")]);
    pagination::check_pagination_sequence(&mut ctx);
    assert!(skip_reason(&ctx, "PAGINATION_SEQUENCE_ERROR")
        .unwrap_or("")
        .contains("no rel=\"next\" column in Internal:All"));
}

#[test]
fn page_number_is_read_only_from_a_token_that_means_a_page() {
    use pagination::pagination_page_number as num;
    assert_eq!(num("https://example.com/blog/page/2"), Some(2));
    assert_eq!(num("https://example.com/blog/page/2/"), Some(2));
    assert_eq!(num("https://example.com/c?paged=11"), Some(11));
    assert_eq!(num("https://example.com/c?PAGE=3"), Some(3));
    assert_eq!(num("https://example.com/2024/03/a-post"), None);
    assert_eq!(num("https://example.com/product/1188"), None);
    assert_eq!(num("https://example.com/?p=417"), None);
    assert_eq!(num("https://example.com/blog/page/2?page=5"), None);
}

fn target_status_ctx(
    source_next: &str,
    target_status: i64,
    target_indexability: &str,
) -> AuditContext {
    let mut source = page("https://example.com/page/1");
    source.rec.insert("rel_next".into(), json!(source_next));
    source.rec.insert("inlinks".into(), json!(2));
    source.rec.insert("crawl_depth".into(), json!(0));
    let mut target = page(source_next);
    target.status_code = Some(target_status);
    target.indexability = target_indexability.to_owned();
    target.rec.insert("inlinks".into(), json!(1));
    target.rec.insert("crawl_depth".into(), json!(1));
    AuditContext::new(vec![source, target])
}

#[test]
fn a_crawled_404_pagination_target_fires_even_though_the_source_is_indexable() {
    let mut ctx = target_status_ctx("https://example.com/page/2", 404, "Non-Indexable");
    pagination::check_pagination(&mut ctx);
    let fired = fired(&ctx, "PAGINATION_NONINDEXABLE");
    assert_eq!(fired, ["https://example.com/page/2"]);
    let d = details(
        &ctx,
        "PAGINATION_NONINDEXABLE",
        "https://example.com/page/2",
    );
    assert_eq!(d["status_code"], json!(404));
    assert_eq!(d["relation"], json!("rel=\"next\""));
    assert_eq!(d["source_url"], json!("https://example.com/page/1"));
}

#[test]
fn a_crawled_301_pagination_target_also_fires() {
    let mut ctx = target_status_ctx("https://example.com/page/2", 301, "Non-Indexable");
    pagination::check_pagination(&mut ctx);
    assert_eq!(
        fired(&ctx, "PAGINATION_NONINDEXABLE"),
        ["https://example.com/page/2"]
    );
}

#[test]
fn an_uncrawled_pagination_target_is_not_asserted_to_be_non_200() {
    let mut source = page("https://example.com/page/1");
    source
        .rec
        .insert("rel_next".into(), json!("https://example.com/page/2"));
    let mut ctx = AuditContext::new(vec![source]);
    pagination::check_pagination(&mut ctx);
    assert!(fired(&ctx, "PAGINATION_NONINDEXABLE").is_empty());
}

#[test]
fn a_healthy_indexable_pagination_target_stays_silent() {
    let mut ctx = target_status_ctx("https://example.com/page/2", 200, "Indexable");
    pagination::check_pagination(&mut ctx);
    assert!(fired(&ctx, "PAGINATION_NONINDEXABLE").is_empty());
}

#[test]
fn the_original_self_indexability_check_still_fires() {
    let mut source = page("https://example.com/page/2");
    source.indexability = "Non-Indexable".into();
    source
        .rec
        .insert("rel_next".into(), json!("https://example.com/page/3"));
    let mut ctx = AuditContext::new(vec![source]);
    pagination::check_pagination(&mut ctx);
    assert_eq!(
        fired(&ctx, "PAGINATION_NONINDEXABLE"),
        ["https://example.com/page/2"]
    );
}

#[test]
fn a_second_rel_next_column_fires_multiple_without_all_inlinks() {
    let mut p1 = page("https://example.com/blog/page/1");
    p1.rec
        .insert("rel_next".into(), json!("https://example.com/blog/page/2"));
    p1.rec.insert(
        "rel_next_2".into(),
        json!("https://example.com/blog/page/3"),
    );
    let mut ctx = AuditContext::new(vec![
        p1,
        page("https://example.com/blog/page/2"),
        page("https://example.com/blog/page/3"),
    ]);
    ctx.fields.insert("rel_next_2".into());
    pagination::check_pagination_declarations(&mut ctx);
    let d = details(
        &ctx,
        "PAGINATION_MULTIPLE",
        "https://example.com/blog/page/1",
    );
    assert_eq!(
        *d,
        serde_json::from_value::<Map<String, Value>>(json!({
            "relation": "rel=\"next\"",
            "urls": ["https://example.com/blog/page/2", "https://example.com/blog/page/3"],
        }))
        .unwrap()
    );
    assert!(skip_reason(&ctx, "PAGINATION_URL_NOT_IN_ANCHOR")
        .unwrap_or("")
        .contains("no all_inlinks export"));
    assert!(skip_reason(&ctx, "PAGINATION_MULTIPLE").is_none());
}

#[test]
fn a_second_rel_next_column_the_same_url_stays_silent() {
    let mut p1 = page("https://example.com/blog/page/1");
    p1.rec
        .insert("rel_next".into(), json!("https://example.com/blog/page/2"));
    p1.rec.insert(
        "rel_next_2".into(),
        json!("https://example.com/blog/page/2"),
    );
    let mut ctx = AuditContext::new(vec![p1, page("https://example.com/blog/page/2")]);
    ctx.fields.insert("rel_next_2".into());
    pagination::check_pagination_declarations(&mut ctx);
    assert!(fired(&ctx, "PAGINATION_MULTIPLE").is_empty());
}

#[test]
fn without_the_export_both_declare_themselves_absent() {
    let mut p1 = page("https://example.com/blog/page/1");
    p1.rec
        .insert("rel_next".into(), json!("https://example.com/blog/page/2"));
    let mut ctx = AuditContext::new(vec![p1, page("https://example.com/blog/page/2")]);
    pagination::check_pagination_declarations(&mut ctx);
    for check in ["PAGINATION_MULTIPLE", "PAGINATION_URL_NOT_IN_ANCHOR"] {
        assert!(
            skip_reason(&ctx, check)
                .unwrap_or("")
                .contains("no all_inlinks export"),
            "{check}"
        );
    }
}

#[test]
fn an_export_with_no_type_column_declares_itself_absent() {
    // An all_inlinks frame whose column list lacks Type: the projected records
    // exist yet carry no link type to tell a declaration from an anchor.
    let mut p1 = page("https://example.com/blog/page/1");
    p1.rec
        .insert("rel_next".into(), json!("https://example.com/blog/page/2"));
    let mut ctx = AuditContext::new(vec![p1, page("https://example.com/blog/page/2")]);
    ctx.exports.insert(
        "all_inlinks".into(),
        vec![inlink(
            "",
            "https://example.com/blog/page/1",
            "https://example.com/blog/page/2",
        )],
    );
    pagination::check_pagination_declarations(&mut ctx);
    for check in ["PAGINATION_MULTIPLE", "PAGINATION_URL_NOT_IN_ANCHOR"] {
        assert!(
            skip_reason(&ctx, check)
                .unwrap_or("")
                .contains("carries no link type"),
            "{check}"
        );
    }
}

/// One typed `all_inlinks` record — `records_from_df` output with the fields
/// the declarations check reads (`type`, `source_url`, `destination_url`).
fn inlink(link_type: &str, source: &str, destination: &str) -> Map<String, Value> {
    let mut rec = Map::new();
    if link_type.is_empty() {
        rec.insert("type".into(), Value::Null);
    } else {
        rec.insert("type".into(), Value::from(link_type));
    }
    rec.insert("source_url".into(), Value::from(source));
    rec.insert("destination_url".into(), Value::from(destination));
    rec
}

/// The `all_inlinks` frame as `ctx.exports` records.
fn inlinks_ctx(pages: Vec<Page>, records: Vec<Map<String, Value>>) -> AuditContext {
    let mut ctx = AuditContext::new(pages);
    ctx.exports.insert("all_inlinks".into(), records);
    ctx
}

const PAGE_1: &str = "https://example.com/blog/page/1";
const PAGE_2: &str = "https://example.com/blog/page/2";
const PAGE_3: &str = "https://example.com/blog/page/3";

#[test]
fn two_different_rel_next_urls_on_one_page_fire() {
    let mut ctx = inlinks_ctx(
        vec![page(PAGE_1), page(PAGE_2), page(PAGE_3)],
        vec![
            inlink("Rel Next", PAGE_1, PAGE_2),
            inlink("Rel Next", PAGE_1, PAGE_3),
            inlink("Hyperlink", PAGE_1, PAGE_2),
            inlink("Hyperlink", PAGE_1, PAGE_3),
        ],
    );
    pagination::check_pagination_declarations(&mut ctx);
    let d = details(&ctx, "PAGINATION_MULTIPLE", PAGE_1);
    assert_eq!(
        *d,
        serde_json::from_value::<Map<String, Value>>(json!({
            "relation": "rel=\"next\"",
            "urls": [PAGE_2, PAGE_3],
        }))
        .unwrap()
    );
}

#[test]
fn one_declaration_per_relation_stays_silent() {
    let mut ctx = inlinks_ctx(
        vec![page(PAGE_1), page(PAGE_2), page(PAGE_3)],
        vec![
            inlink("Rel Next", PAGE_1, PAGE_2),
            inlink("Rel Next", PAGE_2, PAGE_3),
            inlink("Rel Prev", PAGE_2, PAGE_1),
            inlink("Hyperlink", PAGE_1, PAGE_2),
            inlink("Hyperlink", PAGE_2, PAGE_3),
            inlink("Hyperlink", PAGE_2, PAGE_1),
        ],
    );
    pagination::check_pagination_declarations(&mut ctx);
    assert!(fired(&ctx, "PAGINATION_MULTIPLE").is_empty());
}

#[test]
fn the_same_successor_declared_twice_is_one_successor() {
    let mut ctx = inlinks_ctx(
        vec![page(PAGE_1), page(PAGE_2)],
        vec![
            inlink("Rel Next", PAGE_1, PAGE_2),
            inlink("Rel Next", PAGE_1, PAGE_2),
            inlink("Hyperlink", PAGE_1, PAGE_2),
        ],
    );
    pagination::check_pagination_declarations(&mut ctx);
    assert!(fired(&ctx, "PAGINATION_MULTIPLE").is_empty());
}

#[test]
fn one_successor_spelled_two_ways_is_one_successor() {
    let page_2_slash = format!("{PAGE_2}/");
    let mut ctx = inlinks_ctx(
        vec![page(PAGE_1), page(PAGE_2)],
        vec![
            inlink("Rel Next", PAGE_1, PAGE_2),
            inlink("Rel Next", PAGE_1, &page_2_slash),
            inlink("Hyperlink", PAGE_1, PAGE_2),
        ],
    );
    pagination::check_pagination_declarations(&mut ctx);
    // The two halves agree: neither counts the spellings as two successors,
    // and neither calls the declared URL un-anchored.
    assert!(fired(&ctx, "PAGINATION_MULTIPLE").is_empty());
    assert!(fired(&ctx, "PAGINATION_URL_NOT_IN_ANCHOR").is_empty());
}

#[test]
fn the_reported_url_is_the_spelling_the_page_used() {
    let page_2_slash = format!("{PAGE_2}/");
    let mut ctx = inlinks_ctx(
        vec![page(PAGE_1), page(PAGE_2), page(PAGE_3)],
        vec![
            inlink("Rel Next", PAGE_1, &page_2_slash),
            inlink("Rel Next", PAGE_1, PAGE_3),
            inlink("Hyperlink", PAGE_1, PAGE_2),
            inlink("Hyperlink", PAGE_1, PAGE_3),
        ],
    );
    pagination::check_pagination_declarations(&mut ctx);
    let d = details(&ctx, "PAGINATION_MULTIPLE", PAGE_1);
    assert_eq!(
        d.get("urls"),
        Some(&json!([page_2_slash, PAGE_3])),
        "normalizing decides identity, not the spelling quoted back"
    );
}

#[test]
fn a_declared_url_with_no_anchor_on_the_same_page_fires() {
    let mut ctx = inlinks_ctx(
        vec![page(PAGE_1), page(PAGE_2)],
        vec![
            inlink("Rel Next", PAGE_1, PAGE_2),
            inlink("Hyperlink", PAGE_1, "https://example.com/about"),
        ],
    );
    pagination::check_pagination_declarations(&mut ctx);
    let d = details(&ctx, "PAGINATION_URL_NOT_IN_ANCHOR", PAGE_1);
    assert_eq!(
        d.get("declared_without_an_anchor"),
        Some(&json!([{"relation": "rel=\"next\"", "url": PAGE_2}]))
    );
}

#[test]
fn an_anchor_to_the_declared_url_keeps_it_silent() {
    let mut ctx = inlinks_ctx(
        vec![page(PAGE_1), page(PAGE_2)],
        vec![
            inlink("Rel Next", PAGE_1, PAGE_2),
            inlink("Hyperlink", PAGE_1, PAGE_2),
        ],
    );
    pagination::check_pagination_declarations(&mut ctx);
    assert!(fired(&ctx, "PAGINATION_URL_NOT_IN_ANCHOR").is_empty());
}

#[test]
fn a_nofollow_anchor_is_still_an_anchor() {
    // The question is whether an <a href> to the paginated URL exists on the
    // page, not whether it passes equity.
    let mut ctx = inlinks_ctx(
        vec![page(PAGE_1), page(PAGE_2)],
        vec![
            inlink("Rel Next", PAGE_1, PAGE_2),
            inlink("Hyperlink", PAGE_1, PAGE_2),
        ],
    );
    pagination::check_pagination_declarations(&mut ctx);
    assert!(fired(&ctx, "PAGINATION_URL_NOT_IN_ANCHOR").is_empty());
}

#[test]
fn a_trailing_slash_difference_is_not_a_missing_anchor() {
    let page_2_slash = format!("{PAGE_2}/");
    let mut ctx = inlinks_ctx(
        vec![page(PAGE_1), page(PAGE_2)],
        vec![
            inlink("Rel Next", PAGE_1, &page_2_slash),
            inlink("Hyperlink", PAGE_1, PAGE_2),
        ],
    );
    pagination::check_pagination_declarations(&mut ctx);
    assert!(fired(&ctx, "PAGINATION_URL_NOT_IN_ANCHOR").is_empty());
}

#[test]
fn an_export_with_no_pagination_rows_declares_itself_absent() {
    let mut ctx = inlinks_ctx(
        vec![page(PAGE_1), page(PAGE_2)],
        vec![inlink("Hyperlink", PAGE_1, PAGE_2)],
    );
    pagination::check_pagination_declarations(&mut ctx);
    for check in ["PAGINATION_MULTIPLE", "PAGINATION_URL_NOT_IN_ANCHOR"] {
        assert!(
            skip_reason(&ctx, check)
                .unwrap_or("")
                .contains("no rel=\"next\"/rel=\"prev\" rows"),
            "{check}"
        );
    }
}

#[test]
fn an_export_with_no_hyperlink_rows_does_not_report_every_declaration() {
    // Declarations and not one anchor row anywhere is a filtered export, not
    // a site without links; reporting off it would be wrong findings.
    let mut ctx = inlinks_ctx(
        vec![page(PAGE_1), page(PAGE_2)],
        vec![inlink("Rel Next", PAGE_1, PAGE_2)],
    );
    pagination::check_pagination_declarations(&mut ctx);
    assert!(fired(&ctx, "PAGINATION_URL_NOT_IN_ANCHOR").is_empty());
    assert!(skip_reason(&ctx, "PAGINATION_URL_NOT_IN_ANCHOR")
        .unwrap_or("")
        .contains("no Hyperlink rows"));
}

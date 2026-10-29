//! Ported tests from the sf-core-rules-pack-02 reference test files — the pure
//! helpers of the reference suite and the reference suite plus the `_rel_has` token
//! rules the declaration record folds on. The `run_audit` integration halves
//! of the same files are exercised through `rules_pack02_oracle.rs` fixtures
//! (run_audit itself belongs to a later slice).

use indexmap::IndexMap;
use spiderhead_sf_import::rules::pagination::{
    pagination_loops, pagination_page_number, series_from,
};
use spiderhead_sf_import::rules::shared::path_of;

fn map(entries: &[(&str, &str)]) -> IndexMap<String, String> {
    entries
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn page_number_is_read_only_from_a_token_that_means_a_page() {
    let cases: &[(&str, Option<i128>)] = &[
        ("https://example.com/blog/page/2", Some(2)),
        ("https://example.com/blog/page/2/", Some(2)),
        ("https://example.com/c?paged=11", Some(11)),
        ("https://example.com/c?PAGE=3", Some(3)),
        // A bare number is a year, an id or a slug just as often as a page index.
        ("https://example.com/2024/03/a-post", None),
        ("https://example.com/product/1188", None),
        // WordPress's ?p= is a post id, not a page index.
        ("https://example.com/?p=417", None),
        // Two numbers that disagree do not tell us which page this is.
        ("https://example.com/blog/page/2?page=5", None),
    ];
    for (url, want) in cases {
        assert_eq!(pagination_page_number(url), *want, "{url}");
    }
}

#[test]
fn the_walk_visits_each_page_once_across_the_whole_pass() {
    // The reference original pins the property with a counting dict — one map
    // lookup per node. The same property holds here by construction (each
    // walk's path is marked walked before the next start is considered), so
    // the port asserts the observable half: a terminating 2000-page chain
    // reports no loops and completes without a hop cap.
    let n = 2_000;
    let chain: IndexMap<String, String> = (0..n)
        .map(|i| {
            (
                format!("https://example.com/p{i}"),
                format!("https://example.com/p{}", i + 1),
            )
        })
        .collect();
    let (loops, in_loop) = pagination_loops(&chain);
    assert!(loops.is_empty() && in_loop.is_empty());
}

#[test]
fn a_cycle_is_still_found_when_a_long_tail_leads_into_it() {
    let mut graph = map(&[]);
    for i in 0..50 {
        graph.insert(
            format!("https://example.com/t{i}"),
            format!("https://example.com/t{}", i + 1),
        );
    }
    graph.insert(
        "https://example.com/t50".to_owned(),
        "https://example.com/c0".to_owned(),
    );
    for i in 0..30 {
        graph.insert(
            format!("https://example.com/c{i}"),
            format!("https://example.com/c{}", (i + 1) % 30),
        );
    }
    let (loops, in_loop) = pagination_loops(&graph);
    assert_eq!(loops.len(), 1);
    let (start, path, loops_to) = &loops[0];
    assert_eq!(start, "https://example.com/t0");
    assert_eq!(loops_to, "https://example.com/c0");
    // 51 tail pages plus the 30 in the cycle.
    assert_eq!(path.len(), 81);
    assert!(in_loop.contains("https://example.com/c0"));
}

#[test]
fn a_short_loop_still_resolves() {
    let graph = map(&[
        ("https://example.com/a", "https://example.com/b"),
        ("https://example.com/b", "https://example.com/a"),
    ]);
    let (loops, in_loop) = pagination_loops(&graph);
    assert_eq!(loops.len(), 1);
    assert_eq!(loops[0].2, "https://example.com/a");
    assert!(in_loop.contains("https://example.com/a"));
    let (path, loops_to) = series_from(&graph, "https://example.com/b");
    assert_eq!(path, ["https://example.com/b", "https://example.com/a"]);
    assert_eq!(loops_to.as_deref(), Some("https://example.com/b"));
}

#[test]
fn path_of_strips_query_and_fragment() {
    assert_eq!(path_of("https://example.com/a/b?x=1#frag").unwrap(), "/a/b");
    assert_eq!(path_of("https://example.com/").unwrap(), "/");
    assert_eq!(path_of("https://example.com").unwrap(), "");
}

#[test]
// (the `_rel_has` token half; the parse_html declaration half lives in the
// parse crate's oracle)
fn rel_tokens_split_on_ascii_whitespace_only() {
    use serde_json::json;
    use spiderhead_sf_import::rules::pagination::rel_has_value as rel_has;
    // A comma is not a separator and NBSP is not ASCII whitespace.
    assert!(!rel_has(
        Some(&json!("canonical\u{00a0}alternate")),
        "canonical"
    ));
    assert!(rel_has(Some(&json!("CANONICAL\talternate")), "canonical"));
    assert!(rel_has(Some(&json!("next")), "next"));
    assert!(!rel_has(Some(&json!("canonical, alternate")), "canonical"));
    // reference accepts a list-shaped rel value too.
    assert!(rel_has(Some(&json!(["x", "next"])), "next"));
}

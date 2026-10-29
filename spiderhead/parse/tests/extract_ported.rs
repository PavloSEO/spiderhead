//! Ports of the reference unit tests for the extraction slice (duplicate, boilerplate_report,
//! markdown_extract, custom_extract, custom_search, schema_org). Each test names its source.
//! Tests that monkeypatch reference internals, drive the CLI/MCP, or measure wall time against
//! `SIGALRM` are listed in the slice report instead.

use serde_json::{json, Value};
use spiderhead_parse::boilerplate_report::{
    boilerplate_consistency_report, boilerplate_hash, BoilerplatePage, NO_BOILERPLATE_REGIONS,
};
use spiderhead_parse::custom_extract::{
    check_sample, run_extraction, run_extractor, ExtractorSpec,
};
use spiderhead_parse::custom_search::{run_filter, run_search, FilterSpec};
use spiderhead_parse::duplicate::{
    content_hash, find_duplicates, fnv1a_64, shingles, simhash, similarity, DuplicateItem,
    DuplicateOptions,
};
use spiderhead_parse::markdown_extract::extract_markdown;
use spiderhead_parse::schema_org::{check_schema, SchemaSource};
use spiderhead_parse::types::{ContentAreaConfig, CorpusDocument};

// ------------------------------------------------------------------------------- duplicate
const TEXT_A: &str = "Search engine optimization is the process of improving the quality and quantity of website traffic from search engines. SEO targets unpaid traffic rather than direct traffic or paid traffic.";
const TEXT_B: &str = "Today is an excellent day to prepare dinner with fresh vegetables and fish. The recipe is simple and quick enough even for a beginner cook.";
const TEXT_C: &str = "Company contact information: telephone number and email address.";

fn item(id: &str, text: &str) -> DuplicateItem {
    DuplicateItem {
        id: id.into(),
        text: text.into(),
        indexable: true,
    }
}

fn opts(threshold: f64) -> DuplicateOptions {
    DuplicateOptions {
        threshold,
        ..DuplicateOptions::default()
    }
}

#[test]
fn simhash_identical_texts_match() {
    assert_eq!(simhash(TEXT_A, 3), simhash(TEXT_A, 3));
    assert_eq!(similarity(simhash(TEXT_A, 3), simhash(TEXT_A, 3)), 1.0);
}

#[test]
fn simhash_different_texts_differ() {
    let (a, b) = (simhash(TEXT_A, 3), simhash(TEXT_B, 3));
    assert_ne!(a, b);
    assert!(similarity(a, b) < 0.92);
}

#[test]
fn fnv1a_is_deterministic() {
    assert_eq!(fnv1a_64("hello"), fnv1a_64("hello"));
    assert_ne!(fnv1a_64("hello"), fnv1a_64("world"));
}

#[test]
fn shingles_overlap() {
    let tokens: Vec<String> = ["a", "b", "c", "d"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    assert_eq!(
        shingles(&tokens, 3),
        vec![vec!["a", "b", "c"], vec!["b", "c", "d"]]
    );
}

#[test]
fn invalid_duplicate_threshold_is_rejected() {
    for threshold in [-0.1, 1.1, f64::NAN, f64::INFINITY] {
        let err = find_duplicates(&[], &opts(threshold))
            .unwrap_err()
            .to_string();
        assert!(err.contains("threshold"), "{err}");
    }
}

#[test]
fn exact_pair_is_reported_as_exact_not_near() {
    let items = [
        item("/page-a", TEXT_A),
        item("/page-a-copy", TEXT_A),
        item("/page-b", TEXT_B),
        item("/contacts", TEXT_C),
    ];
    let r = find_duplicates(&items, &DuplicateOptions::default()).unwrap();
    assert_eq!(r.count, 4);
    assert!(r.clusters.is_empty());
    assert_eq!(r.exact_duplicates.len(), 1);
    assert_eq!(
        r.exact_duplicates[0].members,
        vec!["/page-a", "/page-a-copy"]
    );
}

#[test]
fn near_cluster_found_while_exact_pair_excluded() {
    let common = "the quick brown fox jumps over the lazy dog every single morning near the office";
    let items = [
        item(
            "/near-1",
            &format!("{common} during the alpha release cycle"),
        ),
        item(
            "/near-2",
            &format!("{common} during the beta release cycle"),
        ),
        item("/exact-a", TEXT_A),
        item("/exact-b", TEXT_A),
    ];
    let r = find_duplicates(&items, &opts(0.8)).unwrap();
    assert!(r
        .clusters
        .iter()
        .any(|c| c.members == vec!["/near-1", "/near-2"]));
    assert!(!r
        .clusters
        .iter()
        .any(|c| c.members.contains(&"/exact-a".to_owned())));
    assert_eq!(r.exact_duplicates[0].members, vec!["/exact-a", "/exact-b"]);
}

#[test]
fn rerun_with_new_threshold_is_pure() {
    let items = [item("1", TEXT_A), item("2", TEXT_B)];
    let first = find_duplicates(&items, &opts(0.5)).unwrap();
    let second = find_duplicates(&items, &opts(0.99)).unwrap();
    assert_eq!(first, find_duplicates(&items, &opts(0.5)).unwrap());
    assert_ne!(first, second);
}

#[test]
fn default_options_carry_a_candidate_budget() {
    // Review 2026-10-27: with the band formula collapsing to one-bit bands below threshold
    // ~0.5, an unbounded default leaves scan-size inputs quadratic. The default options must
    // carry a budget; an explicit `None` remains available to opt out.
    assert!(DuplicateOptions::default()
        .max_candidate_comparisons
        .is_some());
}

#[test]
fn candidate_comparison_budget_is_enforced() {
    let items = [item("/a", TEXT_A), item("/b", TEXT_A), item("/c", TEXT_B)];
    let err = find_duplicates(
        &items,
        &DuplicateOptions {
            threshold: 0.5,
            max_candidate_comparisons: Some(0),
            ..DuplicateOptions::default()
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("budget"), "{err}");
}

#[test]
fn only_indexable_excludes_non_indexable_items_by_default() {
    let items = [
        item("/canonical-target", TEXT_A),
        DuplicateItem {
            indexable: false,
            ..item("/canonicalized-twin", TEXT_A)
        },
    ];
    let default = find_duplicates(&items, &DuplicateOptions::default()).unwrap();
    assert_eq!((default.count, default.excluded_non_indexable), (1, 1));
    assert!(default.exact_duplicates.is_empty());
    let audit = find_duplicates(
        &items,
        &DuplicateOptions {
            only_indexable: false,
            ..DuplicateOptions::default()
        },
    )
    .unwrap();
    assert_eq!((audit.count, audit.excluded_non_indexable), (2, 0));
    assert_eq!(
        audit.exact_duplicates[0].members,
        vec!["/canonical-target", "/canonicalized-twin"]
    );
}

#[test]
fn threshold_respected() {
    let base = "SEO audit checks titles meta headings links images structured data.";
    let items = [
        item(
            "1",
            &format!("{base} Unique tail one about technical crawling and indexing."),
        ),
        item(
            "2",
            &format!("{base} Completely different ending about content marketing strategy."),
        ),
    ];
    assert!(!find_duplicates(&items, &opts(0.5))
        .unwrap()
        .clusters
        .is_empty());
    assert!(find_duplicates(&items, &opts(0.99))
        .unwrap()
        .clusters
        .is_empty());
}

#[test]
fn empty_input_returns_empty() {
    let r = find_duplicates(&[], &DuplicateOptions::default()).unwrap();
    assert!(r.count == 0 && r.clusters.is_empty());
}

#[test]
fn lsh_finds_transitive_candidates() {
    let common = "the quick brown fox jumps over the lazy dog every single morning";
    let items = [
        item(
            "A",
            &format!("{common} alpha version notes for release one"),
        ),
        item("B", &format!("{common} beta version notes for release two")),
        item(
            "C",
            &format!("{common} gamma version notes for release three"),
        ),
    ];
    let r = find_duplicates(&items, &opts(0.7)).unwrap();
    let members: std::collections::HashSet<&String> =
        r.clusters.iter().flat_map(|c| &c.members).collect();
    assert!(["A", "B", "C"]
        .iter()
        .all(|m| members.contains(&(*m).to_owned())));
}

#[test]
fn fingerprints_hidden_by_default_and_on_request() {
    let items = [item("a", TEXT_A), item("b", TEXT_B)];
    assert!(find_duplicates(&items, &DuplicateOptions::default())
        .unwrap()
        .fingerprints
        .is_none());
    let with = DuplicateOptions {
        with_fingerprints: true,
        ..DuplicateOptions::default()
    };
    let keys: Vec<String> = find_duplicates(&items, &with)
        .unwrap()
        .fingerprints
        .unwrap()
        .into_keys()
        .collect();
    assert_eq!(keys, vec!["a", "b"]);
    assert!(find_duplicates(&[], &DuplicateOptions::default())
        .unwrap()
        .fingerprints
        .is_none());
    assert!(find_duplicates(&[], &with)
        .unwrap()
        .fingerprints
        .unwrap()
        .is_empty());
}

#[test]
fn bridging_pair_is_not_joined_into_a_transitive_mega_cluster() {
    let base: String = (0..300)
        .map(|i| format!("shared{i:03}"))
        .collect::<Vec<_>>()
        .join(" ");
    let tail = |p: &str| {
        (0..12)
            .map(|i| format!("{p}{i}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let items = [
        item("A", &format!("{base} {}", tail("onlyA"))),
        item("B", &base),
        item("C", &format!("{base} {}", tail("onlyC"))),
    ];
    let r = find_duplicates(&items, &opts(0.92)).unwrap();
    let all: std::collections::HashSet<&String> =
        r.clusters.iter().flat_map(|c| &c.members).collect();
    assert!(!(all.len() == 3));
    for c in &r.clusters {
        assert!(c.min_similarity >= 0.92 && c.pairs.iter().all(|p| p.similarity >= 0.92));
    }
}

fn templated(
    n: usize,
    template_tokens: usize,
    tail_tokens: usize,
    seed: u64,
) -> Vec<DuplicateItem> {
    let template: String = (0..template_tokens)
        .map(|i| format!("tpl{i:04}"))
        .collect::<Vec<_>>()
        .join(" ");
    let mut state = seed;
    let mut next = || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        state >> 33
    };
    (0..n)
        .map(|i| {
            let tail: String = (0..tail_tokens)
                .map(|j| format!("u{i}_{j}_{}", next()))
                .collect::<Vec<_>>()
                .join(" ");
            item(&format!("/page-{i}"), &format!("{template} {tail}"))
        })
        .collect()
}

#[test]
fn templated_corpus_does_not_collapse_into_one_false_cluster() {
    let r = find_duplicates(&templated(200, 600, 30, 1234), &opts(0.92)).unwrap();
    assert_eq!(r.count, 200);
    assert!(r.clusters.is_empty());
}

#[test]
fn templated_corpus_still_finds_a_genuine_duplicate_pair() {
    let mut items = templated(40, 200, 20, 55);
    let extra: String = (0..40)
        .map(|i| format!("extra{i:03}"))
        .collect::<Vec<_>>()
        .join(" ");
    let template: String = (0..200)
        .map(|i| format!("tpl{i:04}"))
        .collect::<Vec<_>>()
        .join(" ");
    items[0].text = format!("{template} {extra} tailone");
    items[1].text = format!("{template} {extra} tailtwo");
    let r = find_duplicates(&items, &opts(0.92)).unwrap();
    assert_eq!(r.clusters.len(), 1);
    assert_eq!(r.clusters[0].members, vec!["/page-0", "/page-1"]);
}

#[test]
fn items_without_id_or_text_and_duplicate_ids_are_counted() {
    let items = [
        item("a", &"x".repeat(50)),
        item("", &"y".repeat(50)),
        item("b", &"z".repeat(50)),
    ];
    let r = find_duplicates(&items, &DuplicateOptions::default()).unwrap();
    assert_eq!((r.excluded_no_id, r.count), (1, 2));
    let items = [
        item("empty", ""),
        item("whitespace", " \n\t "),
        item("content", TEXT_A),
    ];
    let r = find_duplicates(&items, &DuplicateOptions::default()).unwrap();
    assert_eq!((r.count, r.excluded_no_text), (1, 2));
    assert!(r.exact_duplicates.is_empty());
    let items = [item("a", TEXT_A), item("a", TEXT_B), item("b", TEXT_A)];
    let r = find_duplicates(&items, &DuplicateOptions::default()).unwrap();
    assert_eq!((r.count, r.excluded_duplicate_id), (2, 1));
    assert_eq!(r.exact_duplicates[0].hash, content_hash(TEXT_A));
    assert_eq!(r.exact_duplicates[0].members, vec!["a", "b"]);
}

#[test]
fn budgets_raise_a_distinct_error() {
    let items = templated(10, 50, 10, 3);
    let few = DuplicateOptions {
        max_shingles: Some(5),
        ..DuplicateOptions::default()
    };
    assert!(matches!(
        find_duplicates(&items, &few),
        Err(spiderhead_parse::ParseError::BudgetExceeded(_))
    ));
}

// ------------------------------------------------------------------------------- boilerplate
const HEADER: &str = "<header><a href='/'>Logo</a></header>";
const NAV: &str = "<nav><a href='/a'>A</a><a href='/b'>B</a><a href='/c'>C</a></nav>";
const FOOTER: &str = "<footer><a href='/contact'>Contact</a><a href='/terms'>Terms</a></footer>";

fn good_page() -> String {
    format!("<html><body>{HEADER}{NAV}<main>page content</main>{FOOTER}</body></html>")
}

fn bad_page() -> String {
    format!("<html><body>{HEADER}{NAV}<main>other content</main><footer><a href='/contact'>Contact</a></footer></body></html>")
}

fn page(url: &str, html: &str) -> BoilerplatePage {
    BoilerplatePage {
        url: url.into(),
        html: Some(html.into()),
        hash: None,
    }
}

#[test]
fn boilerplate_hash_equality_order_and_templates() {
    assert_eq!(
        boilerplate_hash(&good_page()),
        boilerplate_hash(&good_page())
    );
    assert_ne!(
        boilerplate_hash(&good_page()),
        boilerplate_hash(&bad_page())
    );
    assert_ne!(
        boilerplate_hash("<body><header>Brand</header><nav>Menu</nav></body>"),
        boilerplate_hash("<body><nav>Menu</nav><header>Brand</header></body>")
    );
    let inert = "<template><footer>Unreleased contact block</footer></template>";
    assert_eq!(
        boilerplate_hash(&good_page()),
        boilerplate_hash(&good_page().replace("</body>", &format!("{inert}</body>")))
    );
}

#[test]
fn nested_template_content_does_not_create_a_minority_group() {
    let html = |draft: &str| {
        format!("<html><body><header><nav>Same live menu<template><footer>{draft}</footer></template></nav></header><main>Article</main></body></html>")
    };
    let pages = [
        page("a", &html("draft-a")),
        page("b", &html("draft-a")),
        page("c", &html("draft-b")),
    ];
    assert!(boilerplate_consistency_report(&pages)
        .unwrap()
        .minority_groups
        .unwrap()
        .is_empty());
}

#[test]
fn report_flags_exactly_the_page_with_the_truncated_footer() {
    let mut pages: Vec<BoilerplatePage> = (1..5)
        .map(|i| page(&format!("https://site.tld/p{i}"), &good_page()))
        .collect();
    pages.push(page("https://site.tld/old-legacy-page", &bad_page()));
    let r = boilerplate_consistency_report(&pages).unwrap();
    assert_eq!(r.count, 5);
    assert_eq!(r.groups.iter().find(|g| g.dominant).unwrap().count, 4);
    let minority = r.minority_groups.unwrap();
    assert_eq!(minority.len(), 1);
    assert_eq!(minority[0].urls, vec!["https://site.tld/old-legacy-page"]);
    assert_eq!(minority[0].fraction, 1.0 / 5.0);
}

#[test]
fn report_empty_precomputed_and_sentinel_cases() {
    let same: Vec<BoilerplatePage> = (0..3)
        .map(|i| page(&format!("p{i}"), &good_page()))
        .collect();
    let r = boilerplate_consistency_report(&same).unwrap();
    assert!(r.minority_groups.unwrap().is_empty());
    assert_eq!(r.groups[0].fraction, 1.0);
    let hashed: Vec<BoilerplatePage> = [("a", "same"), ("b", "same"), ("c", "different")]
        .iter()
        .map(|(u, h)| BoilerplatePage {
            url: (*u).into(),
            html: None,
            hash: Some((*h).into()),
        })
        .collect();
    let r = boilerplate_consistency_report(&hashed).unwrap();
    assert_eq!(r.dominant_hash.as_deref(), Some("same"));
    assert_eq!(r.minority_groups.unwrap()[0].urls, vec!["c"]);
    let empty = boilerplate_consistency_report(&[]).unwrap();
    assert_eq!(
        serde_json::to_value(&empty).unwrap(),
        json!({"ok": true, "count": 0, "dominant_hash": null, "groups": []})
    );
    assert_eq!(
        boilerplate_hash("<html><body><main>Just content</main></body></html>"),
        NO_BOILERPLATE_REGIONS
    );
    let mixed = [
        page("x/a", &good_page()),
        page(
            "x/b",
            "<html><body><main>Totally different page A</main></body></html>",
        ),
        page(
            "x/c",
            "<html><body><div>Totally different page B</div></body></html>",
        ),
    ];
    let r = boilerplate_consistency_report(&mixed).unwrap();
    assert_ne!(r.dominant_hash.as_deref(), Some(NO_BOILERPLATE_REGIONS));
    assert_eq!(r.no_boilerplate_count, Some(2));
    assert!(
        r.groups
            .iter()
            .find(|g| g.hash != NO_BOILERPLATE_REGIONS)
            .unwrap()
            .dominant
    );
}

// ------------------------------------------------------------------------------- markdown
const MD_HTML: &str = "<html><body>\n<nav>Home About Contact</nav>\n<main id=\"content\">\n<h1>Widget Pro</h1>\n<p>A <strong>durable</strong> widget for <em>everyone</em>.</p>\n<ul><li>Feature one</li><li>Feature two</li></ul>\n<p>See the <a href=\"/specs\">specs</a> for details.</p>\n</main>\n<footer>Copyright 2026 Acme</footer>\n</body></html>";

#[test]
fn markdown_content_full_and_default_strategy() {
    let config = ContentAreaConfig {
        include_selector: Some("#content".into()),
        ..ContentAreaConfig::default()
    };
    let out = extract_markdown(MD_HTML, Some(&config)).unwrap();
    let md = &out.content_markdown;
    for want in [
        "# Widget Pro",
        "**durable**",
        "*everyone*",
        "- Feature one",
        "- Feature two",
        "[specs](/specs)",
    ] {
        assert!(md.contains(want), "{want}");
    }
    assert!(!md.contains("Home About Contact") && !md.contains("Copyright"));
    assert_eq!(out.content_area_strategy, "include_selector");
    assert!(
        out.full_markdown.contains("Home About Contact")
            && out.full_markdown.contains("Copyright 2026 Acme")
    );
    let auto = extract_markdown(MD_HTML, None).unwrap();
    assert_eq!(auto.content_area_strategy, "auto_main");
    assert!(!auto.content_markdown.contains("Copyright"));
}

#[test]
fn markdown_wrappers_svg_and_math() {
    let out = extract_markdown("<main><article><h1>Widget guide</h1><p>Intro text.</p><ul><li>First step</li><li>Second step</li></ul></article></main>", None).unwrap();
    for want in [
        "# Widget guide",
        "Intro text.",
        "- First step",
        "- Second step",
    ] {
        assert!(out.content_markdown.contains(want));
    }
    assert!(!out.content_markdown.contains("Widget guideIntro text."));
    let deep = extract_markdown(
        "<section><article><h2>Deep heading</h2><ul><li>Only item</li></ul></article></section>",
        None,
    )
    .unwrap();
    assert!(
        deep.full_markdown.contains("## Deep heading")
            && deep.full_markdown.contains("- Only item")
    );
    let svg = extract_markdown("<main><p>Real content paragraph.</p><svg><title>Icon: home</title><text>Decorative label text</text></svg></main>", None).unwrap();
    assert_eq!(
        (svg.content_markdown.as_str(), svg.full_markdown.as_str()),
        ("Real content paragraph.", "Real content paragraph.")
    );
    let math = extract_markdown(
        "<main><p>Real content paragraph.</p><math><mi>x</mi><mo>+</mo><mn>1</mn></math></main>",
        None,
    )
    .unwrap();
    assert_eq!(math.content_markdown, "Real content paragraph.");
    let plain = extract_markdown("<main><h1>Title</h1><p>Para <b>bold</b> and <i>italic</i>.</p><ul><li>one</li><li>two</li></ul><p>A <a href=\"/x\">link</a>.</p></main>", None).unwrap();
    for want in [
        "# Title",
        "Para **bold** and *italic*.",
        "- one",
        "- two",
        "[link](/x)",
    ] {
        assert!(plain.content_markdown.contains(want), "{want}");
    }
}

// ------------------------------------------------------------------------------- custom_extract
fn doc(url: &str, html: &str, ok: bool, rendered: bool) -> CorpusDocument {
    CorpusDocument {
        url: url.into(),
        ok,
        html: Some(html.into()),
        text: None,
        rendered,
    }
}

fn spec(value: Value) -> ExtractorSpec {
    serde_json::from_value(value).unwrap()
}

#[test]
fn css_xpath_and_regex_modes() {
    let docs = [
        doc(
            "https://example.com/a",
            "<div class=\"price\">$19.99</div>",
            true,
            false,
        ),
        doc(
            "https://example.com/b",
            "<div>no price here</div>",
            true,
            false,
        ),
    ];
    let r = run_extractor(
        &docs,
        &spec(json!({"name": "price", "mode": "css", "query": ".price", "output": "text"})),
        2.0,
    )
    .unwrap();
    assert_eq!(r.rows[0].values, vec!["$19.99"]);
    assert_eq!((r.rows[0].count, r.rows[0].budget_exceeded), (1, false));
    assert!(r.rows[1].values.is_empty());
    let one = [doc("u", "<div class=\"x\"><b>bold</b></div>", true, false)];
    let element = run_extractor(
        &one,
        &spec(json!({"mode": "css", "query": ".x", "output": "element"})),
        2.0,
    )
    .unwrap();
    let inner = run_extractor(
        &one,
        &spec(json!({"mode": "css", "query": ".x", "output": "html"})),
        2.0,
    )
    .unwrap();
    assert!(element.rows[0].values[0].contains("<div class=\"x\">"));
    assert_eq!(inner.rows[0].values[0], "<b>bold</b>");
    let h1 = [doc(
        "u",
        "<html><body><h1>Title Here</h1></body></html>",
        true,
        false,
    )];
    let x = run_extractor(
        &h1,
        &spec(json!({"mode": "xpath", "query": "//h1/text()", "output": "text"})),
        2.0,
    )
    .unwrap();
    assert_eq!(x.rows[0].values, vec!["Title Here"]);
    let ga = [doc("u", "counter id: UA-12345-1 end", true, false)];
    let g = run_extractor(
        &ga,
        &spec(json!({"mode": "regex", "query": "UA-(\\d+-\\d+)", "output": "group", "group": 1})),
        2.0,
    )
    .unwrap();
    assert_eq!(g.rows[0].values, vec!["12345-1"]);
    assert!(g.notes.join(" ").contains("raw HTML"));
}

#[test]
fn regex_group_edge_cases_do_not_raise() {
    let docs = [doc("u", "<div>price: 100</div>", true, false)];
    let run = |s: Value| {
        run_extraction(&docs, &[spec(s)], 2.0).unwrap().extractors[0].rows[0]
            .values
            .clone()
    };
    assert!(run(json!({"mode": "regex", "query": "price:\\s*\\d+"})).is_empty());
    assert!(run(json!({"mode": "regex", "query": "price:\\s*(\\d+)", "group": 5})).is_empty());
    assert_eq!(
        run(json!({"mode": "regex", "query": "price:\\s*(\\d+)"})),
        vec!["100"]
    );
}

#[test]
fn fetch_failures_and_bad_specs() {
    let docs = [
        doc("a", "<p>x</p>", true, false),
        doc("b", "", false, false),
    ];
    let r = run_extractor(
        &docs,
        &spec(json!({"mode": "css", "query": "p", "output": "text"})),
        2.0,
    )
    .unwrap();
    assert_eq!((r.pages_considered, r.pages_excluded_fetch_failed), (1, 1));
    let one = &docs[..1];
    for (bad, word) in [
        (json!({"mode": "bogus", "query": "p"}), "mode"),
        (
            json!({"mode": "css", "query": "p", "output": "group"}),
            "output",
        ),
        (json!({"mode": "css", "query": ""}), "query"),
    ] {
        let err = run_extractor(one, &spec(bad), 2.0).unwrap_err().to_string();
        assert!(err.contains(word), "{err}");
    }
}

#[test]
fn run_extraction_and_check_sample() {
    let docs = [doc(
        "u",
        "<div class=\"price\">$5</div><h1>Widget</h1>",
        true,
        false,
    )];
    let out = run_extraction(
        &docs,
        &[
            spec(json!({"name": "price", "query": ".price", "output": "text"})),
            spec(json!({"name": "title", "query": "h1", "output": "text"})),
        ],
        2.0,
    )
    .unwrap();
    assert_eq!(
        out.extractors
            .iter()
            .map(|e| e.name.as_str())
            .collect::<Vec<_>>(),
        vec!["price", "title"]
    );
    let html = "<div class=\"price\">$19.99</div><h1>Title</h1>";
    let r = check_sample(
        html,
        &[
            spec(json!({"name": "price", "query": ".price", "output": "text"})),
            spec(
                json!({"name": "title", "mode": "xpath", "query": "//h1/text()", "output": "text"}),
            ),
        ],
        "https://example.com/saved",
        2.0,
    )
    .unwrap();
    assert_eq!(
        (r.url.as_str(), r.representation.clone()),
        ("https://example.com/saved", json!("static_markup"))
    );
    assert_eq!(r.extractors[0].values, vec!["$19.99"]);
    assert_eq!(r.extractors[0].reason, None);
    assert_eq!(r.extractors[1].values, vec!["Title"]);
    let nm = check_sample(
        "<p>nothing here</p>",
        &[spec(json!({"name": "price", "query": ".price"}))],
        "",
        2.0,
    )
    .unwrap();
    assert_eq!(nm.extractors[0].reason.as_deref(), Some("no_match"));
    let empty = check_sample(
        "   ",
        &[spec(json!({"name": "price", "query": ".price"}))],
        "",
        2.0,
    )
    .unwrap();
    assert_eq!(empty.extractors[0].reason.as_deref(), Some("empty_html"));
    let rx = check_sample(
        "<div>price: 100</div>",
        &[spec(
            json!({"name": "p", "mode": "regex", "query": "price:\\s*(\\d+)", "output": "group"}),
        )],
        "",
        2.0,
    )
    .unwrap();
    assert_eq!(rx.extractors[0].values, vec!["100"]);
    assert!(rx.extractors[0]
        .notes
        .iter()
        .any(|n| n.contains("raw HTML")));
    assert!(check_sample(
        "<p>x</p>",
        &[spec(json!({"name": "bad", "mode": "jq", "query": "."}))],
        "",
        2.0
    )
    .is_err());
}

#[test]
fn pathological_regex_is_bounded_not_hung() {
    // (the wall-clock SIGALRM budget is a deadline + backtrack limit here; the regex engine may
    // also simply finish the input, so only termination and the healthy rows are asserted)
    let docs = [
        doc("ok1", "aaab", true, false),
        doc("evil", &format!("{}c", "a".repeat(30)), true, false),
        doc("ok2", "aaab", true, false),
    ];
    let out = run_extraction(
        &docs,
        &[spec(
            json!({"mode": "regex", "query": "(a+)+b", "output": "text"}),
        )],
        0.3,
    )
    .unwrap();
    let rows = &out.extractors[0].rows;
    assert_eq!(rows[0].values, vec!["aaab"]);
    assert_eq!(rows[2].values, vec!["aaab"]);
    assert!(!rows[0].budget_exceeded && !rows[2].budget_exceeded);
}

// ------------------------------------------------------------------------------- custom_search
fn sdoc(url: &str, html: Option<&str>, ok: bool) -> CorpusDocument {
    CorpusDocument {
        url: url.into(),
        ok,
        html: html.map(str::to_owned),
        text: None,
        rendered: false,
    }
}

fn fspec(value: Value) -> FilterSpec {
    serde_json::from_value(value).unwrap()
}

#[test]
fn not_contains_excludes_failed_fetches_from_the_denominator() {
    let mut docs: Vec<CorpusDocument> = (1..=860)
        .map(|i| {
            sdoc(
                &format!("https://example.com/with/{i}"),
                Some("<div>consent-banner</div>"),
                true,
            )
        })
        .collect();
    docs.extend((1..=40).map(|i| {
        sdoc(
            &format!("https://example.com/without/{i}"),
            Some("<div>no banner here</div>"),
            true,
        )
    }));
    docs.extend((1..=5).map(|i| sdoc(&format!("https://example.com/failed/{i}"), None, false)));
    let r = run_filter(&docs, &fspec(json!({"name": "consent", "mode": "not_contains", "kind": "text", "scope": "raw", "query": "consent-banner"}))).unwrap();
    assert_eq!(
        (r.pages_considered, r.pages_excluded_fetch_failed, r.count),
        (900, 5, 40)
    );
    assert!(r.matching_pages.iter().all(|u| u.contains("without")));
}

#[test]
fn contains_regex_text_element_and_xpath_scopes() {
    let docs = [
        sdoc("a", Some("<script>gtag('config')</script>"), true),
        sdoc("b", Some("<p>nothing here</p>"), true),
    ];
    let r = run_filter(
        &docs,
        &fspec(json!({"mode": "contains", "scope": "raw", "query": "gtag("})),
    )
    .unwrap();
    assert_eq!((r.matching_pages, r.fraction), (vec!["a".to_owned()], 0.5));
    let phones = [
        sdoc("a", Some("phone: +1-555-000-1111"), true),
        sdoc("b", Some("no phone here"), true),
    ];
    let r = run_filter(&phones, &fspec(json!({"mode": "contains", "kind": "regex", "scope": "raw", "query": "\\+1-555-\\d{3}-\\d{4}"}))).unwrap();
    assert_eq!(r.matching_pages, vec!["a"]);
    let tracking = [sdoc(
        "a",
        Some("<script>trackingId</script><p>Hello</p>"),
        true,
    )];
    assert_eq!(
        run_filter(
            &tracking,
            &fspec(json!({"scope": "raw", "query": "trackingId"}))
        )
        .unwrap()
        .count,
        1
    );
    assert_eq!(
        run_filter(
            &tracking,
            &fspec(json!({"scope": "text", "query": "trackingId"}))
        )
        .unwrap()
        .count,
        0
    );
    let price = [
        sdoc("a", Some("<div class=\"price\">$19.99</div>"), true),
        sdoc("b", Some("<div>no price div</div>"), true),
    ];
    let r = run_filter(&price, &fspec(json!({"mode": "not_contains", "kind": "regex", "scope": "element", "selector": ".price", "query": "\\$\\d"}))).unwrap();
    assert_eq!(r.matching_pages, vec!["b"]);
    let h1 = [
        sdoc(
            "a",
            Some("<html><body><h1>Special Title</h1></body></html>"),
            true,
        ),
        sdoc("b", Some("<html><body><h1>Other</h1></body></html>"), true),
    ];
    let r = run_filter(
        &h1,
        &fspec(json!({"scope": "xpath", "selector": "//h1/text()", "query": "Special"})),
    )
    .unwrap();
    assert_eq!(r.matching_pages, vec!["a"]);
}

#[test]
fn malformed_inputs_are_rejected_not_treated_as_no_match() {
    let docs = [sdoc("a", Some("<p>banner</p>"), true)];
    for (bad, word) in [
        (json!({"scope": "xpath", "query": "x"}), "selector"),
        (
            json!({"mode": "not_contains", "scope": "element", "selector": "div[", "query": "banner"}),
            "invalid CSS selector",
        ),
        (
            json!({"mode": "not_contains", "scope": "xpath", "selector": "//h1[", "query": "banner"}),
            "invalid XPath expression",
        ),
        (
            json!({"mode": "not_contains", "kind": "regex", "scope": "raw", "query": "banner("}),
            "invalid regex",
        ),
        (json!({"scope": "bogus", "query": "x"}), "scope"),
    ] {
        let err = run_filter(&docs, &fspec(bad)).unwrap_err().to_string();
        assert!(err.contains(word), "{err}");
    }
    let two = [
        sdoc("a", Some("<p>banner</p>"), true),
        sdoc("b", Some("<p>banner</p>"), true),
    ];
    let r = run_filter(&two, &fspec(json!({"mode": "not_contains", "scope": "element", "selector": ".nonexistent-class", "query": "banner"}))).unwrap();
    assert_eq!(r.count, 2);
}

#[test]
fn xpath_element_scope_keeps_nested_and_tail_text() {
    let run = |html: &str, selector: &str, query: &str| {
        run_filter(
            &[sdoc("a", Some(html), true)],
            &fspec(json!({"scope": "xpath", "selector": selector, "query": query})),
        )
        .unwrap()
        .matching_pages
    };
    assert_eq!(
        run(
            "<h1><strong>Special</strong> Title</h1>",
            "//h1",
            "Special Title"
        ),
        vec!["a"]
    );
    assert_eq!(
        run("<p>Hello <b>world</b> and more</p>", "//p", "and more"),
        vec!["a"]
    );
    assert_eq!(run("<h1>Plain</h1>", "//h1/text()", "Plain"), vec!["a"]);
}

#[test]
fn representation_search_empty_corpus_and_svg() {
    let static_doc = doc("a", "<p>x</p>", true, false);
    let rendered = doc("a", "<p>x</p>", true, true);
    let rep = |docs: &[CorpusDocument]| {
        run_filter(docs, &fspec(json!({"scope": "raw", "query": "x"})))
            .unwrap()
            .representation
    };
    assert_eq!(
        rep(std::slice::from_ref(&static_doc)),
        json!("static_markup")
    );
    assert_eq!(rep(std::slice::from_ref(&rendered)), json!("rendered_dom"));
    assert_eq!(
        rep(&[static_doc, rendered]),
        json!(["rendered_dom", "static_markup"])
    );
    let out = run_search(
        &[sdoc("a", Some("foo bar"), true)],
        &[
            fspec(json!({"name": "foo", "scope": "raw", "query": "foo"})),
            fspec(json!({"name": "baz", "scope": "raw", "query": "baz"})),
        ],
    )
    .unwrap();
    assert_eq!(
        out.filters
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        vec!["foo", "baz"]
    );
    assert_eq!((out.filters[0].count, out.filters[1].count), (1, 0));
    let empty = run_filter(
        &[],
        &fspec(json!({"mode": "not_contains", "scope": "raw", "query": "x"})),
    )
    .unwrap();
    assert_eq!((empty.pages_considered, empty.fraction), (0, 0.0));
    let svg = [sdoc(
        "a",
        Some("<html><body><svg><text>consent-banner</text></svg><p>Hello</p></body></html>"),
        true,
    )];
    let rule = |mode: &str| {
        run_filter(
            &svg,
            &fspec(
                json!({"kind": "text", "scope": "text", "query": "consent-banner", "mode": mode}),
            ),
        )
        .unwrap()
        .count
    };
    assert_eq!((rule("contains"), rule("not_contains")), (0, 1));
}

// ------------------------------------------------------------------------------- schema_org
fn ld(json_str: &str) -> String {
    format!("<html><head><script type=\"application/ld+json\">{json_str}</script></head><body></body></html>")
}

fn check(html: &str) -> Value {
    serde_json::to_value(check_schema(SchemaSource::Html(html), None, 0.0, None).unwrap()).unwrap()
}

fn rich<'a>(r: &'a Value, kind: &str) -> &'a Value {
    r["rich_results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["type"] == kind)
        .unwrap()
}

#[test]
fn schema_blocks_vocabulary_and_inheritance() {
    let r = check(
        "<html><head><div itemscope><span itemprop=\"x\">y</span></div></head><body></body></html>",
    );
    assert_eq!(r["blocks"], 0);
    assert!(r["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f.as_str().unwrap().contains("JSON-LD")));
    assert_eq!(
        (
            r["other_markup"]["microdata"].as_bool(),
            r["other_markup"]["rdfa"].as_bool()
        ),
        (Some(true), Some(false))
    );
    let r = check(&ld("{\"@type\": \"Article\", \"headline\": }"));
    let errors = r["parse_errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1);
    assert!(errors[0]
        .as_str()
        .unwrap()
        .to_lowercase()
        .contains("json-ld block #1"));
    let r = check(&ld("{\"@context\":\"https://schema.org\",\"@type\":\"Article\",\"headline\":\"T\",\"author\":{\"@type\":\"Person\",\"name\":\"Joe\"}}"));
    assert!(!r["entities"][0]["errors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e.as_str().unwrap().contains("author")));
    assert_eq!(
        check(&ld("{\"@type\":\"BogusType\",\"name\":\"x\"}"))["entities"][0]["errors"],
        json!(["Type BogusType is not present in the Schema.org vocabulary"])
    );
    assert_eq!(
        check(&ld(
            "{\"@type\":\"Article\",\"totallyMadeUp\":\"x\",\"headline\":\"t\"}"
        ))["entities"][0]["errors"],
        json!(["Property totallyMadeUp is not present in the Schema.org vocabulary"])
    );
    let r = check(&ld("{\"@type\":\"Code\",\"name\":\"x\"}"));
    assert!(r["entities"][0]["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w.as_str().unwrap().contains("SoftwareSourceCode")));
    let r = check(&ld("{\"@type\":\"3DModel\",\"name\":\"x\"}"));
    assert!(r["entities"][0]["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w.as_str().unwrap().contains("pending")));
    let r = check(&ld(
        "{\"@type\":\"Article\",\"headline\":\"t\",\"servesCuisine\":\"pizza\"}",
    ));
    assert!(r["entities"][0]["errors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e.as_str().unwrap().contains("servesCuisine")));
}

#[test]
fn schema_graph_integrity() {
    let r = check(&ld(
        "{\"@type\":\"Article\",\"headline\":\"t\",\"author\":{\"@id\":\"#joe\"}}",
    ));
    let flat: Vec<String> = r["entities"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|e| e["errors"].as_array().unwrap().clone())
        .map(|e| e.as_str().unwrap().to_owned())
        .collect();
    assert!(flat
        .iter()
        .any(|e| e.contains("#joe") && e.contains("author")));
    let r = check(&ld("[{\"@type\":\"Organization\",\"@id\":\"#org\",\"name\":\"A\"},{\"@type\":\"WebPage\",\"@id\":\"#page\",\"name\":\"P\"}]"));
    assert_eq!(
        (
            r["graph"]["nodes"].as_u64(),
            r["graph"]["is_graph"].as_bool()
        ),
        (Some(2), Some(false))
    );
    assert!(r["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f.as_str().unwrap().contains("isolated blocks")));
    assert_eq!(r["graph"]["islands"].as_array().unwrap().len(), 2);
    let r = check(&ld("[{\"@type\":\"Organization\",\"@id\":\"#org\",\"name\":\"A\"},{\"@type\":\"WebPage\",\"@id\":\"#page\",\"name\":\"P\",\"publisher\":{\"@id\":\"#org\"}}]"));
    assert_eq!(r["graph"]["is_graph"], true);
    assert!(r["graph"]["linked_by_id"].as_u64().unwrap() >= 1);
    let r = check(&ld("{\"@context\":\"https://schema.org\",\"@type\":\"Product\",\"name\":\"Widget\",\"offers\":{\"@type\":\"Offer\",\"price\":19.99,\"priceCurrency\":\"USD\"}}"));
    assert_eq!(r["graph"]["nodes"], 2);
    assert_eq!(r["graph"]["islands"], json!([]));
    let r = check(&ld("[{\"@type\":\"WebPage\",\"@id\":\"#page\",\"publisher\":{\"@id\":\"#org\",\"@type\":\"Organization\",\"name\":\"Acme\"}},{\"@type\":\"Organization\",\"@id\":\"#org\",\"name\":\"Acme\"}]"));
    assert_eq!(
        (
            r["graph"]["is_graph"].clone(),
            r["graph"]["islands"].clone()
        ),
        (json!(true), json!([]))
    );
}

#[test]
fn schema_rich_results() {
    let r = check(&ld("{\"@type\":\"Product\",\"description\":\"x\"}"));
    assert_eq!(rich(&r, "Product")["eligible"], false);
    assert!(rich(&r, "Product")["missing_required"]
        .as_array()
        .unwrap()
        .contains(&json!("name")));
    assert_eq!(r["rules_version"], rich(&r, "Product")["rules_version"]);
    let faq = check(&ld("{\"@type\":\"FAQPage\",\"mainEntity\":[{\"@type\":\"Question\",\"name\":\"q\",\"acceptedAnswer\":{\"@type\":\"Answer\",\"text\":\"a\"}}]}"));
    assert!(rich(&faq, "FAQPage").get("note").is_some());
    let art = check(&ld(
        "{\"@type\":\"Article\",\"author\":{\"@type\":\"Person\",\"name\":\"Ada\"}}",
    ));
    assert_eq!(rich(&art, "Article")["eligible"], true);
    assert!(rich(&art, "Article")["missing_recommended"]
        .as_array()
        .unwrap()
        .contains(&json!("headline")));
    let offer = check(&ld("{\"@type\":\"Product\",\"name\":\"Widget\",\"offers\":{\"@type\":\"Offer\",\"price\":19.99}}"));
    assert!(!offer["rich_results"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x["type"] == "Offer"));
    let name_only = check(&ld("{\"@type\":\"Product\",\"name\":\"Widget\"}"));
    assert_eq!(rich(&name_only, "Product")["eligible"], false);
    assert_eq!(
        rich(&name_only, "Product")["missing_required_any_of"],
        json!(["review", "aggregateRating", "offers"])
    );
    let ok = check(&ld("{\"@type\":\"Product\",\"name\":\"Widget\",\"offers\":{\"@type\":\"Offer\",\"price\":19.99,\"priceCurrency\":\"USD\"}}"));
    assert_eq!(
        (
            rich(&ok, "Product")["eligible"].clone(),
            rich(&ok, "Product")["missing_required_any_of"].clone()
        ),
        (json!(true), json!([]))
    );
    assert!(ok["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f.as_str().unwrap().contains("not a guarantee")));
    let app = check(&ld("{\"@type\":\"SoftwareApplication\",\"name\":\"App\",\"offers\":{\"@type\":\"Offer\",\"price\":0,\"priceCurrency\":\"USD\"}}"));
    assert_eq!(rich(&app, "SoftwareApplication")["eligible"], false);
    assert_eq!(
        rich(&app, "SoftwareApplication")["missing_required_any_of"],
        json!(["aggregateRating", "review"])
    );
    let rated = check(&ld("{\"@type\":\"SoftwareApplication\",\"name\":\"App\",\"offers\":{\"@type\":\"Offer\",\"price\":0,\"priceCurrency\":\"USD\"},\"aggregateRating\":{\"@type\":\"AggregateRating\",\"ratingValue\":4.6,\"ratingCount\":9}}"));
    assert_eq!(rich(&rated, "SoftwareApplication")["eligible"], true);
    let no_offers = check(&ld("{\"@type\":\"SoftwareApplication\",\"name\":\"App\",\"review\":{\"@type\":\"Review\",\"author\":{\"@type\":\"Person\",\"name\":\"Ada\"}}}"));
    assert_eq!(
        rich(&no_offers, "SoftwareApplication")["missing_required"],
        json!(["offers"])
    );
}

#[test]
fn schema_findings_and_contexts() {
    let r = check(&ld(
        "{\"@type\":\"Article\",\"headline\":\"t\",\"bogusProp\":\"x\"}",
    ));
    assert!(r["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f.as_str().unwrap().contains("Vocabulary errors: 1")));
    let r = check(&ld("{\"@context\":\"https://schema.org\",\"@type\":\"Article\",\"headline\":\"T\",\"author\":{\"@type\":\"Person\",\"name\":\"Joe\"}}"));
    let vocabularies = r["vocabularies"].as_array().unwrap();
    assert!(vocabularies
        .iter()
        .any(|v| v["context"] == "https://schema.org"
            && v["supported"] == true
            && v["blocks"] == 1));
    let body = "<script type=\"application/ld+json\">{\"@context\":\"https://schema.org\",\"@type\":\"Article\",\"headline\":\"T\"}</script><script type=\"application/ld+json\">{\"@context\":\"https://example.org/x\",\"@type\":\"Foo\",\"name\":\"bar\"}</script>";
    let r = check(&format!("<html><head>{body}</head><body></body></html>"));
    assert_eq!(r["blocks"], 2);
    let foo = r["entities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["types"].as_array().unwrap().contains(&json!("Foo")))
        .unwrap();
    assert_eq!(foo["errors"], json!([]));
    assert_eq!(foo["warnings"], json!(["Node uses unsupported vocabulary https://example.org/x; Schema.org validation was skipped"]));
    let by_ctx: Vec<(&str, bool)> = r["vocabularies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                v["context"].as_str().unwrap(),
                v["supported"].as_bool().unwrap(),
            )
        })
        .collect();
    assert!(
        by_ctx.contains(&("https://schema.org", true))
            && by_ctx.contains(&("https://example.org/x", false))
    );
}

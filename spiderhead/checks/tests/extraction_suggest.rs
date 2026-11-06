//! Oracle tests for text-driven selector suggestions.
//!
//! Every candidate's match count is measured with the same closed selector
//! engine extraction rules run under, so the emitted rule evaluates
//! identically in `scan-extract` without modification.

use std::collections::HashMap;

use serde_json::{json, Value};
use spiderhead_checks::extraction_suggest::suggest;

const SHOP_HTML: &str = r#"
<html><body>
<header><nav><a href="/">Home</a></nav></header>
<main id="content">
  <h1>Catalog</h1>
  <div class="catalog">
    <article class="product-card" itemprop="itemListElement" data-testid="product-card-1">
      <h3 class="product-title" itemprop="name">Steel Pump P-100</h3>
      <span class="price" itemprop="price">$49.00</span>
    </article>
    <article class="product-card" data-testid="product-card-2">
      <h3 class="product-title">Bronze Pump B-200</h3>
      <span class="price">$59.00</span>
    </article>
    <article class="product-card" data-testid="product-card-3">
      <h3 class="product-title">Brass Pump BR-300</h3>
      <span class="price">$69.00</span>
    </article>
  </div>
  <div id="featured"><h2 id="featured-title">Featured: Steel Pump P-100</h2></div>
</main>
</body></html>
"#;

/// Nested structure with no classes, ids, or semantic attributes anywhere.
const PLAIN_HTML: &str = "<html><body><div><div><div><p>Alpha</p><p>Beta</p></div>\
<div><p>Gamma</p></div></div></div></body></html>";

fn rank(strategy: &str) -> u8 {
    match strategy {
        "id" => 0,
        "semantic" => 1,
        "path" => 2,
        _ => 3,
    }
}

fn by_selector(result: &Value) -> HashMap<&str, &Value> {
    result["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|candidate| (candidate["selector"].as_str().unwrap(), candidate))
        .collect()
}

// from tests/test_extract_suggest.py::test_text_yields_candidates_with_measured_document_wide_counts
#[test]
fn text_yields_candidates_with_measured_document_wide_counts() {
    let out = suggest(SHOP_HTML, "Steel Pump P-100", false, None).unwrap();
    assert_eq!(out["ok"], json!(true));
    assert_eq!(out["elements_matched"], json!(1));
    let candidates = out["candidates"].as_array().unwrap();
    // The acceptance bar: one text "click" gives several candidates with
    // different match counts, measured across the whole document.
    assert!(candidates.len() >= 3);
    let counts: Vec<u64> = candidates
        .iter()
        .map(|candidate| candidate["matches"].as_u64().unwrap())
        .collect();
    assert!(counts.contains(&1));
    assert!(counts.contains(&3));
    let ranked: Vec<u8> = candidates
        .iter()
        .map(|candidate| rank(candidate["strategy"].as_str().unwrap()))
        .collect();
    let mut sorted = ranked.clone();
    sorted.sort();
    assert_eq!(ranked, sorted);
    assert_eq!(
        candidates
            .iter()
            .filter(|candidate| candidate.get("best").and_then(Value::as_bool) == Some(true))
            .count(),
        1
    );
    assert_eq!(candidates[0]["best"], json!(true));
}

// from tests/test_extract_suggest.py::test_grid_selector_sample_texts_show_the_whole_kind
#[test]
fn grid_selector_sample_texts_show_the_whole_kind() {
    let out = suggest(SHOP_HTML, "Steel Pump P-100", false, None).unwrap();
    let map = by_selector(&out);
    let grid = map["h3.product-title"];
    assert_eq!(grid["matches"], json!(3));
    assert_eq!(
        grid["sample_matches"],
        json!(["Steel Pump P-100", "Bronze Pump B-200", "Brass Pump BR-300"])
    );
}

// from tests/test_extract_suggest.py::test_id_candidate_anchors_and_wins_for_an_id_element
#[test]
fn id_candidate_anchors_and_wins_for_an_id_element() {
    let out = suggest(SHOP_HTML, "Featured: Steel Pump P-100", false, None).unwrap();
    let best = &out["candidates"][0];
    assert_eq!(best["selector"], json!("#featured-title"));
    assert_eq!(best["strategy"], json!("id"));
    assert_eq!(best["matches"], json!(1));
    assert_eq!(
        best["sample_matches"],
        json!(["Featured: Steel Pump P-100"])
    );
}

// from tests/test_extract_suggest.py::test_semantic_attribute_candidate_uses_schema_org_hook
#[test]
fn semantic_attribute_candidate_uses_schema_org_hook() {
    let out = suggest(SHOP_HTML, "Steel Pump P-100", false, None).unwrap();
    let map = by_selector(&out);
    let semantic = map[r#"h3[itemprop="name"]"#];
    assert_eq!(semantic["strategy"], json!("semantic"));
    assert_eq!(semantic["matches"], json!(1));
}

// from tests/test_extract_suggest.py::test_wrapper_whose_text_equals_the_target_is_not_a_separate_target
#[test]
fn wrapper_whose_text_equals_the_target_is_not_a_separate_target() {
    let wrapped = r#"<html><body><a href="/p1"><h3 class="title">Widget</h3></a></body></html>"#;
    let out = suggest(wrapped, "Widget", false, None).unwrap();
    assert_eq!(out["elements_matched"], json!(1));
    let selectors: Vec<&str> = out["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|candidate| candidate["selector"].as_str().unwrap())
        .collect();
    assert_eq!(selectors, vec!["h3.title"]);
}

// from tests/test_extract_suggest.py::test_nth_fallback_appears_only_when_no_candidate_is_unique
#[test]
fn nth_fallback_appears_only_when_no_candidate_is_unique() {
    let out = suggest(PLAIN_HTML, "Gamma", false, None).unwrap();
    let strategies: Vec<&str> = out["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|candidate| candidate["strategy"].as_str().unwrap())
        .collect();
    assert!(strategies.contains(&"nth"));
    assert!(strategies.iter().all(|s| *s == "path" || *s == "nth"));
    let nth = out["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["strategy"] == "nth")
        .unwrap();
    assert_eq!(nth["matches"], json!(1));

    let grid = suggest(SHOP_HTML, "Steel Pump P-100", false, None).unwrap();
    assert!(!grid["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .any(|candidate| candidate["strategy"] == "nth"));
}

// from tests/test_extract_suggest.py::test_class_names_with_css_specials_roundtrip_through_the_engine
#[test]
fn class_names_with_css_specials_roundtrip_through_the_engine() {
    let html = r#"<html><body><div class="a.b"><span class="t">Odd</span></div>"#;
    let html =
        format!("{html}<div class=\"a.c\"><span class=\"t\">Other</span></div></body></html>");
    let out = suggest(&html, "Odd", false, None).unwrap();
    let map = by_selector(&out);
    let signature = map["span.t"];
    assert_eq!(signature["matches"], json!(2));
    let anchored = map[r#"div.a\.b span.t"#];
    assert_eq!(anchored["matches"], json!(1));
    assert_eq!(anchored["sample_matches"], json!(["Odd"]));
}

// from tests/test_extract_suggest.py::test_no_match_is_reported_not_invented
#[test]
fn no_match_is_reported_not_invented() {
    let out = suggest(SHOP_HTML, "Nonexistent Waffle", false, None).unwrap();
    assert_eq!(out["ok"], json!(true));
    assert_eq!(out["candidates"], json!([]));
    assert!(out["note"]
        .as_str()
        .unwrap()
        .contains("no element with the requested text"));
}

// from tests/test_extract_suggest.py::test_script_text_never_counts_as_page_copy
#[test]
fn script_text_never_counts_as_page_copy() {
    let html = r#"<html><body><script>var t = "Steel Pump P-100";</script></body></html>"#;
    let out = suggest(html, "Steel Pump P-100", false, None).unwrap();
    assert_eq!(out["candidates"], json!([]));
}

// from tests/test_extract_suggest.py::test_empty_html_or_text_is_rejected
#[test]
fn empty_html_or_text_is_rejected() {
    for (html, text) in [
        ("", "Anything"),
        ("   ", "Anything"),
        (SHOP_HTML, ""),
        (SHOP_HTML, "   "),
    ] {
        assert!(suggest(html, text, false, None).is_err());
    }
}

// from tests/test_extract_suggest.py::test_oversized_text_is_rejected_with_the_rule_limit_named
#[test]
fn oversized_text_is_rejected_with_the_rule_limit_named() {
    let error = suggest(SHOP_HTML, &"x".repeat(2049), false, None).unwrap_err();
    assert!(error.contains("2048"), "{error}");
}

// from tests/test_extract_suggest.py::test_emitted_rule_evaluates_to_the_same_matches
#[test]
fn emitted_rule_evaluates_to_the_same_matches() {
    let out = suggest(SHOP_HTML, "Steel Pump P-100", true, None).unwrap();
    let rule = &out["rule"];
    let best = &out["candidates"][0];
    assert_eq!(rule["id"], json!("steel-pump-p-100"));
    assert_eq!(rule["kind"], json!("text"));
    assert_eq!(rule["selector"], best["selector"]);
    assert_eq!(rule["operator"], json!("equals"));
    assert_eq!(rule["value"], json!("Steel Pump P-100"));
    assert_eq!(
        rule["max_matches"].as_u64().unwrap(),
        best["matches"].as_u64().unwrap()
    );
    let result = spiderhead_parse::extraction_rules::evaluate(
        Some(SHOP_HTML),
        Some(&json!({"jsonld": []})),
        &json!([rule]),
        "static",
    )
    .unwrap();
    assert_eq!(result.state, "complete");
    let row = &result.rules[0];
    assert_eq!(row.matched, Some(true));
    let value = row.value.as_ref().unwrap();
    assert!(value
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item.as_str() == Some("Steel Pump P-100")));
    assert_eq!(row.count, Some(best["matches"].as_u64().unwrap()));
}

// from tests/test_extract_suggest.py::test_emit_rule_honors_explicit_id_and_rejects_invalid_ids
#[test]
fn emit_rule_honors_explicit_id_and_rejects_invalid_ids() {
    let out = suggest(SHOP_HTML, "Steel Pump P-100", true, Some("product-title")).unwrap();
    assert_eq!(out["rule"]["id"], json!("product-title"));
    let error = suggest(SHOP_HTML, "Steel Pump P-100", true, Some("Bad Id")).unwrap_err();
    assert!(!error.is_empty());
}

// from tests/test_extract_suggest.py::test_emit_rule_without_candidates_is_an_explicit_error
#[test]
fn emit_rule_without_candidates_is_an_explicit_error() {
    let error = suggest(SHOP_HTML, "Nonexistent Waffle", true, None).unwrap_err();
    assert!(error.contains("no candidate"), "{error}");
}

// from tests/test_extract_suggest.py::test_non_latin_text_still_gets_a_valid_default_rule_id
#[test]
fn non_latin_text_still_gets_a_valid_default_rule_id() {
    let russian = "<html><body><p>Насос стальной</p></body></html>";
    let out = suggest(russian, "Насос стальной", true, None).unwrap();
    assert_eq!(out["rule"]["id"], json!("element"));
    assert_eq!(out["candidates"][0]["selector"], json!("p"));
}

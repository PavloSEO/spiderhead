//! Ported tests for `reports/facts.rs`: the `fact` state machine and the
//! multi-site `facts.v1` export (reference the reference suite).
//!
//! The three `mcp.handlers.facts_export` tests are deferred — the handler
//! layer is not part of this crate.

use serde_json::{json, Value};
use spiderhead_reports::document::SITE_AUDIT_SCHEMA;
use spiderhead_reports::facts::{build_facts_export, fact, FactError, FACT_STATES};

fn sf_document_a() -> Value {
    json!({
        "schema_version": "2.0",
        "tool": "sf-analyzer",
        "run": {
            "input_mode": "crawl",
            "source": "https://a-example.com/",
            "generated_at": "2026-09-05T00:00:00Z",
            "crawl_partial": false,
        },
        "summary": {
            "totals": {"urls_crawled": 10, "html_indexable": 8, "issues_total": 2},
            "by_severity": {"critical": 1, "warning": 1, "notice": 0},
            "by_check": {"THIN_CONTENT": 2},
            "check_coverage": {"checks_available": 90, "checks_skipped": 5, "checks_disabled": 1},
        },
        "issues": [],
        "pages": [],
        "groups": [],
    })
}

fn sf_document_partial() -> Value {
    json!({
        "schema_version": "2.0",
        "tool": "sf-analyzer",
        "run": {
            "input_mode": "crawl",
            "source": "https://c-example.com/",
            "generated_at": "2026-09-05T00:00:00Z",
            "crawl_partial": true,
            "crawl_finish_reason": "hit max_urls before the sitemap was exhausted",
        },
        "summary": {
            "totals": {"urls_crawled": 4, "html_indexable": 0, "issues_total": 0},
            "by_severity": {"critical": 0, "warning": 0, "notice": 0},
        },
        "issues": [],
        "pages": [],
        "groups": [],
    })
}

fn site_document_a() -> Value {
    json!({
        "ok": true,
        "schema": SITE_AUDIT_SCHEMA,
        "url": "https://a-example.com/",
        "domain": "a-example.com",
        "generated_at": "2026-09-05T00:00:00Z",
        "site": {},
        "pages": [],
        "findings": [],
        "summary": {
            "pages_checked": 5,
            "findings_total": 0,
            "findings_by_severity": {"critical": 0, "warning": 0, "notice": 0},
            "tools_run": ["robots_check", "security_check"],
            "tools_failed": [],
        },
    })
}

fn site_document_b() -> Value {
    json!({
        "ok": true,
        "schema": SITE_AUDIT_SCHEMA,
        "url": "https://b-example.com/",
        "domain": "b-example.com",
        "generated_at": "2026-09-05T00:00:00Z",
        "site": {},
        "pages": [],
        "findings": [{"severity": "warning", "text": "x"}],
        "summary": {
            "pages_checked": 3,
            "findings_total": 1,
            "findings_by_severity": {"critical": 0, "warning": 1, "notice": 0},
            "tools_run": ["robots_check"],
            "tools_failed": [{"tool": "security_check", "error": "timeout"}],
        },
    })
}

fn leaf_facts(document: &Value) -> Vec<&Value> {
    document["sites"]
        .as_array()
        .expect("sites")
        .iter()
        .flat_map(|site| {
            site["facts"]
                .as_object()
                .expect("facts")
                .values()
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn fact_measured_accepts_any_value_with_no_reason() {
    let f = fact(json!(5), "measured", "s", "m", None, None, false, None).expect("fact");
    assert_eq!(f["value"], json!(5));
    assert_eq!(f["state"], json!("measured"));
    assert!(f["reason"].is_null());
}

#[test]
fn fact_absent_carries_a_true_zero_with_a_reason() {
    let f = fact(
        json!(0),
        "absent",
        "s",
        "m",
        Some("checked, found none"),
        None,
        false,
        None,
    )
    .expect("fact");
    assert_eq!(f["value"], json!(0));
    assert_eq!(f["state"], json!("absent"));
}

#[test]
fn fact_partial_requires_a_bound() {
    assert!(fact(
        json!(4),
        "partial",
        "s",
        "m",
        Some("crawl stopped early"),
        None,
        false,
        None,
    )
    .is_err());
}

#[test]
fn fact_partial_with_bound_and_reason_succeeds() {
    let f = fact(
        json!(4),
        "partial",
        "s",
        "m",
        Some("crawl stopped early"),
        Some(">= 4"),
        false,
        None,
    )
    .expect("fact");
    assert_eq!(f["state"], json!("partial"));
    assert_eq!(f["bound"], json!(">= 4"));
}

#[test]
fn fact_unavailable_must_not_carry_a_value() {
    assert!(fact(
        json!(0),
        "unavailable",
        "s",
        "m",
        Some("not run"),
        None,
        false,
        None,
    )
    .is_err());
}

#[test]
fn fact_not_requested_must_not_carry_a_value() {
    assert!(fact(
        json!("anything"),
        "not_requested",
        "s",
        "m",
        Some("never asked"),
        None,
        false,
        None,
    )
    .is_err());
}

#[test]
fn fact_non_measured_without_reason_raises() {
    assert!(fact(
        Value::Null,
        "unavailable",
        "s",
        "m",
        None,
        None,
        false,
        None
    )
    .is_err());
}

#[test]
fn fact_unknown_state_raises() {
    assert!(fact(json!(1), "bogus", "s", "m", Some("x"), None, false, None,).is_err());
}

#[test]
fn every_leaf_fact_in_an_export_obeys_the_state_invariants() {
    let document = build_facts_export(&[
        json!({"label": "a-example.com", "crawl_audit": sf_document_a(), "site_audit": site_document_a()}),
        json!({"label": "b-example.com", "site_audit": site_document_b()}),
        json!({"label": "c-example.com", "crawl_audit": sf_document_partial()}),
    ])
    .expect("export");
    let leaves = leaf_facts(&document);
    assert!(!leaves.is_empty(), "fixture produced no facts");

    let (mut measured, mut absent, mut partial, mut unavailable, mut not_requested) =
        (false, false, false, false, false);
    for leaf in &leaves {
        let state = leaf["state"].as_str().expect("state");
        assert!(FACT_STATES.contains(&state), "unknown state {state}");
        if matches!(state, "unavailable" | "not_requested") {
            assert!(
                leaf["value"].is_null(),
                "{state} fact carries a value: {leaf}"
            );
        }
        if state != "measured" {
            assert!(
                leaf["reason"].as_str().is_some_and(|r| !r.is_empty()),
                "non-measured fact with no reason: {leaf}"
            );
        }
        if state == "partial" {
            assert!(
                leaf["bound"].as_str().is_some_and(|b| !b.is_empty()),
                "partial fact with no bound: {leaf}"
            );
        }
        measured |= state == "measured";
        absent |= state == "absent";
        partial |= state == "partial";
        unavailable |= state == "unavailable";
        not_requested |= state == "not_requested";
    }
    assert!(measured && absent && partial && unavailable && not_requested);

    let site_a = document["sites"]
        .as_array()
        .expect("sites")
        .iter()
        .find(|s| s["label"] == json!("a-example.com"))
        .expect("site a");
    assert_eq!(
        site_a["facts"]["crawl_pages_crawled"],
        json!({
            "value": 10,
            "state": "measured",
            "source": "sf_audit_run",
            "method": "summary.totals.urls_crawled",
            "reason": null,
            "bound": null,
            "paid": false,
            "note": null,
            "is_not": "not the number of pages the site actually has",
        })
    );
}

#[test]
fn export_is_json_serializable() {
    let document = build_facts_export(&[json!({
        "label": "a-example.com",
        "crawl_audit": sf_document_a(),
        "site_audit": site_document_a(),
    })])
    .expect("export");
    serde_json::to_string(&document).expect("serializable");
}

#[test]
fn crawl_partial_flag_produces_a_partial_pages_crawled_fact() {
    let document = build_facts_export(&[json!({
        "label": "c-example.com",
        "crawl_audit": sf_document_partial(),
    })])
    .expect("export");
    let leaf = &document["sites"][0]["facts"]["crawl_pages_crawled"];
    assert_eq!(leaf["state"], json!("partial"));
    assert_eq!(leaf["value"], json!(4));
    assert!(leaf["reason"]
        .as_str()
        .expect("reason")
        .contains("hit max_urls"));
    assert!(leaf["bound"].as_str().is_some_and(|b| !b.is_empty()));
}

#[test]
fn zero_issues_reads_as_absent_not_unavailable() {
    let document = build_facts_export(&[json!({
        "label": "c-example.com",
        "crawl_audit": sf_document_partial(),
    })])
    .expect("export");
    let leaf = &document["sites"][0]["facts"]["crawl_issues_total"];
    assert_eq!(leaf["state"], json!("absent"));
    assert_eq!(leaf["value"], json!(0));
}

#[test]
fn index_counts_are_permanently_not_requested() {
    let document = build_facts_export(&[json!({"label": "a-example.com"})]).expect("export");
    let facts_row = &document["sites"][0]["facts"];
    for key in ["index_count_google", "index_count_yandex"] {
        let leaf = &facts_row[key];
        assert_eq!(leaf["state"], json!("not_requested"), "{key}");
        assert!(leaf["value"].is_null(), "{key}");
        assert_eq!(leaf["paid"], json!(false), "{key}");
        assert!(leaf["reason"]
            .as_str()
            .expect("reason")
            .contains("no configured provider returns a result count"));
    }
}

#[test]
fn site_with_no_documents_gets_a_full_unavailable_row_and_is_named() {
    let document = build_facts_export(&[
        json!({"label": "a-example.com", "crawl_audit": sf_document_a()}),
        json!({"label": "empty-example.com"}),
    ])
    .expect("export");
    let sites = document["sites"].as_array().expect("sites");
    let empty_row = sites
        .iter()
        .find(|s| s["label"] == json!("empty-example.com"))
        .expect("empty site is never dropped silently");
    assert!(empty_row["note"]
        .as_str()
        .expect("note")
        .contains("no readable documents"));
    let non_index: Vec<_> = empty_row["facts"]
        .as_object()
        .expect("facts")
        .iter()
        .filter(|(k, _)| !k.starts_with("index_count_"))
        .collect();
    assert!(!non_index.is_empty());
    for (key, leaf) in non_index {
        assert_eq!(leaf["state"], json!("unavailable"), "{key}");
        assert!(leaf["value"].is_null(), "{key}");
    }
}

#[test]
fn mismatched_domain_is_refused_naming_file_label_and_actual_domain() {
    let mut mismatched = sf_document_a();
    mismatched["run"]["source"] = json!("https://other.com/");
    let err = build_facts_export(&[json!({"label": "a-example.com", "crawl_audit": mismatched})])
        .expect_err("mismatch refused");
    let FactError(message) = err;
    assert!(message.contains("a-example.com"), "{message}");
    assert!(message.contains("other.com"), "{message}");
}

#[test]
fn matching_domain_is_accepted() {
    let document = build_facts_export(&[json!({
        "label": "a-example.com",
        "crawl_audit": sf_document_a(),
        "site_audit": site_document_a(),
    })])
    .expect("export");
    assert_eq!(document["sites"][0]["domain"], json!("a-example.com"));
}

#[test]
fn site_audit_domain_mismatch_is_also_refused() {
    let mut mismatched = site_document_a();
    mismatched["domain"] = json!("other.com");
    let err = build_facts_export(&[json!({"label": "a-example.com", "site_audit": mismatched})])
        .expect_err("mismatch refused");
    assert!(err.0.contains("other.com"), "{}", err.0);
}

#[test]
fn same_exact_domain_under_two_labels_is_refused_before_loading_anything() {
    // `json!(42)` would fail `_load` if ever read: the duplicate-label check
    // must fire first.
    let err = build_facts_export(&[
        json!({"label": "a-example.com"}),
        json!({"label": "a-example.com", "crawl_audit": 42}),
    ])
    .expect_err("duplicate refused");
    assert!(err.0.contains("a-example.com"), "{}", err.0);
}

#[test]
fn two_subdomains_of_one_registrable_domain_are_allowed_and_noted() {
    let document = build_facts_export(&[
        json!({"label": "blog.a-example.com"}),
        json!({"label": "shop.a-example.com"}),
    ])
    .expect("export");
    let sites = document["sites"].as_array().expect("sites");
    let labels: Vec<&str> = sites
        .iter()
        .map(|s| s["label"].as_str().expect("label"))
        .collect();
    assert_eq!(labels, ["blog.a-example.com", "shop.a-example.com"]);
    for site in sites {
        let note = site["note"].as_str().expect("note");
        assert!(note.contains("a-example.com"), "{note}");
        assert!(note.contains("shares registrable domain"), "{note}");
    }
}

#[test]
fn export_contains_no_percentage_or_ratio_keys() {
    let document = build_facts_export(&[json!({
        "label": "a-example.com",
        "crawl_audit": sf_document_a(),
        "site_audit": site_document_a(),
    })])
    .expect("export");
    for key in document["sites"][0]["facts"]
        .as_object()
        .expect("facts")
        .keys()
    {
        let lowered = key.to_lowercase();
        for needle in ["ratio", "percent", "score", "rank"] {
            assert!(!lowered.contains(needle), "{key} contains {needle}");
        }
    }
}

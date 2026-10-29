//! command-line surface (`--set` assignments, header mappings, nested and dotted overrides,
//! the rate-warning text).
//!
//! The reference tests build their overrides through the CLI parser (`cli._build_kwargs`, the
//! `--max-urls-per-second` flag, `cli._print_effective_rate`). Those CLI halves are not ported
//! here; the settings halves use the same `--set` text through `parse_setting_assignment`.
//! Nothing here touches the network or the real environment.

use serde_json::{json, Map, Value};
use spiderhead_core::crawl_settings::{
    delay_for_request_rate, load, parse_setting_assignment, rate_warnings, ConfigError, LoadInput,
};
use spiderhead_core::host::FakeHost;
use spiderhead_core::settings_data;

/// Load with explicit dotted overrides only, like reference `load(overrides=...)`.
fn load_with(pairs: &[(&str, Value)]) -> Result<Value, ConfigError> {
    let overrides: Map<String, Value> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect();
    load(
        &LoadInput {
            overrides,
            ..LoadInput::default()
        },
        &FakeHost::default(),
    )
}

/// Parse `--set` style assignments into the overrides map the crawl would receive.
fn set_overrides(assignments: &[&str]) -> Map<String, Value> {
    assignments
        .iter()
        .map(|text| parse_setting_assignment(text).expect("valid --set text"))
        .collect()
}

// (settings half; the CLI half runs `--set` through the crawl-site parser and is deferred)
#[test]
fn set_reaches_a_setting_that_has_no_flag_of_its_own() {
    let overrides = set_overrides(&["speed.concurrency=4"]);
    assert_eq!(overrides["speed.concurrency"], json!(4));
    let resolved = load_with(&[("speed.concurrency", json!(4))]).unwrap();
    assert_eq!(resolved["speed"]["concurrency"], 4);
}

// (settings half; the CLI half is deferred)
#[test]
fn set_reaches_the_rendering_browser_settings() {
    let overrides = set_overrides(&[
        "rendering.browser.engine=webkit",
        "rendering.browser.viewport_width=800",
        "rendering.browser.viewport_height=600",
        "rendering.browser.page_concurrency=3",
    ]);
    let pairs: Vec<(&str, Value)> = overrides
        .iter()
        .map(|(k, v)| (k.as_str(), v.clone()))
        .collect();
    let resolved = load_with(&pairs).unwrap();
    let browser = &resolved["rendering"]["browser"];
    assert_eq!(browser["engine"], "webkit");
    assert_eq!(browser["viewport_width"], 800);
    assert_eq!(browser["viewport_height"], 600);
    assert_eq!(browser["page_concurrency"], 3);
}

#[test]
fn a_value_gets_the_type_its_default_has() {
    let cases: [(&str, Value); 8] = [
        ("speed.concurrency=4", json!(4)),
        ("speed.min_delay_seconds=0.25", json!(0.25)),
        ("sitemaps.auto_discover=true", json!(true)),
        ("sitemaps.auto_discover=no", json!(false)),
        (
            "scope.include_patterns=/blog/,/docs/",
            json!(["/blog/", "/docs/"]),
        ),
        (
            "scope.include_extensions=.html,.pdf",
            json!([".html", ".pdf"]),
        ),
        (
            "scope.include_media_types=text/html,image/*",
            json!(["text/html", "image/*"]),
        ),
        ("http.user_agent=Example/1.0", json!("Example/1.0")),
    ];
    for (assignment, expected) in cases {
        let (_, value) = parse_setting_assignment(assignment).expect(assignment);
        assert_eq!(value, expected, "{assignment}");
    }
}

#[test]
fn an_empty_list_value_is_an_empty_list_not_one_empty_pattern() {
    let (_, value) = parse_setting_assignment("scope.include_patterns=").unwrap();
    assert_eq!(value, json!([]));
}

#[test]
fn set_refuses_an_unrecognized_boolean_instead_of_turning_it_off() {
    for raw in ["treu", "enable", "", "2"] {
        let err =
            parse_setting_assignment(&format!("sitemaps.auto_discover={raw}")).expect_err(raw);
        assert!(
            err.to_string().contains("sitemaps.auto_discover"),
            "{raw}: {err}"
        );
    }
}

// (settings half; the CLI half is deferred)
#[test]
fn set_parses_header_mapping_before_the_crawl_starts() {
    let overrides = set_overrides(&[r#"http.headers={"X-Audit":"seohead"}"#]);
    assert_eq!(overrides["http.headers"], json!({"X-Audit": "seohead"}));
    let resolved = load_with(&[("http.headers", json!({"X-Audit": "seohead"}))]).unwrap();
    assert_eq!(resolved["http"]["headers"], json!({"X-Audit": "seohead"}));
}

#[test]
fn set_refuses_invalid_header_mapping_before_the_crawl_starts() {
    for raw in ["X-Audit:seohead", "[]", r#"{"X-Audit": 1}"#] {
        let err = parse_setting_assignment(&format!("http.headers={raw}")).expect_err(raw);
        assert!(err.to_string().contains("http.headers"), "{raw}: {err}");
    }
}

#[test]
fn load_refuses_wrongly_typed_headers_before_the_crawl_starts() {
    let err = load_with(&[("http.headers", json!("X-Audit:seohead"))]).expect_err("typed");
    assert!(err.to_string().contains("http.headers"), "{err}");
}

#[test]
fn a_misspelled_path_names_the_setting_the_operator_meant() {
    let err = parse_setting_assignment("concurrency=4").expect_err("misspelled");
    assert!(err.to_string().contains("speed.concurrency"), "{err}");
}

#[test]
fn malformed_assignments_are_refused() {
    for bad in ["speed.concurrency", "=4", "speed.nope=1"] {
        assert!(parse_setting_assignment(bad).is_err(), "{bad}");
    }
}

#[test]
fn a_rate_of_zero_or_less_is_refused_rather_than_meaning_unbounded() {
    for rate in [0.0_f64, -1.0] {
        assert!(delay_for_request_rate(rate).is_err(), "{rate}");
    }
}

#[test]
fn a_nested_mapping_override_is_refused_by_name() {
    let err = load_with(&[("limits", json!({"max_urls": 50}))]).expect_err("nested");
    assert!(err.to_string().contains("limits.max_urls"), "{err}");
}

#[test]
fn a_dotted_override_leaves_its_siblings_alone() {
    let resolved = load_with(&[("limits.max_urls", json!(50))]).unwrap();
    let limits = resolved["limits"].as_object().expect("limits object");
    assert_eq!(limits["max_urls"], 50);
    let defaults = settings_data::defaults()["limits"]
        .as_object()
        .expect("default limits object");
    let mut keys: Vec<&String> = limits.keys().collect();
    let mut default_keys: Vec<&String> = defaults.keys().collect();
    keys.sort();
    default_keys.sort();
    assert_eq!(keys, default_keys);
}

// (warning text only; the stderr routing is the CLI half and is deferred)
#[test]
fn a_rate_above_two_prints_a_warning() {
    // 4 requests per second is a 0.25 s minimum delay.
    let resolved = load_with(&[("speed.min_delay_seconds", json!(0.25))]).unwrap();
    let warnings = rate_warnings(&resolved);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].contains("effective rate 4.00 req/s is above 2 req/s"),
        "{warnings:?}"
    );
}

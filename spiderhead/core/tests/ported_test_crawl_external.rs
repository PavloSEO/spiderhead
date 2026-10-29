//! Ports of the settings-wiring tests in the reference suite (the opt-in external
//! destination check: defaults, refusals and results-affecting classification). The crawl,
//! fetch and handler tests in that file are deferred; see the slice report.

use serde_json::{json, Map, Value};
use spiderhead_core::crawl_settings::{load, LoadInput};
use spiderhead_core::host::FakeHost;
use spiderhead_core::settings_data::RESULTS_AFFECTING;

fn load_with(pairs: &[(&str, Value)]) -> Result<Value, String> {
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
    .map_err(|e| e.to_string())
}

fn ok(pairs: &[(&str, Value)]) -> Value {
    load_with(pairs).expect("settings should load")
}

#[test]
fn discovery_external_crawl_defaults_off() {
    assert_eq!(ok(&[])["discovery"]["external"]["crawl"], json!(false));
}

#[test]
fn external_crawl_requires_stored_edges() {
    // load() validates: the refusal happens while the config is being built.
    let err = load_with(&[
        ("discovery.external.store", json!(false)),
        ("discovery.external.crawl", json!(true)),
    ])
    .expect_err("crawl without store must be refused");
    assert!(err.contains("store"), "{err}");
}

#[test]
fn external_checks_budgets_must_be_nonnegative_integers() {
    for name in [
        "max_targets",
        "max_hosts",
        "max_requests",
        "max_depth",
        "max_redirects",
    ] {
        let path = format!("external_checks.{name}");
        let err =
            load_with(&[(path.as_str(), json!(-1))]).expect_err("negative budget must be refused");
        assert!(err.contains(&path), "{name}: {err}");
    }
}

#[test]
fn external_checks_max_redirects_has_the_chain_ceiling() {
    let err = load_with(&[("external_checks.max_redirects", json!(11))])
        .expect_err("above the chain ceiling must be refused");
    assert!(err.contains("external_checks.max_redirects"), "{err}");
    assert_eq!(
        ok(&[("external_checks.max_redirects", json!(10))])["external_checks"]["max_redirects"],
        json!(10)
    );
}

#[test]
fn external_settings_are_results_affecting() {
    for path in [
        "discovery.external.crawl",
        "external_checks.max_targets",
        "external_checks.max_hosts",
        "external_checks.max_requests",
        "external_checks.max_depth",
        "external_checks.max_redirects",
    ] {
        assert!(
            RESULTS_AFFECTING.contains(&path),
            "{path} not results-affecting"
        );
    }
}

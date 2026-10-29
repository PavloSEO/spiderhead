//! manifest tests. The fetch-loop and spider tests need the crawl/fetch crates, which are not
//! ported yet, so they are not here.

use serde_json::{json, Value};
use spiderhead_core::crawl_settings::{load, manifest, ConfigError, LoadInput};
use spiderhead_core::host::FakeHost;

/// Load the default settings with one explicit dotted override, like reference `load(overrides=...)`.
fn load_with(path: &str, value: Value) -> Result<Value, ConfigError> {
    let mut overrides = serde_json::Map::new();
    overrides.insert(path.to_owned(), value);
    load(
        &LoadInput {
            overrides,
            ..LoadInput::default()
        },
        &FakeHost::default(),
    )
}

#[test]
fn out_of_range_retry_values_are_refused() {
    let values = [json!(-1), json!(4), json!(2.5), json!(true), json!("2")];
    for value in values {
        let error = load_with("fetch.retry_server_errors", value.clone())
            .expect_err("value must be refused");
        assert!(
            error.0.contains("retry_server_errors"),
            "refusal for {value} must name the setting, got {:?}",
            error.0
        );
    }
}

#[test]
fn the_setting_is_results_affecting_and_reaches_the_manifest() {
    let config = load_with("fetch.retry_server_errors", json!(2)).expect("2 is in range");
    assert_eq!(config["fetch"]["retry_server_errors"], json!(2));
    let manifest = manifest(&config, &FakeHost::default()).expect("manifest");
    assert!(manifest.contains_key("fetch.retry_server_errors"));
}

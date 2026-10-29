//! filters. The crawl-to-scan tests need the crawl crate and are deferred (see the report).

use serde_json::{json, Map, Value};
use spiderhead_core::crawl_settings::{fingerprint, load, LoadInput};
use spiderhead_core::host::FakeHost;

/// Load defaults plus explicit dotted overrides, as the reference `load(overrides=...)` does.
fn load_with(pairs: &[(&str, Value)]) -> Value {
    let overrides: Map<String, Value> = pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect();
    load(
        &LoadInput {
            overrides,
            ..LoadInput::default()
        },
        &FakeHost::default(),
    )
    .expect("settings should load")
}

#[test]
fn changed_file_filters_change_the_native_resume_fingerprint() {
    let host = FakeHost::default();
    let first = load_with(&[("scope.exclude_extensions", json!(["pdf"]))]);
    let second = load_with(&[("scope.exclude_extensions", json!(["docx"]))]);
    let third = load_with(&[("scope.exclude_media_types", json!(["application/pdf"]))]);

    let first_fp = fingerprint(&first, &host).expect("fingerprint");
    assert_ne!(first_fp, fingerprint(&second, &host).expect("fingerprint"));
    assert_ne!(first_fp, fingerprint(&third, &host).expect("fingerprint"));
}

#[test]
fn new_filters_accept_existing_config_files_without_filter_keys() {
    let config = r#"{"scope": {"include_patterns": ["/blog/"]}}"#;
    let resolved = load(
        &LoadInput {
            file: Some(("old-crawl.json", config)),
            ..LoadInput::default()
        },
        &FakeHost::default(),
    )
    .expect("old config should load");

    assert_eq!(resolved["scope"]["include_extensions"], json!([]));
    assert_eq!(resolved["scope"]["exclude_extensions"], json!([]));
    assert_eq!(resolved["scope"]["include_media_types"], json!([]));
    assert_eq!(resolved["scope"]["exclude_media_types"], json!([]));
}

//! Ports of the configuration-side tests of the reference suite: the dotted
//! file-type filter overrides and their validation. The crawl-time tests (URL suffix scope,
//! response media scope, fetch, cache and resume) need the crawl crate and are deferred.

use serde_json::{json, Map, Value};
use spiderhead_core::crawl_settings::{load, ConfigError, LoadInput};
use spiderhead_core::host::FakeHost;

/// reference `load(overrides={path: value})` with the default environment and no config file.
fn load_override(path: &str, value: Value) -> Result<Value, ConfigError> {
    let mut overrides = Map::new();
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
fn file_filter_lists_load_from_dotted_overrides() {
    let cases = [
        ("scope.include_extensions", json!([".HTML", ".pdf"])),
        ("scope.include_media_types", json!(["text/html", "image/*"])),
    ];
    for (path, expected) in cases {
        let resolved = load_override(path, expected.clone()).expect("load");
        let (category, setting) = path.split_once('.').expect("dotted path");
        assert_eq!(resolved[category][setting], expected, "{path}");
    }
}

#[test]
fn file_filter_validation_names_invalid_paths() {
    // The two duplicate-suffix cases of this parametrized test are split out below.
    let cases: [(&str, Value, &str); 8] = [
        (
            "scope.include_extensions",
            json!("pdf"),
            "scope.include_extensions must be a list",
        ),
        (
            "scope.exclude_extensions",
            json!([1]),
            "scope.exclude_extensions entries must be strings",
        ),
        (
            "scope.include_extensions",
            json!(["..pdf"]),
            "scope.include_extensions entry",
        ),
        (
            "scope.exclude_extensions",
            json!(["tar.gz"]),
            "scope.exclude_extensions entry",
        ),
        (
            "scope.include_media_types",
            json!("text/html"),
            "scope.include_media_types must be a list",
        ),
        (
            "scope.exclude_media_types",
            json!(["text/html; charset=utf-8"]),
            "scope.exclude_media_types entry",
        ),
        (
            "scope.include_media_types",
            json!(["*/json"]),
            "scope.include_media_types entry",
        ),
        (
            "scope.include_media_types",
            json!(["text/"]),
            "scope.include_media_types entry",
        ),
    ];
    for (path, value, message) in cases {
        let err = load_override(path, value.clone()).expect_err(&format!("{path} {value}"));
        assert!(err.0.contains(message), "{path} {value}: {err}");
    }
}

#[test]
fn file_filter_validation_names_duplicate_suffix() {
    let err = load_override("scope.include_extensions", json!([".PDF", "pdf"])).expect_err("dup");
    assert!(err.0.contains("duplicate suffix 'pdf'"), "{err}");
}

#[test]
fn file_filter_validation_names_duplicate_media_type() {
    let err = load_override(
        "scope.exclude_media_types",
        json!(["TEXT/HTML", "text/html"]),
    )
    .expect_err("dup");
    assert!(err.0.contains("duplicate media type 'text/html'"), "{err}");
}

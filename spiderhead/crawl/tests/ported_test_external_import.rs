//! Offline contract tests for the third-party crawl CSV import, ported from
//! the reference suite. The handler/CLI/MCP contract test (reference tests 17–19)
//! lives with the mcp and cli surfaces.

use std::fs;
use std::path::{Path, PathBuf};

use spiderhead_crawl::external_import::{
    import_third_party_crawl, import_third_party_crawl_with_limits, ImportLimits, MANIFEST_VERSION,
};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures_third_party_crawl")
}

/// A writable copy of one fixture bundle; returns its manifest path.
fn copy_bundle(tmp: &Path, partial: bool, name: &str) -> PathBuf {
    let source = fixtures().join(if partial { "partial" } else { "full" });
    let target = tmp.join(name);
    fs::create_dir_all(&target).unwrap();
    for entry in fs::read_dir(&source).unwrap().filter_map(Result::ok) {
        fs::copy(entry.path(), target.join(entry.file_name())).unwrap();
    }
    target.join("manifest.json")
}

fn read_manifest(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

/// Re-serialize the manifest after mutating its parsed document.
fn rewrite_manifest(path: &Path, mutate: impl FnOnce(&mut serde_json::Value)) {
    let mut raw: serde_json::Value = serde_json::from_str(&read_manifest(path)).unwrap();
    mutate(&mut raw);
    fs::write(path, serde_json::to_string(&raw).unwrap()).unwrap();
}

fn err(
    import: Result<serde_json::Value, spiderhead_crawl::external_import::ExternalCrawlImportError>,
) -> String {
    import.unwrap_err().0
}

#[test]
fn complete_bundle_keeps_foreign_identity_and_counts_each_dataset() {
    let manifest = fixtures().join("full").join("manifest.json");
    let result = import_third_party_crawl(manifest.to_str().unwrap()).unwrap();

    assert_eq!(result["schema_version"], "third_party_crawl.v1");
    assert_eq!(result["evidence_kind"], "third_party_crawl_export");
    assert_eq!(result["compatibility"]["scan_v1"], serde_json::json!(false));
    assert_eq!(
        result["compatibility"]["sf_audit_json"],
        serde_json::json!(false)
    );
    assert_eq!(result["source"]["product"], "FixtureSpider");
    assert_eq!(
        result["crawl_completeness"],
        serde_json::json!({
            "state": "source_claimed_complete",
            "verified_by_importer": false,
            "reason": "synthetic fixture source claim",
        })
    );
    assert_eq!(
        result["summary"]["rows_by_dataset"],
        serde_json::json!({"pages": 3, "links": 2, "statuses": 3, "redirects": 2})
    );
    assert_eq!(result["import_coverage"]["state"], "partial");
    assert!(!Path::new(result["provenance"]["manifest_file"].as_str().unwrap()).is_absolute());
    let reparsed: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&result).unwrap()).unwrap();
    assert_eq!(reparsed, result);
}

// (the None/int cases are type-system level in Rust and covered by the handler test)
#[test]
fn invalid_manifest_path_has_a_contract_error() {
    for invalid_path in ["", "   "] {
        let error = err(import_third_party_crawl(invalid_path));
        assert!(
            error.contains("manifest_path"),
            "case {invalid_path:?}: {error}"
        );
    }
}

#[test]
fn url_normalization_preserves_raw_urls_and_reports_collisions() {
    let manifest = fixtures().join("full").join("manifest.json");
    let result = import_third_party_crawl(manifest.to_str().unwrap()).unwrap();
    let pages = &result["datasets"]["pages"];

    assert_eq!(
        pages["records"][1]["url"],
        "https://EXAMPLE.test:443/products/"
    );
    assert_eq!(
        pages["records"][1]["url_key"],
        "https://example.test/products"
    );
    assert_eq!(
        pages["records"][2]["url"],
        "https://example.test/products#top"
    );
    assert_eq!(
        pages["records"][2]["url_key"],
        pages["records"][1]["url_key"]
    );
    assert_eq!(
        pages["duplicate_summary"],
        serde_json::json!({"duplicate_groups": 1, "duplicate_rows": 1})
    );
    assert_eq!(pages["row_count"], serde_json::json!(3));
}

#[test]
fn link_status_and_redirect_duplicates_are_counted_but_never_removed() {
    let manifest = fixtures().join("full").join("manifest.json");
    let result = import_third_party_crawl(manifest.to_str().unwrap()).unwrap();

    for (name, rows) in [("links", 2), ("statuses", 3), ("redirects", 2)] {
        let dataset = &result["datasets"][name];
        assert_eq!(dataset["row_count"], serde_json::json!(rows), "{name}");
        assert_eq!(dataset["records"].as_array().unwrap().len(), rows, "{name}");
        assert_eq!(
            dataset["duplicate_summary"],
            serde_json::json!({"duplicate_groups": 1, "duplicate_rows": 1}),
            "{name}"
        );
    }
}

#[test]
fn complete_field_coverage_means_all_rows_in_export_have_valid_values() {
    let manifest = fixtures().join("full").join("manifest.json");
    let result = import_third_party_crawl(manifest.to_str().unwrap()).unwrap();
    let coverage = &result["datasets"]["pages"]["field_coverage"]["status_code"];

    assert_eq!(coverage["state"], "complete");
    assert!(
        coverage["scope"]
            .as_str()
            .unwrap()
            .starts_with("values in the supplied CSV rows only"),
        "scope: {}",
        coverage["scope"]
    );
    assert_eq!(coverage["rows"], coverage["present"]);
    assert_eq!(coverage["rows"], serde_json::json!(3));
    assert_eq!(coverage["missing"], serde_json::json!(0));
    assert_eq!(coverage["invalid"], serde_json::json!(0));
}

#[test]
fn partial_bundle_preserves_missing_columns_bad_values_and_dataset_gaps() {
    let manifest = fixtures().join("partial").join("manifest.json");
    let result = import_third_party_crawl(manifest.to_str().unwrap()).unwrap();
    let pages = &result["datasets"]["pages"];

    assert_eq!(
        result["crawl_completeness"]["state"],
        "source_claimed_partial"
    );
    assert_eq!(result["import_coverage"]["state"], "partial");
    assert_eq!(pages["records"][0]["url"], "/relative-path");
    assert_eq!(pages["records"][0]["url_key"], serde_json::Value::Null);
    assert_eq!(pages["records"][0]["field_errors"][0]["field"], "url");
    assert_eq!(pages["records"][1]["status_code"], serde_json::Value::Null);
    assert_eq!(pages["records"][1]["unparsed_values"]["status_code"], "999");
    assert_eq!(pages["field_coverage"]["title"]["state"], "unavailable");
    assert_eq!(
        pages["field_coverage"]["canonical_url"]["state"],
        "unavailable"
    );
    assert_eq!(pages["field_coverage"]["status_code"]["state"], "partial");

    let statuses = &result["datasets"]["statuses"];
    assert_eq!(statuses["state"], "unavailable");
    assert_eq!(
        statuses["field_coverage"]["status_code"]["state"],
        "not_exported"
    );
    assert!(
        statuses["reason"]
            .as_str()
            .unwrap()
            .contains("did not export"),
        "reason: {}",
        statuses["reason"]
    );

    let links = &result["datasets"]["links"];
    assert_eq!(links["row_count"], serde_json::json!(1));
    assert_eq!(
        links["records"][0]["destination_url"],
        serde_json::Value::Null
    );
    assert_eq!(
        links["field_coverage"]["destination_url"]["state"],
        "partial"
    );

    let redirects = &result["datasets"]["redirects"];
    assert_eq!(redirects["field_coverage"]["hop"]["state"], "unavailable");
}

#[test]
fn importer_never_returns_embedded_url_credentials() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = copy_bundle(tmp.path(), false, "bundle");
    let root = manifest.parent().unwrap();
    let pages = fs::read_to_string(root.join("pages.csv")).unwrap();
    fs::write(
        root.join("pages.csv"),
        pages.replacen(
            "https://example.test/",
            "https://user:secret@example.test/",
            1,
        ),
    )
    .unwrap();
    let result = import_third_party_crawl(manifest.to_str().unwrap()).unwrap();
    let record = &result["datasets"]["pages"]["records"][0];
    assert_eq!(record["url"], serde_json::Value::Null);
    assert_eq!(record["url_key"], serde_json::Value::Null);
    assert!(
        !serde_json::to_string(&result).unwrap().contains("secret"),
        "credentials leaked into the envelope"
    );
}

#[test]
fn unknown_schema_dataset_and_field_mappings_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = copy_bundle(tmp.path(), false, "bundle");

    rewrite_manifest(&manifest, |raw| {
        raw["schema_version"] = serde_json::json!("future.v9");
    });
    assert!(err(import_third_party_crawl(manifest.to_str().unwrap()))
        .contains("unsupported manifest schema_version"));

    rewrite_manifest(&manifest, |raw| {
        raw["schema_version"] = serde_json::json!(MANIFEST_VERSION);
        raw["datasets"]["unrecognized"] = serde_json::json!({"file": "pages.csv", "columns": {}});
    });
    assert!(err(import_third_party_crawl(manifest.to_str().unwrap())).contains("unknown dataset"));

    rewrite_manifest(&manifest, |raw| {
        if let Some(map) = raw["datasets"].as_object_mut() {
            map.remove("unrecognized");
        }
        raw["datasets"]["pages"]["columns"]["made_up"] = serde_json::json!("Title");
    });
    assert!(err(import_third_party_crawl(manifest.to_str().unwrap())).contains("unknown field"));

    rewrite_manifest(&manifest, |raw| {
        if let Some(map) = raw["datasets"]["pages"]["columns"].as_object_mut() {
            map.remove("made_up");
        }
        raw["source"]["crawl_state"] = serde_json::json!([]);
    });
    assert!(err(import_third_party_crawl(manifest.to_str().unwrap())).contains("crawl_state"));
}

#[test]
fn duplicate_manifest_keys_and_source_headers_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = copy_bundle(tmp.path(), false, "headers");
    fs::write(
        &manifest,
        r#"{"schema_version":"third_party_crawl_manifest.v1","schema_version":"third_party_crawl_manifest.v1"}"#,
    )
    .unwrap();
    assert!(
        err(import_third_party_crawl(manifest.to_str().unwrap())).contains("duplicate JSON key")
    );

    let manifest = copy_bundle(tmp.path(), false, "bundle");
    fs::write(
        manifest.parent().unwrap().join("pages.csv"),
        "Address,Address\nhttps://example.test/,https://example.test/\n",
    )
    .unwrap();
    assert!(err(import_third_party_crawl(manifest.to_str().unwrap())).contains("duplicate headers"));
}

#[test]
fn manifest_rejects_unpaired_unicode_surrogates() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = copy_bundle(tmp.path(), false, "bundle");
    // A lone surrogate cannot exist in a Rust string, so the escape is written
    // into the file bytes the way the reference json.dumps emits it.
    let text = read_manifest(&manifest).replace("FixtureSpider", "fixture-\\ud800");
    fs::write(&manifest, text).unwrap();
    assert!(err(import_third_party_crawl(manifest.to_str().unwrap())).contains("Unicode surrogate"));
}

#[test]
fn manifest_rejects_absolute_and_traversal_dataset_paths() {
    for bad_path in ["../outside.csv", "/tmp/outside.csv", "C:\\outside.csv"] {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = copy_bundle(tmp.path(), false, "bundle");
        rewrite_manifest(&manifest, |raw| {
            raw["datasets"]["pages"]["file"] = serde_json::json!(bad_path);
        });
        let error = err(import_third_party_crawl(manifest.to_str().unwrap()));
        assert!(
            error.contains("relative path inside"),
            "case {bad_path}: {error}"
        );
    }
}

#[test]
fn manifest_rejects_symlinked_dataset_file() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = copy_bundle(tmp.path(), false, "bundle");
    let root = manifest.parent().unwrap();
    std::os::unix::fs::symlink(root.join("pages.csv"), root.join("linked-pages.csv")).unwrap();
    rewrite_manifest(&manifest, |raw| {
        raw["datasets"]["pages"]["file"] = serde_json::json!("linked-pages.csv");
    });
    assert!(err(import_third_party_crawl(manifest.to_str().unwrap())).contains("symlink"));
}

#[test]
fn one_csv_cannot_be_reused_for_multiple_dataset_roles() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = copy_bundle(tmp.path(), false, "bundle");
    rewrite_manifest(&manifest, |raw| {
        raw["datasets"]["statuses"]["file"] = serde_json::json!("pages.csv");
        raw["datasets"]["statuses"]["columns"] = serde_json::json!({"url": "Address"});
    });
    assert!(err(import_third_party_crawl(manifest.to_str().unwrap()))
        .contains("assigned to more than one dataset"));
}

#[test]
fn oversized_source_file_is_refused_before_parsing() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = copy_bundle(tmp.path(), false, "bundle");
    let limits = ImportLimits {
        max_total_csv_bytes: 8,
        ..ImportLimits::default()
    };
    assert!(err(import_third_party_crawl_with_limits(
        manifest.to_str().unwrap(),
        &limits
    ))
    .contains("byte import budget"));
}

#[test]
fn row_limit_is_explicit_and_does_not_silently_truncate() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = copy_bundle(tmp.path(), false, "bundle");
    let limits = ImportLimits {
        max_rows_per_dataset: 1,
        ..ImportLimits::default()
    };
    assert!(err(import_third_party_crawl_with_limits(
        manifest.to_str().unwrap(),
        &limits
    ))
    .contains("row dataset limit"));
}

#[test]
fn input_files_remain_byte_identical_after_import() {
    let root = fixtures().join("full");
    let snapshot = || -> std::collections::BTreeMap<String, Vec<u8>> {
        fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                (name, fs::read(entry.path()).unwrap())
            })
            .collect()
    };
    let before = snapshot();
    import_third_party_crawl(root.join("manifest.json").to_str().unwrap()).unwrap();
    assert_eq!(snapshot(), before);
}

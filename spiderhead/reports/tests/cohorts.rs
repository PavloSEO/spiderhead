//! Offline contracts for publication and branded-search cohort projections.
//!
//! Rust equivalent yet (handlers live outside this crate).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use spiderhead_reports::cohorts::{gsc_progress, publication_cohorts};

fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("fixture json")
}

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cohorts-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Read a CSV file into row maps the way `csv.DictReader` yields them.
fn csv_rows(path: &Path) -> Vec<serde_json::Map<String, Value>> {
    let mut reader = csv::Reader::from_path(path).expect("csv");
    let headers = reader.headers().expect("headers").clone();
    reader
        .records()
        .map(|record| {
            let record = record.expect("record");
            headers
                .iter()
                .zip(record.iter())
                .map(|(k, v)| (k.to_owned(), Value::String(v.to_owned())))
                .collect()
        })
        .collect()
}

#[test]
fn publication_projection_preserves_dates_sources_and_measured_zero() {
    let mut document = fixture("publication_cohort_input.json");
    document["age_windows"] = json!([{"id": "first-month", "start_day": 0, "end_day": 30}]);
    document["as_of_date"] = json!("2026-02-28");
    let out = tmp("pub-dates").join("publication");
    let result =
        publication_cohorts(Some(&document), None, Some(out.to_str().unwrap())).expect("ok");
    assert_eq!(
        result["format"].as_str().unwrap(),
        "spiderhead.publication-cohort-result.v1"
    );
    let inventory = csv_rows(&out.join("publication_inventory.csv"));
    let uncertain = inventory
        .iter()
        .find(|row| row["content_id"] == "content-1")
        .expect("content-1");
    assert_eq!(uncertain["publication_raw"], "Spring 2026");
    assert_eq!(uncertain["publication_state"], "ambiguous");
    assert_eq!(uncertain["author_state"], "missing");
    let authors = csv_rows(&out.join("publication_authors.csv"));
    let names: BTreeSet<&str> = authors
        .iter()
        .map(|row| row["author"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(names, ["Ada", "Bea"].into_iter().collect());
    let observations = csv_rows(&out.join("provider_observations.csv"));
    let gsc_zero = observations
        .iter()
        .find(|row| row["provider"] == "gsc" && row["metric"] == "clicks")
        .expect("gsc clicks");
    assert_eq!(gsc_zero["metric_value"], "0.0");
    assert_eq!(gsc_zero["metric_state"], "measured");
    let providers: BTreeSet<&str> = observations
        .iter()
        .map(|row| row["provider"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(providers, ["gsc", "ga4"].into_iter().collect());
    let age_rows = csv_rows(&out.join("age_window_observations.csv"));
    assert!(!age_rows.is_empty());
    assert_eq!(age_rows[0]["history_state"], "partial");
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(out.join("manifest.json")).expect("manifest"),
    )
    .unwrap();
    assert!(manifest["policy"]["causality"]
        .as_str()
        .unwrap_or_default()
        .starts_with("publication timing"));
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn publication_projection_does_not_join_normalization_collisions() {
    let mut document = fixture("publication_cohort_input.json");
    let mut duplicate = document["content"][0].clone();
    duplicate["url"] = json!("https://EXAMPLE.test/guide/");
    document["content"].as_array_mut().unwrap().push(duplicate);
    let out = tmp("pub-collision").join("collision");
    publication_cohorts(Some(&document), None, Some(out.to_str().unwrap())).expect("ok");
    let observations = csv_rows(&out.join("provider_observations.csv"));
    let guide: Vec<_> = observations
        .iter()
        .filter(|row| row["url"] == "https://example.test/guide")
        .collect();
    assert!(!guide.is_empty());
    let states: BTreeSet<&str> = guide
        .iter()
        .map(|row| row["join_state"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(states, ["collision"].into_iter().collect());
    assert!(guide.iter().all(|row| row["content_ids_json"] == "[]"));
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn publication_projection_requires_a_new_output_directory() {
    let out = tmp("pub-exists");
    std::fs::create_dir_all(&out).unwrap();
    let err = publication_cohorts(
        Some(&fixture("publication_cohort_input.json")),
        None,
        Some(out.to_str().unwrap()),
    )
    .expect_err("existing dir refused");
    assert!(err.to_string().contains("must not already exist"), "{err}");
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn gsc_progress_preserves_classification_scope_and_weighted_ctr() {
    let out = tmp("gsc-ctr").join("gsc");
    let result = gsc_progress(
        Some(&fixture("gsc_progress_input.json")),
        None,
        Some(out.to_str().unwrap()),
    )
    .expect("ok");
    assert_eq!(
        result["format"].as_str().unwrap(),
        "spiderhead.gsc-progress-result.v1"
    );
    let contributions = csv_rows(&out.join("query_contributions.csv"));
    let classes: BTreeSet<&str> = contributions
        .iter()
        .map(|row| row["classification"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        classes,
        ["branded", "non_branded", "ambiguous"]
            .into_iter()
            .collect()
    );
    assert!(contributions
        .iter()
        .any(|row| row["clicks"] == "0.0" && row["clicks_state"] == "measured"));
    let summaries = csv_rows(&out.join("period_summaries.csv"));
    let non_brand = summaries
        .iter()
        .find(|row| row["classification"] == "non_branded")
        .expect("non_branded summary");
    assert_eq!(non_brand["ctr"], "0.2");
    assert_ne!(non_brand["scope_id"].as_str().unwrap_or_default(), "");
    let changes = csv_rows(&out.join("period_changes.csv"));
    assert!(changes
        .iter()
        .any(|row| row["percent_change_state"] == "new_from_zero"));
    let positions = csv_rows(&out.join("position_bands.csv"));
    assert!(positions.iter().any(|row| row["movement"] == "improved"));
    assert!(positions.iter().all(|row| row["interpretation"]
        .as_str()
        .unwrap_or_default()
        .contains("not a tracked rank")));
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(out.join("manifest.json")).expect("manifest"),
    )
    .unwrap();
    assert!(manifest["policy"]["absence"]
        .as_str()
        .unwrap_or_default()
        .contains("unknown"));
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn gsc_progress_refuses_non_gsc_evidence() {
    let mut document = fixture("gsc_progress_input.json");
    document["evidence"]["mapping"]["source"]["provider"] = json!("ga4");
    let out = tmp("gsc-non-gsc").join("gsc");
    let err = gsc_progress(Some(&document), None, Some(out.to_str().unwrap()))
        .expect_err("non-gsc refused");
    assert!(err.to_string().contains("only normalized GSC"), "{err}");
    let _ = std::fs::remove_dir_all(out.parent().unwrap());
}

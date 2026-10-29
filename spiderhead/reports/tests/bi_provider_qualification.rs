//! Ported tests for BI provider qualification (reference
//! the reference suite): unknown provider facts stay out
//! of measured quadrants.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use spiderhead_reports::bi::project::{export_bi, ExportLimits};
use spiderhead_reports::evidence_import::normalize_inline;

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bi-qual-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn sha256(data: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(data))
}

/// `evidence(provider, metric, value)` — one normalize_inline join document.
fn evidence(provider: &str, metric: &str, value: &Value) -> Value {
    normalize_inline(
        &json!([{ "url": "https://example.test/", metric: value }]),
        &json!({
            "format": "spiderhead.evidence-mapping.v1",
            "source": {
                "provider": provider,
                "operation": "synthetic",
                "reporting_identity": format!("synthetic-{provider}"),
                "privacy": "supplied",
                "timezone": "UTC",
                "attribution": "last-click",
                "search_engine": if provider == "gsc" { json!("google") } else { Value::Null },
                "search_type": if provider == "gsc" { json!("web") } else { Value::Null },
            },
            "url": {"field": "url", "kind": "absolute"},
            "row_shape": "flat",
            "dimensions": [],
            "metrics": [{"name": metric, "type": "number", "unit": "count"}],
            "period": {"start_date": "2026-01-01", "end_date": "2026-01-07"},
            "collection": {
                "state": "complete",
                "sampled": false,
                "thresholded": false,
                "truncated": false,
            },
        }),
    )
    .expect("normalize_inline")
}

type DatasetRows = std::collections::BTreeMap<String, Vec<Map<String, Value>>>;

fn dataset_rows(package: &Path, manifest: &Value) -> DatasetRows {
    let mut datasets = std::collections::BTreeMap::new();
    for (name, dataset) in manifest["datasets"].as_object().unwrap() {
        let mut rows = Vec::new();
        for part in dataset["partitions"].as_array().unwrap() {
            let path = package.join(part["path"].as_str().unwrap());
            let mut reader = csv::Reader::from_path(&path).unwrap();
            let headers: Vec<String> = reader
                .headers()
                .unwrap()
                .iter()
                .map(str::to_owned)
                .collect();
            for record in reader.records() {
                let record = record.unwrap();
                rows.push(
                    headers
                        .iter()
                        .cloned()
                        .zip(record.iter().map(|c| json!(c)))
                        .collect(),
                );
            }
        }
        assert_eq!(rows.len() as i64, dataset["row_count"].as_i64().unwrap());
        datasets.insert(name.clone(), rows);
    }
    datasets
}

/// `project(tmp_path, *documents)` — export and return the search-visibility
/// quadrant row, all dataset rows and the manifest.
fn project(tag: &str, documents: Vec<Value>) -> (Map<String, Value>, DatasetRows, Value) {
    let dir = tmp(tag);
    let mut paths = Vec::new();
    let mut hashes = Vec::new();
    for (index, document) in documents.iter().enumerate() {
        let path = dir.join(format!("provider-{index}.json"));
        std::fs::write(&path, serde_json::to_string(document).unwrap()).unwrap();
        hashes.push(sha256(&std::fs::read(&path).unwrap()));
        paths.push(path.to_string_lossy().into_owned());
    }
    let audit = json!({
        "schema": "spiderhead.site-audit/1",
        "url": "https://example.test/",
        "site": {},
        "pages": [{"url": "https://example.test/", "status_code": 200}],
        "findings": [],
        "summary": {"pages_checked": 1, "findings_total": 0, "tools_run": [], "tools_failed": []},
    });
    let package = dir.join("package");
    export_bi(
        None,
        Some(&audit),
        &paths,
        Some(&package),
        ExportLimits {
            search_metric: Some("clicks".to_owned()),
            ..ExportLimits::default()
        },
    )
    .expect("export_bi");
    for (index, path) in paths.iter().enumerate() {
        assert_eq!(sha256(&std::fs::read(path).unwrap()), hashes[index]);
    }
    let manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(package.join("manifest.json")).unwrap())
            .unwrap();
    let datasets = dataset_rows(&package, &manifest);
    let quadrant = datasets
        .get("cohorts")
        .unwrap()
        .iter()
        .find(|row| row["cohort_id"] == "search_visibility_vs_sessions")
        .expect("search_visibility_vs_sessions row")
        .clone();
    (quadrant, datasets, manifest)
}

#[test]
fn explicit_measurements_keep_all_four_quadrants_and_axis_context() {
    for (index, (clicks, sessions, label)) in [
        (2, 3, "positive_search_positive_sessions"),
        (2, 0, "positive_search_zero_sessions"),
        (0, 3, "zero_search_positive_sessions"),
        (0, 0, "zero_search_zero_sessions"),
    ]
    .iter()
    .enumerate()
    {
        let gsc = evidence("gsc", "clicks", &json!(clicks));
        let ga4 = evidence("ga4", "sessions", &json!(sessions));
        let (row, datasets, manifest) = project(&format!("quadrant-{index}"), vec![gsc, ga4]);
        assert_eq!(
            (
                row["membership"].as_str().unwrap(),
                row["state"].as_str().unwrap(),
                row["value_label"].as_str().unwrap()
            ),
            ("member", "available", *label)
        );
        let axes: Vec<Value> =
            serde_json::from_str(row["source_metric_observations_json"].as_str().unwrap()).unwrap();
        for axis in &axes {
            let metric = datasets
                .get("metrics")
                .unwrap()
                .iter()
                .find(|item| item["provider"] == axis["provider"])
                .expect("metric row for axis provider");
            assert_eq!(axis["provider_source_id"], metric["provider_source_id"]);
            assert_eq!(axis["reporting_identity"], metric["reporting_identity"]);
            assert_eq!(axis["attribution"], "last-click");
            assert_eq!(axis["metric_unit"], "count");
            assert_eq!(axis["collection"]["state"], "complete");
            for flag in ["sampled", "thresholded", "truncated"] {
                assert_eq!(axis["collection"][flag], false, "{flag}");
            }
            assert_eq!(axis["boundary_policy"], "strict");
            assert_eq!(axis["cross_source_policy"], "juxtapose");
        }
        assert_eq!(axes[0]["search_engine"], "google");
        assert!(axes[1]["search_engine"].is_null());
        let states: std::collections::BTreeSet<&str> = manifest["provider_sources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|source| source["state"].as_str().unwrap())
            .collect();
        assert_eq!(states, ["complete"].into_iter().collect());
    }
}

#[test]
fn incomplete_or_incompatible_observations_do_not_enter_zero_quadrants() {
    let cases: [(&str, &str); 19] = [
        ("unknown_collection", "collection_state"),
        ("malformed_collection_origin", "collection_state"),
        ("unknown_flags", "explicitly"),
        ("unknown_flag_origin", "explicitly"),
        ("partial", "not complete"),
        ("failed", "not complete"),
        ("skipped", "not complete"),
        ("not_configured", "not complete"),
        ("sampled", "sampled"),
        ("thresholded", "thresholded"),
        ("truncated", "truncated"),
        ("missing_attribution", "unverified_scope"),
        ("unknown_attribution_origin", "unverified_scope"),
        ("negative_clicks", "nonnegative"),
        ("negative_sessions", "nonnegative"),
        ("null_clicks", "did not measure"),
        ("ratio_unit", "count units"),
        ("timezone_mismatch", "compatible"),
        ("period_mismatch", "compatible"),
    ];
    for (case, reason) in cases {
        let mut gsc = match case {
            "negative_clicks" => evidence("gsc", "clicks", &json!(-1)),
            "null_clicks" => evidence("gsc", "clicks", &Value::Null),
            _ => evidence("gsc", "clicks", &json!(0)),
        };
        let mut ga4 = if case == "negative_sessions" {
            evidence("ga4", "sessions", &json!(-1))
        } else {
            evidence("ga4", "sessions", &json!(2))
        };
        match case {
            "unknown_collection" => {
                gsc["provenance"]["fields"]["collection_state"] =
                    json!({"value": Value::Null, "origin": "unknown"});
            }
            "malformed_collection_origin" => {
                gsc["provenance"]["fields"]["collection_state"]["origin"] =
                    json!({"not": "a source attestation"});
            }
            "unknown_flags" => {
                for flag in ["sampled", "thresholded", "truncated"] {
                    gsc["mapping"]["collection"][flag] = Value::Null;
                    gsc["provenance"]["fields"][flag] =
                        json!({"value": Value::Null, "origin": "unknown"});
                }
            }
            "unknown_flag_origin" => {
                gsc["provenance"]["fields"]["sampled"]["origin"] = json!("unknown");
            }
            "partial" | "failed" | "skipped" | "not_configured" => {
                gsc["mapping"]["collection"]["state"] = json!(case);
            }
            "sampled" | "thresholded" | "truncated" => {
                gsc["mapping"]["collection"][case] = json!(true);
            }
            "missing_attribution" => {
                gsc["mapping"]["source"]["attribution"] = Value::Null;
                gsc["provenance"]["fields"]["attribution"] =
                    json!({"value": Value::Null, "origin": "unknown"});
            }
            "unknown_attribution_origin" => {
                gsc["provenance"]["fields"]["attribution"]["origin"] = json!("unknown");
            }
            "ratio_unit" => {
                gsc["mapping"]["metrics"][0]["unit"] = json!("ratio");
            }
            "timezone_mismatch" => {
                ga4["mapping"]["source"]["timezone"] = json!("Europe/Minsk");
            }
            "period_mismatch" => {
                ga4["mapping"]["period"]["end_date"] = json!("2026-01-08");
            }
            "negative_clicks" | "negative_sessions" | "null_clicks" => {}
            _ => unreachable!(),
        }
        let (row, datasets, manifest) = project(&format!("case-{case}"), vec![gsc, ga4]);
        assert_eq!(
            (
                row["membership"].as_str().unwrap(),
                row["state"].as_str().unwrap()
            ),
            ("unclassified", "incomplete"),
            "{case}"
        );
        assert!(
            row["reason"].as_str().unwrap().contains(reason),
            "{case}: reason {:?} lacks {reason:?}",
            row["reason"]
        );
        for key in ["value_label", "search_value", "sessions_value"] {
            assert_eq!(row[key].as_str().unwrap(), "", "{case} {key}");
        }
        assert_eq!(datasets.get("metrics").unwrap().len(), 2, "{case}");
        let collection_cases: Map<String, Value> = [
            ("unknown_collection", "unknown"),
            ("malformed_collection_origin", "unknown"),
            ("unknown_flags", "unknown"),
            ("unknown_flag_origin", "unknown"),
            ("sampled", "partial"),
            ("thresholded", "partial"),
            ("truncated", "partial"),
            ("partial", "partial"),
            ("failed", "failed"),
            ("skipped", "skipped"),
            ("not_configured", "not_configured"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), json!(v)))
        .collect();
        if let Some(expected_state) = collection_cases.get(case) {
            let metric = datasets
                .get("metrics")
                .unwrap()
                .iter()
                .find(|item| item["provider"] == "gsc")
                .expect("gsc metric row");
            assert_eq!(metric["collection_state"], *expected_state, "{case}");
            if !["partial", "failed", "skipped", "not_configured"].contains(&case) {
                assert!(
                    metric["collection_reason"]
                        .as_str()
                        .unwrap()
                        .contains(reason),
                    "{case}: collection_reason"
                );
            }
            let source_id = metric["provider_source_id"].as_str().unwrap();
            let rows: Vec<&Map<String, Value>> = datasets
                .get("coverage")
                .unwrap()
                .iter()
                .filter(|item| item["evidence_source_id"] == source_id)
                .collect();
            assert!(!rows.is_empty(), "{case}");
            let states: std::collections::BTreeSet<&str> = rows
                .iter()
                .map(|item| item["state"].as_str().unwrap())
                .collect();
            assert_eq!(
                states,
                [expected_state.as_str().unwrap()].into_iter().collect(),
                "{case}"
            );
            if !["partial", "failed", "skipped", "not_configured"].contains(&case) {
                assert!(
                    rows.iter()
                        .all(|item| item["reason"].as_str().unwrap().contains(reason)),
                    "{case}: coverage reason"
                );
            }
            let source = manifest["provider_sources"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["provider_source_id"] == source_id)
                .expect("provider source");
            assert_eq!(source["state"], *expected_state, "{case}");
            assert_eq!(
                source["evidence"]["collection"]["state"],
                gsc_source_collection_state(case),
                "{case}"
            );
        }
    }
}

/// The gsc `mapping.collection.state` left by each mutation (for the
/// provider-source evidence assertion).
fn gsc_source_collection_state(case: &str) -> Value {
    match case {
        "partial" | "failed" | "skipped" | "not_configured" => json!(case),
        _ => json!("complete"),
    }
}

#[test]
fn declared_cross_source_differences_are_retained_on_each_axis() {
    let gsc = evidence("gsc", "clicks", &json!(0));
    let mut ga4 = evidence("ga4", "sessions", &json!(2));
    ga4["mapping"]["source"]["attribution"] = json!("data-driven");
    ga4["mapping"]["source"]["search_engine"] = json!("bing");
    ga4["provenance"]["fields"]["search_engine"] = json!({"value": "bing", "origin": "declared"});
    let (row, _datasets, _manifest) = project("cross-source", vec![gsc, ga4]);
    assert_eq!(row["membership"], "member");
    let axes: Vec<Value> =
        serde_json::from_str(row["source_metric_observations_json"].as_str().unwrap()).unwrap();
    let attributions: Vec<&str> = axes
        .iter()
        .map(|axis| axis["attribution"].as_str().unwrap())
        .collect();
    assert_eq!(attributions, ["last-click", "data-driven"]);
    let engines: Vec<&str> = axes
        .iter()
        .map(|axis| axis["search_engine"].as_str().unwrap())
        .collect();
    assert_eq!(engines, ["google", "bing"]);
}

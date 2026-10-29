//! Differential tests for `reporting_pack` (and the `evidence_import` port it
//! depends on) against the reference baseline fixtures.
//!
//! Covers the reference suite: provider-input normalization,
//! worksheet packaging, and the boundary errors.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use spiderhead_reports::reporting_pack::{build_worksheets, load_provider_input};

fn fixtures() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reports_oracle.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture file"))
        .expect("fixture json")
}

fn pack() -> Value {
    fixtures()
        .get("reporting_pack")
        .cloned()
        .expect("reporting_pack fixtures")
}

fn assert_json_eq(context: &str, expected: &Value, actual: &Value) {
    assert_eq!(
        expected,
        actual,
        "{context}\nEXPECTED: {}\nACTUAL: {}",
        serde_json::to_string_pretty(expected).unwrap_or_default(),
        serde_json::to_string_pretty(actual).unwrap_or_default()
    );
}

fn write_tmp(dir: &Path, name: &str, data: &Value) -> PathBuf {
    let path = dir.join(name);
    // reference gen wrote `json.dumps(data, ensure_ascii=False)` — default
    // separators (", " / ": "), matching evidence_import::dumps_spaced.
    std::fs::write(
        &path,
        spiderhead_reports::evidence_import::dumps_spaced(data),
    )
    .unwrap();
    path
}

// byte-identically through `load_provider_input`.
#[test]
fn provider_input_parity() {
    let sources = pack().get("sources").cloned().unwrap_or(json!({}));
    let dir = tempfile_dir();
    for (name, row) in sources.as_object().unwrap() {
        let path = write_tmp(&dir, &format!("{name}.json"), &row["input"]);
        let result = load_provider_input(&path);
        if let Some(expected) = row.get("document") {
            let actual = result.unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_json_eq(&format!("{name} normalized"), expected, &actual);
        } else {
            let want = row.get("error").and_then(Value::as_str).unwrap_or_default();
            let got = result.err().map(|e| e.0).unwrap_or_default();
            assert_eq!(want, got, "{name}: error message");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// — result dict, manifest, and emitted CSV partitions match.
#[test]
fn build_worksheets_parity() {
    let cases = pack();
    let sources = cases.get("sources").cloned().unwrap_or(json!({}));
    let order = cases
        .get("provider_order")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let dir = tempfile_dir();
    let audit_path = write_tmp(&dir, "audit.json", &cases["audit"]);
    let paths: Vec<PathBuf> = order
        .iter()
        .map(|name| {
            let key = name.as_str().unwrap();
            write_tmp(&dir, &format!("{key}.json"), &sources[key]["input"])
        })
        .collect();
    for (name, row) in cases["worksheets"].as_object().unwrap() {
        let metric = match name.as_str() {
            "clicks" => Some("clicks"),
            _ => None,
        };
        let out_dir = dir.join(format!("ws-{name}"));
        let result = build_worksheets(&audit_path, &paths, metric, &out_dir);
        if let Some(error) = row.get("error") {
            let got = result.err().map(|e| e.0).unwrap_or_default();
            assert_eq!(error.as_str().unwrap(), got, "{name}: error");
            continue;
        }
        let mut result = result.unwrap_or_else(|e| panic!("{name}: {e}"));
        result.as_object_mut().unwrap().remove("output_directory");
        assert_json_eq(&format!("{name} result"), &row["result"], &result);
        for (fname, expected) in row["files"].as_object().unwrap() {
            let emitted = out_dir.join(fname);
            if fname == "manifest.json" {
                let actual: Value = serde_json::from_str(
                    &std::fs::read_to_string(&emitted).expect("manifest exists"),
                )
                .unwrap();
                assert_json_eq(&format!("{name}:{fname}"), expected, &actual);
            } else {
                let actual = std::fs::read(&emitted).expect("partition exists");
                let actual = String::from_utf8_lossy(&actual).to_string();
                let expected_text = expected.as_str().unwrap_or_default();
                assert_eq!(
                    actual,
                    expected_text.trim_start_matches('\u{feff}'),
                    "{name}:{fname} content"
                );
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// Boundary errors from the reference suite.
#[test]
fn boundary_errors_parity() {
    let cases = pack();
    let errors = cases.get("errors").cloned().unwrap_or(json!({}));
    let sources = cases.get("sources").cloned().unwrap_or(json!({}));
    let order = cases
        .get("provider_order")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let dir = tempfile_dir();
    let audit_path = write_tmp(&dir, "audit.json", &cases["audit"]);
    let paths: Vec<PathBuf> = order
        .iter()
        .map(|name| {
            write_tmp(
                &dir,
                &format!("{}.json", name.as_str().unwrap()),
                &sources[name.as_str().unwrap()]["input"],
            )
        })
        .collect();
    let existing = dir.join("exists");
    std::fs::create_dir(&existing).unwrap();
    let got = build_worksheets(&audit_path, &paths, None, &existing)
        .err()
        .map(|e| e.0)
        .unwrap_or_default();
    assert_eq!(
        errors["existing-out-dir"].as_str().unwrap_or_default(),
        got,
        "existing-out-dir"
    );
    let got = load_provider_input(&dir.join("nope.json"))
        .err()
        .map(|e| e.0)
        .unwrap_or_default();
    assert_eq!(
        errors["missing-source"].as_str().unwrap_or_default(),
        got,
        "missing-source"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn tempfile_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rp-test-{}-{}", std::process::id(), unique()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn unique() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

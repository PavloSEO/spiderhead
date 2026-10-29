//! Differential test against the reference baseline for the reference suite.
//!
//! Fixture: `tests/fixtures/loader.json` + real export dirs under
//! `tests/fixtures/loader/<case>/`, produced by the reference suite.

use serde_json::Value;
use spiderhead_sf_import::loader::{discover_exports, load_exports, LoadError, LoadedExports};
use spiderhead_sf_import::table::Table;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn fixture() -> Value {
    let text = include_str!("fixtures/loader.json");
    serde_json::from_str(text).expect("fixture parses")
}

fn base() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/loader")
}

/// Replace the fixture-dir absolute path with `<FIXTURES>`, matching the gen.
fn norm(s: &str) -> String {
    s.replace(base().to_str().expect("utf8 path"), "<FIXTURES>")
}

/// Error text comparison: strict equality, else (reference codec detail tails
/// differ) prefix equality through the last repr-quoted segment.
fn assert_err_eq(want: &str, got: &str, ctx: &str) {
    if want == got {
        return;
    }
    if let Some(p) = want.rfind("': ") {
        assert!(
            got.starts_with(&want[..p + 2]),
            "{ctx}: error diverged\nwant: {want}\n got: {got}"
        );
        return;
    }
    panic!("{ctx}: error diverged\nwant: {want}\n got: {got}");
}

fn sorted_map(m: &serde_json::Map<String, Value>) -> BTreeMap<String, Value> {
    m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

fn loaded_eq(got: &LoadedExports, want: &Value, ctx: &str) {
    let want_files: BTreeMap<String, String> = sorted_map(want["files"].as_object().expect("map"))
        .into_iter()
        .map(|(k, v)| (k, v.as_str().expect("str").to_owned()))
        .collect();
    let got_files: BTreeMap<String, String> = got
        .files
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                Path::new(v)
                    .file_name()
                    .expect("name")
                    .to_string_lossy()
                    .into_owned(),
            )
        })
        .collect();
    assert_eq!(got_files, want_files, "{ctx}: files");

    let got_enc: BTreeMap<_, _> = got.encodings.iter().collect();
    let want_enc = sorted_map(want["encodings"].as_object().expect("map"));
    assert_eq!(got_enc.len(), want_enc.len(), "{ctx}: encodings count");
    for (k, v) in &want_enc {
        assert_eq!(
            got_enc.get(k).map(|s| s.as_str()),
            v.as_str().map(Some).unwrap_or(None),
            "{ctx}: encodings[{k}]"
        );
    }

    let want_found: Vec<String> = want["found"]
        .as_array()
        .expect("list")
        .iter()
        .map(|v| v.as_str().expect("str").to_owned())
        .collect();
    assert_eq!(got.found, want_found, "{ctx}: found");

    let want_missing: Vec<String> = want["missing"]
        .as_array()
        .expect("list")
        .iter()
        .map(|v| v.as_str().expect("str").to_owned())
        .collect();
    assert_eq!(
        got.missing.len(),
        want_missing.len(),
        "{ctx}: missing count"
    );
    for (g, w) in got.missing.iter().zip(&want_missing) {
        assert_err_eq(w, &norm(g), &format!("{ctx}: missing entry"));
    }

    let got_der: BTreeMap<_, _> = got.derived_from.iter().collect();
    let want_der = sorted_map(want["derived_from"].as_object().expect("map"));
    assert_eq!(got_der.len(), want_der.len(), "{ctx}: derived_from count");
    for (k, v) in &want_der {
        assert_eq!(
            got_der.get(k).map(|s| s.as_str()),
            v.as_str().map(Some).unwrap_or(None),
            "{ctx}: derived_from[{k}]"
        );
    }

    let got_derr: BTreeMap<_, _> = got.derivation_errors.iter().collect();
    let want_derr = sorted_map(want["derivation_errors"].as_object().expect("map"));
    assert_eq!(
        got_derr.len(),
        want_derr.len(),
        "{ctx}: derivation_errors count"
    );
    for (k, v) in &want_derr {
        assert_eq!(
            got_derr.get(k).map(|s| s.as_str()),
            v.as_str().map(Some).unwrap_or(None),
            "{ctx}: derivation_errors[{k}]"
        );
    }

    let want_frames = want["frames"].as_object().expect("map");
    let got_keys: std::collections::BTreeSet<_> = got.frames.keys().collect();
    let want_keys: std::collections::BTreeSet<_> = want_frames.keys().collect();
    assert_eq!(got_keys, want_keys, "{ctx}: frame keys");
    for (k, fw) in want_frames {
        let got_t: &Table = got.get(k).expect("frame");
        let want_cols: Vec<String> = fw["columns"]
            .as_array()
            .expect("cols")
            .iter()
            .map(|c| c.as_str().expect("str").to_owned())
            .collect();
        assert_eq!(got_t.columns(), want_cols.as_slice(), "{ctx}: {k} columns");
        let want_rows = fw["rows"].as_array().expect("rows");
        assert_eq!(got_t.len(), want_rows.len(), "{ctx}: {k} row count");
        for (r, want_row) in want_rows.iter().enumerate() {
            for (c, want_cell) in want_row.as_array().expect("row").iter().enumerate() {
                let got_cell = got_t.cell(r, c);
                assert_eq!(got_cell, want_cell, "{ctx}: {k}[{r}][{c}]");
            }
        }
    }
}

#[test]
fn oracle_cases() {
    let fx = fixture();
    for case in fx["cases"].as_array().expect("cases") {
        let name = case["dir"].as_str().expect("name");
        let dir = base().join(name);
        let ctx = || format!("case {name}");

        match case.get("discovered") {
            Some(want) => {
                let got = discover_exports(&dir)
                    .unwrap_or_else(|e| panic!("{}: discover failed: {e}", ctx()));
                let got_map: BTreeMap<String, String> = got
                    .iter()
                    .map(|(k, p)| {
                        (
                            k.clone(),
                            p.file_name().expect("name").to_string_lossy().into_owned(),
                        )
                    })
                    .collect();
                let want_map: BTreeMap<String, String> = sorted_map(want.as_object().expect("map"))
                    .into_iter()
                    .map(|(k, v)| (k, v.as_str().expect("str").to_owned()))
                    .collect();
                assert_eq!(got_map, want_map, "{}: discovered", ctx());
            }
            None => {
                let err = discover_exports(&dir).expect_err("should fail");
                let want = norm(case["discovered_error"].as_str().expect("str"));
                let want_msg = want.split_once(": ").map(|x| x.1).unwrap_or(&want);
                assert_err_eq(want_msg, &norm(&err.to_string()), &ctx());
            }
        }

        match case.get("loaded") {
            Some(want) => {
                let got = load_exports(&dir, &["internal_all"], true)
                    .unwrap_or_else(|e| panic!("{}: load failed: {e}", ctx()));
                loaded_eq(&got, want, &ctx());
            }
            None => {
                let err = match load_exports(&dir, &["internal_all"], true) {
                    Ok(_) => panic!("{}: load should fail", ctx()),
                    Err(e) => e,
                };
                // `discover` may already have raised the same error upstream.
                let want = norm(case["loaded_error"].as_str().expect("str"));
                let want_msg = want.split_once(": ").map(|x| x.1).unwrap_or(&want);
                assert_err_eq(want_msg, &norm(&err.to_string()), &ctx());
            }
        }
    }
}

#[test]
fn read_error_variant_shape() {
    // The unreadable required export maps to the RequiredUnreadable variant.
    let dir = base().join("internal_bad");
    match load_exports(&dir, &["internal_all"], true) {
        Err(LoadError::RequiredUnreadable { key, path, .. }) => {
            assert_eq!(key, "'internal_all'");
            assert!(path.contains("internal_all.csv"));
        }
        other => panic!("expected RequiredUnreadable, got {other:?}"),
    }
    let dir = base().join("empty");
    match load_exports(&dir, &["internal_all"], true) {
        Err(LoadError::RequiredMissing { key, .. }) => assert_eq!(key, "'internal_all'"),
        other => panic!("expected RequiredMissing, got {other:?}"),
    }
}

//! Differential test of the URL query slice (`storage-pack-10`): the reference baseline's answers over
//! `oracle/raw-scans/basic`, captured by the reference suite, must be reproduced exactly.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;

use serde_json::{json, Map, Value};
use spiderhead_store::url_query::{
    export_scan_query, scan_url_counts, scan_url_query, ExportArgs, QueryArgs,
};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../")
}

fn scan() -> String {
    root()
        .join("oracle/raw-scans/basic/scan.sqlite")
        .to_string_lossy()
        .into_owned()
}

fn fixture() -> Value {
    let text =
        std::fs::read_to_string(root().join("spiderhead/store/tests/fixtures/pack10-basic.json"))
            .expect("fixture");
    serde_json::from_str(&text).expect("fixture json")
}

fn normalised(map: Map<String, Value>) -> Value {
    fn walk(value: Value) -> Value {
        match value {
            Value::Object(m) => Value::Object(
                m.into_iter()
                    .filter(|(k, _)| k != "elapsed_ms")
                    .map(|(k, v)| {
                        if k == "scan" {
                            (k, Value::String("<SCAN>".into()))
                        } else {
                            (k, walk(v))
                        }
                    })
                    .collect(),
            ),
            Value::Array(items) => Value::Array(items.into_iter().map(walk).collect()),
            other => other,
        }
    }
    walk(Value::Object(map))
}

/// A stub of another slice panicked: the case cannot run yet.
fn blocked(payload: &(dyn std::any::Any + Send)) -> bool {
    let text = payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default();
    text.contains("slice ")
}

// the reference suite, replayed against the oracle's answers on the raw basic scan.
#[test]
fn queries_match_the_reference_oracle() {
    let fixture = fixture();
    let mut failures = Vec::new();
    let mut skipped = Vec::new();
    for (i, case) in fixture["queries"].as_array().unwrap().iter().enumerate() {
        let kwargs = case["call"].as_object().unwrap();
        let args = QueryArgs::from_kwargs(kwargs);
        let outcome = catch_unwind(AssertUnwindSafe(|| scan_url_query(&scan(), &args)));
        match outcome {
            Ok(result) => {
                let actual = normalised(result);
                if actual != case["result"] {
                    failures.push(format!(
                        "#{i} {}\n  rust:   {actual}\n  python: {}",
                        case["call"], case["result"]
                    ));
                }
            }
            Err(p) if blocked(p.as_ref()) => skipped.push(format!("#{i} {}", case["call"])),
            Err(p) => std::panic::resume_unwind(p),
        }
    }
    eprintln!("blocked by other slices: {} case(s)", skipped.len());
    assert!(skipped.is_empty(), "blocked cases: {skipped:?}");
    for s in &skipped {
        eprintln!("  {s}");
    }
    assert!(
        failures.is_empty(),
        "{} divergence(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn counts_match_the_reference_oracle() {
    let fixture = fixture();
    for case in fixture["counts"].as_array().unwrap() {
        let actual = normalised(scan_url_counts(&scan(), &case["filter_sets"], 30.0));
        assert_eq!(actual, case["result"], "{}", case["filter_sets"]);
    }
}

#[test]
fn exports_match_the_reference_oracle() {
    let fixture = fixture();
    let dir = std::env::temp_dir().join(format!("sh-pack10-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (i, case) in fixture["exports"].as_array().unwrap().iter().enumerate() {
        let kwargs = case["call"].as_object().unwrap();
        let fmt = kwargs["fmt"].as_str().unwrap();
        let target = dir.join(format!("out{i}.{fmt}"));
        let args = ExportArgs::from_kwargs(kwargs);
        let mut result = normalised(export_scan_query(
            &scan(),
            &json!(target.to_string_lossy()),
            &args,
        ));
        if result.get("file").is_some() {
            result["file"] = json!("<OUT>");
        }
        assert_eq!(result, case["result"], "export #{i} {}", case["call"]);
        if let Some(text) = case.get("file").and_then(Value::as_str) {
            assert_eq!(
                std::fs::read_to_string(&target).unwrap(),
                text,
                "export #{i} bytes"
            );
        }
        if let Some(sheets) = case.get("sheets").and_then(Value::as_object) {
            use calamine::{open_workbook, Data, Reader, Xlsx};
            let mut book: Xlsx<_> = open_workbook(&target).unwrap();
            for (name, rows) in sheets {
                let range = book.worksheet_range(name).unwrap();
                let actual: Vec<Value> = range
                    .rows()
                    .map(|row| {
                        Value::Array(
                            row.iter()
                                .map(|c| match c {
                                    Data::Empty => Value::Null,
                                    Data::Int(n) => json!(n),
                                    Data::Float(f) if f.fract() == 0.0 => json!(*f as i64),
                                    Data::Float(f) => json!(f),
                                    Data::Bool(b) => json!(b),
                                    other => json!(other.to_string()),
                                })
                                .collect(),
                        )
                    })
                    .collect();
                assert_eq!(
                    Value::Array(actual),
                    rows.clone(),
                    "export #{i} sheet {name}"
                );
            }
        }
    }
    std::fs::remove_dir_all(&dir).ok();
}

// projection is never built and never loaded; a v2 round trip needs the scan writer (storage-scan_lease).
#[test]
fn projection_is_inert_on_scan_v1() {
    let fixture = fixture();
    assert_eq!(fixture["projection"]["built"], json!(false));
    let dir = std::env::temp_dir().join(format!("sh-pack10p-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let copy = dir.join("scan.sqlite");
    std::fs::copy(scan(), &copy).unwrap();
    let con = rusqlite::Connection::open(&copy).unwrap();
    con.execute_batch("BEGIN").unwrap();
    assert!(!spiderhead_store::url_query_projection::persist(&con, 8.0).unwrap());
    assert!(spiderhead_store::url_query_projection::load(&con)
        .unwrap()
        .is_none());
    con.execute_batch("COMMIT").unwrap();
    std::fs::remove_dir_all(&dir).ok();
}

//! Differential test of the scan export (`storage-scan_export`): files written by the reference
//! oracle over `oracle/raw-scans/basic` and a trimmed audit document must be reproduced exactly.
//! Cases that reach a stub of another slice (audit.v2 reader, capability contract) are reported
//! as blocked, not passed.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use spiderhead_store::scan_export::export_scan_data;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../")
}

fn fixtures() -> PathBuf {
    root().join("spiderhead/store/tests/fixtures")
}

fn blocked(payload: &(dyn std::any::Any + Send)) -> bool {
    let text = payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default();
    text.contains("slice ")
}

fn grid(path: &Path) -> Value {
    use calamine::{open_workbook, Data, Reader, Xlsx};
    let mut book: Xlsx<_> = open_workbook(path).unwrap();
    let mut out = serde_json::Map::new();
    for name in book.sheet_names() {
        let range = book.worksheet_range(&name).unwrap();
        let rows: Vec<Value> = range
            .rows()
            .map(|row| {
                Value::Array(
                    row.iter()
                        .map(|c| match c {
                            Data::Empty => Value::Null,
                            Data::Int(n) => json!(n),
                            Data::Float(f) if f.fract() == 0.0 && f.abs() < 1e15 => {
                                json!(*f as i64)
                            }
                            Data::Float(f) => json!(f),
                            Data::Bool(b) => json!(b),
                            other => json!(other.to_string()),
                        })
                        .collect(),
                )
            })
            .collect();
        out.insert(name, Value::Array(rows));
    }
    Value::Object(out)
}

/// Compare grids treating integral floats and ints alike (openpyxl writes ints, Excel stores doubles).
fn same_grid(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same_grid(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| same_grid(v, w)))
        }
        _ => a == b,
    }
}

// replayed against the oracle's files on the raw basic scan and a trimmed audit document.
#[test]
fn exports_match_the_reference_oracle() {
    let cases: Value = serde_json::from_str(
        &std::fs::read_to_string(fixtures().join("scan-export-basic.json")).unwrap(),
    )
    .unwrap();
    let scan = root()
        .join("oracle/raw-scans/basic/scan.sqlite")
        .canonicalize()
        .unwrap();
    let doc = fixtures().join("audit-basic.json").canonicalize().unwrap();
    let mut blocked_cases = Vec::new();
    let mut failures = Vec::new();
    for (i, case) in cases.as_array().unwrap().iter().enumerate() {
        let dir = std::env::temp_dir().join(format!("sh-scan-export-{}-{i}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let call = case["call"].as_object().unwrap();
        let fmt = call["fmt"].as_str().unwrap();
        let input = match case["input"].as_str().unwrap() {
            "scan" => scan.clone(),
            "doc" => doc.clone(),
            _ => dir.join("nope.sqlite"),
        };
        let target = dir.join(if fmt == "csv" {
            "export.csv".to_string()
        } else {
            format!("export.{fmt}")
        });
        let null = Value::Null;
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            export_scan_data(
                &json!(input.to_string_lossy()),
                Some(&target),
                fmt,
                call.get("records").unwrap_or(&null),
                call.get("fields").unwrap_or(&null),
            )
        }));
        let result = match outcome {
            Ok(r) => r,
            Err(p) if blocked(p.as_ref()) => {
                blocked_cases.push(format!("#{i} {}", case["call"]));
                std::fs::remove_dir_all(&dir).ok();
                continue;
            }
            Err(p) => std::panic::resume_unwind(p),
        };
        let normalise = |text: String| {
            text.replace(&*dir.to_string_lossy(), "<OUT>")
                .replace(&*input.to_string_lossy(), "<IN>")
        };
        let mut actual =
            serde_json::from_str::<Value>(&normalise(Value::Object(result).to_string())).unwrap();
        if actual["ok"] == json!(false) {
            actual["error"] = json!(normalise(
                actual["error"].as_str().unwrap_or("").to_string()
            ));
        }
        if actual != case["result"] {
            failures.push(format!(
                "#{i} result {}\n rust: {actual}\n py:   {}",
                case["call"], case["result"]
            ));
        }
        for (name, expected) in case["files"].as_object().unwrap() {
            let path = dir.join(name);
            if !path.exists() {
                failures.push(format!("#{i} missing file {name}"));
                continue;
            }
            if name.ends_with(".xlsx") {
                let got: Value = serde_json::from_str(&normalise(grid(&path).to_string())).unwrap();
                if !same_grid(&got, expected) {
                    let mut detail = String::new();
                    for (sheet, rows) in got.as_object().unwrap() {
                        let want = &expected[sheet];
                        for (r, row) in rows.as_array().unwrap().iter().enumerate() {
                            if !same_grid(row, &want[r]) {
                                detail = format!("{sheet} row {r}: rust {row} / py {}", want[r]);
                                break;
                            }
                        }
                    }
                    failures.push(format!("#{i} {name}: workbook differs: {detail}"));
                }
            } else {
                let text = normalise(std::fs::read_to_string(&path).unwrap());
                if Some(text.as_str()) != expected.as_str() {
                    let want = expected.as_str().unwrap();
                    let at = text
                        .bytes()
                        .zip(want.bytes())
                        .position(|(a, b)| a != b)
                        .unwrap_or(text.len().min(want.len()));
                    failures.push(format!(
                        "#{i} {name}: differs at byte {at}: rust {:?} / py {:?}",
                        &text[at.saturating_sub(40)..(at + 60).min(text.len())],
                        &want[at.saturating_sub(40)..(at + 60).min(want.len())]
                    ));
                }
            }
        }
        // No extra visible output beyond what reference wrote.
        let mut produced: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| !n.starts_with('.'))
            .collect();
        produced.sort();
        let mut wanted: Vec<String> = case["files"].as_object().unwrap().keys().cloned().collect();
        wanted.sort();
        if produced != wanted {
            failures.push(format!("#{i} files {produced:?} != {wanted:?}"));
        }
        if let (Ok(keep), 0) = (std::env::var("SH_KEEP_EXPORT"), i) {
            std::fs::copy(dir.join("export.json"), keep).unwrap();
        }
        std::fs::remove_dir_all(&dir).ok();
    }
    eprintln!("blocked by other slices: {} case(s)", blocked_cases.len());
    assert!(blocked_cases.is_empty(), "blocked cases: {blocked_cases:?}");
    for b in &blocked_cases {
        eprintln!("  {b}");
    }
    assert!(
        failures.is_empty(),
        "{} divergence(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

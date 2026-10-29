//! Differential test of the corpus-side readers (`storage-pack-03`): the reference baseline's answers
//! over `oracle/raw-scans/basic`, captured by the reference suite.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;

use serde_json::Value;
use spiderhead_store::directory_tree::{build_directory_tree, scan_directory_tree};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../")
}

fn fixture() -> Value {
    let text =
        std::fs::read_to_string(root().join("spiderhead/store/tests/fixtures/pack03-basic.json"))
            .unwrap();
    serde_json::from_str(&text).unwrap()
}

#[test]
fn scan_directory_tree_matches_the_reference_oracle() {
    let scan = root().join("oracle/raw-scans/basic/scan.sqlite");
    let mut blocked = 0;
    for case in fixture()["scan_tree"].as_array().unwrap() {
        let call = case["call"].as_object().unwrap();
        let get = |k: &str, d: i64| call.get(k).and_then(Value::as_i64).unwrap_or(d);
        let timeout = call
            .get("timeout_seconds")
            .and_then(Value::as_f64)
            .unwrap_or(15.0);
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            scan_directory_tree(
                &scan,
                get("max_depth", 3),
                get("max_children", 50),
                get("max_nodes", 10_000),
                get("max_bytes", 1_048_576),
                timeout,
            )
        }));
        let actual = match outcome {
            Ok(r) => r,
            Err(p) => {
                let text = p
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                // the light scan validator belongs to storage-scan_schema
                assert!(text.contains("slice "), "{text}");
                blocked += 1;
                continue;
            }
        };
        match (actual, case.get("error")) {
            (Err(e), Some(_)) => assert!(
                e.to_string().contains("invalid directory-tree budget"),
                "{e}"
            ),
            (Ok(mut got), None) => {
                // the scan path is not part of the answer; everything else must match exactly
                let expected = case["result"].clone();
                if let Some(v) = got.get_mut("validation") {
                    assert!(v == "light" || v == "full");
                }
                assert_eq!(Value::Object(got), expected, "{}", case["call"]);
            }
            (other, error) => panic!("{} -> {other:?} vs {error:?}", case["call"]),
        }
    }
    eprintln!("blocked by other slices: {blocked} case(s)");
    assert_eq!(blocked, 0, "every case must reach real code");
}

#[test]
fn build_directory_tree_matches_the_reference_oracle() {
    for case in fixture()["build_tree"].as_array().unwrap() {
        let urls: Vec<String> = case["urls"]
            .as_array()
            .unwrap()
            .iter()
            .map(|u| u.as_str().unwrap().to_string())
            .collect();
        let call = case["call"].as_object().unwrap();
        let depth = call.get("max_depth").and_then(Value::as_i64).unwrap_or(3);
        let children = call
            .get("max_children")
            .and_then(Value::as_i64)
            .unwrap_or(50);
        match (
            build_directory_tree(&urls, depth, children),
            case.get("error"),
        ) {
            (Ok(got), None) => assert_eq!(
                Value::Object(got),
                case["result"],
                "{} {}",
                case["urls"],
                case["call"]
            ),
            (Err(e), Some(msg)) => assert!(e.to_string().contains(msg.as_str().unwrap()), "{e}"),
            (other, error) => panic!("{other:?} vs {error:?}"),
        }
    }
}

fn blocked(payload: &(dyn std::any::Any + Send)) -> bool {
    let text = payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default();
    text.contains("slice ")
}

// against the oracle's verdicts on the raw scans and on mutated copies of the basic one.
#[test]
fn validate_corpus_matches_the_reference_oracle() {
    use rusqlite::{Connection, OpenFlags};
    use spiderhead_store::corpus_validation::validate_corpus;
    let dir = std::env::temp_dir().join(format!("sh-pack03-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut blocked_cases = 0;
    let mut checked = 0;
    for (i, case) in fixture()["validate"].as_array().unwrap().iter().enumerate() {
        if case["result"]["ok"].is_null() {
            continue; // the mutation itself was refused by a schema CHECK
        }
        let source = root()
            .join("oracle/raw-scans")
            .join(case["site"].as_str().unwrap())
            .join("scan.sqlite");
        let path = dir.join(format!("scan{i}.sqlite"));
        std::fs::copy(&source, &path).unwrap();
        if let Some(sql) = case["mutation"].as_str() {
            let con = Connection::open(&path).unwrap();
            con.execute_batch(sql).unwrap();
        }
        let con = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let (scan, retention): (
            serde_json::Map<String, Value>,
            serde_json::Map<String, Value>,
        ) = {
            let mut rows = con.prepare("SELECT * FROM scan WHERE singleton=1").unwrap();
            let names: Vec<String> = rows.column_names().iter().map(|s| s.to_string()).collect();
            let mut cursor = rows.query([]).unwrap();
            let row = cursor.next().unwrap().unwrap();
            let mut scan = serde_json::Map::new();
            for (n, name) in names.iter().enumerate() {
                let v = match row.get_ref(n).unwrap() {
                    rusqlite::types::ValueRef::Text(t) => {
                        Value::String(String::from_utf8_lossy(t).into_owned())
                    }
                    rusqlite::types::ValueRef::Integer(x) => Value::from(x),
                    _ => Value::Null,
                };
                scan.insert(name.clone(), v);
            }
            let retention = serde_json::from_str(scan["retention_json"].as_str().unwrap()).unwrap();
            (scan, retention)
        };
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            validate_corpus(&con, &scan, &retention, true)
        }));
        match outcome {
            Ok(Ok(())) => assert_eq!(
                case["result"]["ok"], true,
                "{} {}",
                case["site"], case["mutation"]
            ),
            Ok(Err(e)) => {
                let message = match e {
                    spiderhead_store::error::StoreError::ScanError(s) => s.message,
                    other => other.to_string(),
                };
                assert_eq!(
                    case["result"]["ok"], false,
                    "{} {}: unexpected {message}",
                    case["site"], case["mutation"]
                );
                assert_eq!(
                    Value::String(message),
                    case["result"]["error"],
                    "{}",
                    case["mutation"]
                );
            }
            Err(p) if blocked(p.as_ref()) => blocked_cases += 1,
            Err(p) => std::panic::resume_unwind(p),
        }
        checked += 1;
        drop(con);
        std::fs::remove_file(&path).ok();
    }
    eprintln!("validated {checked} case(s), blocked by other slices: {blocked_cases}");
    assert_eq!(blocked_cases, 0, "every case must reach real code");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn corpus_summaries_match_the_reference_oracle() {
    use rusqlite::{Connection, OpenFlags};
    use spiderhead_store::corpus::{corpus_summary, html_body_retention, rendered_body_retention};
    for (site, expected) in fixture()["summaries"].as_object().unwrap() {
        let path = root()
            .join("oracle/raw-scans")
            .join(site)
            .join("scan.sqlite");
        let con = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let retention: Value = {
            let text: String = con
                .query_row(
                    "SELECT retention_json FROM scan WHERE singleton=1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            serde_json::from_str(&text).unwrap()
        };
        assert_eq!(
            Value::Object(html_body_retention(&con).unwrap()),
            expected["html"],
            "{site} html"
        );
        assert_eq!(
            Value::Object(rendered_body_retention(&con).unwrap()),
            expected["rendered"],
            "{site} rendered"
        );
        assert_eq!(
            Value::Object(corpus_summary(&con, &retention).unwrap()),
            expected["summary"],
            "{site} summary"
        );
    }
}

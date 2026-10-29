//! Differential test of page commit and finalization on the real `NativeScan` writer
//! (`storage-scan_lease`): the reference suite replays the same operations with the reference
//! writer and stores every answer and the resulting tables.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use spiderhead_store::crawl_capture::CaptureEvent;
use spiderhead_store::error::StoreError;
use spiderhead_store::native_scan::NativeScan;
use spiderhead_store::scan_lease::{Lease, NativeScanLease, PageCommit};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn message(e: StoreError) -> String {
    match e {
        StoreError::ScanError(s) => s.message,
        StoreError::Invalid(m) => m,
        other => other.to_string(),
    }
}

fn answer<T>(result: Result<T, StoreError>, value: impl FnOnce(T) -> Value) -> Value {
    match result {
        Ok(v) => json!({"ok": true, "value": value(v)}),
        Err(e) => json!({"ok": false, "error": message(e)}),
    }
}

fn lease_json(l: &Lease) -> Value {
    json!({"url_id": l.url_id, "url": l.url, "depth": l.depth, "queue_ordinal": l.queue_ordinal})
}

fn maps(op: &Value, key: &str) -> Vec<Map<String, Value>> {
    op[key]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_object().cloned()).collect())
        .unwrap_or_default()
}

fn commit_of(op: &Value, lease: &Lease) -> PageCommit {
    PageCommit {
        lease: Some(lease.clone()),
        record: op["record"].as_object().cloned().unwrap_or_default(),
        links: maps(op, "links"),
        forms: maps(op, "forms"),
        decisions: maps(op, "decisions"),
        discovered: op["discovered"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|p| (p[0].as_str().unwrap().to_string(), p[1].as_i64().unwrap()))
                    .collect()
            })
            .unwrap_or_default(),
        query_reservations: op["query_reservations"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|p| {
                        let s = |i: usize| p[i].as_str().unwrap().to_string();
                        (s(0), s(1), s(2))
                    })
                    .collect()
            })
            .unwrap_or_default(),
        candidates: maps(op, "candidates"),
        partial_reasons: op["partial_reasons"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        runtime: op["runtime"].as_object().cloned(),
        context: maps(op, "context"),
        captures: op["captures"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|e| serde_json::from_value::<CaptureEvent>(e.clone()).unwrap())
                    .collect()
            })
            .unwrap_or_default(),
        ..Default::default()
    }
}

fn dump_tables(path: &Path) -> Value {
    let con = Connection::open(path).unwrap();
    let mut out = Map::new();
    for name in [
        "scan",
        "urls",
        "frontier",
        "decisions",
        "resume_state",
        "context_items",
        "pages",
        "links",
        "forms",
        "query_variants",
        "responses",
        "documents",
        "bodies",
    ] {
        let columns: Vec<String> = {
            let mut stmt = con
                .prepare(&format!("PRAGMA table_info(\"{name}\")"))
                .unwrap();
            let rows = stmt.query_map([], |r| r.get::<_, String>(1)).unwrap();
            rows.map(Result::unwrap).collect()
        };
        let mut stmt = con
            .prepare(&format!("SELECT * FROM \"{name}\" ORDER BY rowid"))
            .unwrap();
        let mut rows = stmt.query([]).unwrap();
        let mut dumped = Vec::new();
        while let Some(row) = rows.next().unwrap() {
            let cells: Vec<Value> = (0..columns.len())
                .map(|i| match row.get_ref(i).unwrap() {
                    _ if columns[i] == "finished_at" => Value::Null,
                    rusqlite::types::ValueRef::Null => Value::Null,
                    rusqlite::types::ValueRef::Integer(n) => json!(n),
                    rusqlite::types::ValueRef::Real(f) => json!(f),
                    rusqlite::types::ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                    rusqlite::types::ValueRef::Blob(b) => {
                        json!(b.iter().map(|x| format!("{x:02x}")).collect::<String>())
                    }
                })
                .collect();
            dumped.push(Value::Array(cells));
        }
        out.insert(
            name.to_string(),
            json!({"columns": columns, "rows": dumped}),
        );
    }
    Value::Object(out)
}

fn first_diff(a: &Value, b: &Value, path: &str) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for (k, v) in x {
                match y.get(k) {
                    None => return Some(format!("{path}/{k}: only in rust")),
                    Some(w) => {
                        if let Some(d) = first_diff(v, w, &format!("{path}/{k}")) {
                            return Some(d);
                        }
                    }
                }
            }
            y.keys()
                .find(|k| !x.contains_key(*k))
                .map(|k| format!("{path}/{k}: only in python"))
        }
        (Value::Array(x), Value::Array(y)) => {
            for (i, (v, w)) in x.iter().zip(y).enumerate() {
                if let Some(d) = first_diff(v, w, &format!("{path}[{i}]")) {
                    return Some(d);
                }
            }
            (x.len() != y.len()).then(|| format!("{path}: length {} vs {}", x.len(), y.len()))
        }
        (Value::Number(x), Value::Number(y)) if x.as_f64() == y.as_f64() => None,
        _ => (a != b).then(|| format!("{path}: rust {a} vs python {b}")),
    }
}

fn run(name: &str, scenario: &Value) -> (Vec<Value>, Value) {
    let dir = std::env::temp_dir().join(format!("sh-commit-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("native.sqlite");
    std::fs::copy(fixtures().join("commit-pre.sqlite"), &path).unwrap();
    let con = Connection::open(&path).unwrap();
    con.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
    con.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    let mut scan = NativeScan::new(&path, con, None).unwrap();
    let mut leases: Vec<Lease> = Vec::new();
    let mut answers = Vec::new();
    for op in scenario["ops"].as_array().unwrap() {
        let result = match op["op"].as_str().unwrap() {
            "seed" => {
                let url = op["url"].as_str().unwrap();
                let seed = json!({"requested_url": url, "frontier_url": url, "depth": 0, "reason": "", "source": "start", "reserve_query": false, "seed": false});
                answer(
                    scan.seed_frontier(&[seed.as_object().cloned().unwrap()]),
                    Value::Object,
                )
            }
            "enqueue" => {
                let urls: Vec<(String, i64)> = op["urls"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|u| (u[0].as_str().unwrap().to_string(), u[1].as_i64().unwrap()))
                    .collect();
                answer(scan.enqueue(&urls, "enqueue"), |l| {
                    Value::Array(l.iter().map(lease_json).collect())
                })
            }
            "claim" => {
                let got = scan.claim(op["n"].as_i64().unwrap());
                if let Ok(l) = &got {
                    leases.extend(l.clone());
                }
                answer(got, |l| Value::Array(l.iter().map(lease_json).collect()))
            }
            "commit" => {
                let lease = leases[op["lease"].as_u64().unwrap() as usize].clone();
                let commit = commit_of(op, &lease);
                answer(
                    scan.commit_page(&commit),
                    |r| json!({"evidence_revision": r.evidence_revision, "already_committed": r.already_committed}),
                )
            }
            "snapshot_state" => answer(
                scan.resume_snapshot(op["edges"].as_bool().unwrap()),
                Value::Object,
            ),
            "finish_without_audit" => answer(
                scan.finish_without_audit("capture_finished_no_audit", 10.0),
                |b| json!(b),
            ),
            "finish_reanalysis" => answer(scan.finish_reanalysis(), |b| json!(b)),
            "finish_capture" => answer(
                scan.finish_capture(op["reason"].as_str().unwrap(), 10.0),
                |b| json!(b),
            ),
            "snapshot_file" => {
                let dest = dir.join("snap.sqlite");
                let result = scan.snapshot(&dest, 1 << 30, 0, 60.0, None);
                answer(result, |p| {
                    let c = Connection::open(p).unwrap();
                    let pages: i64 = c
                        .query_row("SELECT COUNT(*) FROM pages", [], |r| r.get(0))
                        .unwrap();
                    let journal: String = c
                        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
                        .unwrap();
                    json!({"pages": pages, "journal": journal})
                })
            }
            other => panic!("unknown op {other}"),
        };
        answers.push(result);
    }
    scan.close();
    let tables = dump_tables(&path);
    std::fs::remove_dir_all(&dir).ok();
    (answers, tables)
}

// (commit_page atomicity, retries, ordering, capture retention, finalization, snapshot),
// replayed against the oracle.
#[test]
fn page_commit_and_finalization_match_the_reference_oracle() {
    let expected: Value = serde_json::from_str(
        &std::fs::read_to_string(fixtures().join("commit-expected.json")).unwrap(),
    )
    .unwrap();
    let mut failures = Vec::new();
    for name in ["a", "b", "c"] {
        let scenario = &expected[name];
        let (answers, tables) = run(name, scenario);
        let want = scenario["answers"].as_array().unwrap();
        for (i, (got, want)) in answers.iter().zip(want).enumerate() {
            // the finish stamp is the wall clock
            let mask = |v: &Value| {
                let mut v = v.clone();
                if let Some(slot) = v.pointer_mut("/value/scan/finished_at") {
                    *slot = Value::Null;
                }
                v
            };
            let (got, want) = (&mask(got), &mask(want));
            if let Some(d) = first_diff(got, want, "") {
                failures.push(format!("{name} op#{i} {}: {d}", scenario["ops"][i]["op"]));
            }
        }
        if let Some(d) = first_diff(&tables, &scenario["tables"], "") {
            failures.push(format!("{name} tables: {d}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

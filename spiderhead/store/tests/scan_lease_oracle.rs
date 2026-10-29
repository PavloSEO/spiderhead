//! Differential test of the frontier/runtime lanes of the native scan writer
//! (`storage-scan_lease`): the reference suite replays them with the reference `NativeScan` and
//! records every answer and the final tables. The Rust lanes run over a copy of the same freshly
//! created scan through a minimal writer host (no event sink, as in the reference run).

use std::path::PathBuf;

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use spiderhead_store::error::StoreError;
use spiderhead_store::scan_artifact::ScanError;
use spiderhead_store::scan_lease::{Lease, NativeScanLease, ScanWriterHost};

struct Host {
    con: Connection,
}

impl ScanWriterHost for Host {
    fn connection(&self) -> &Connection {
        &self.con
    }

    fn assert_mutable(&self) -> Result<(), StoreError> {
        let lifecycle: String =
            self.con
                .query_row("SELECT lifecycle FROM scan WHERE singleton=1", [], |r| {
                    r.get(0)
                })?;
        if lifecycle == "finished" || lifecycle == "failed" {
            return Err(ScanError {
                message: format!("native scan lifecycle '{lifecycle}' is immutable"),
            }
            .into());
        }
        Ok(())
    }

    fn begin(&self, _enforce_wal_bound: bool) -> Result<(), StoreError> {
        self.con
            .execute_batch("BEGIN IMMEDIATE; DELETE FROM audit")?;
        Ok(())
    }

    fn rollback(&self) {
        if !self.con.is_autocommit() {
            let _ = self.con.execute_batch("ROLLBACK");
        }
    }

    fn hit(&self, _name: &str) {}

    fn event(&self, _event_type: &str, _payload: Value) -> Result<(), StoreError> {
        Ok(())
    }

    fn event_coverage(&self) -> Result<(), StoreError> {
        Ok(())
    }
}

impl NativeScanLease for Host {}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn json_file(name: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(fixtures().join(name)).unwrap()).unwrap()
}

fn lease_json(l: &Lease) -> Value {
    json!({"url_id": l.url_id, "url": l.url, "depth": l.depth, "queue_ordinal": l.queue_ordinal})
}

fn runtime(over: &[(&str, Value)]) -> Map<String, Value> {
    let mut base = json!({
        "max_depth_reached": 1,
        "elapsed_seconds": 0.0,
        "circuit_timeout_streak": 0,
        "circuit_server_error_streak": 0,
        "crawl_delay_applied": null,
        "throttle": {"delay_seconds": 0.0, "concurrency": 1, "consecutive_ok": 0},
    })
    .as_object()
    .cloned()
    .unwrap();
    for (k, v) in over {
        base.insert(k.to_string(), v.clone());
    }
    base
}

fn seed(url: &str) -> Map<String, Value> {
    json!({"requested_url": url, "frontier_url": url, "depth": 0, "reason": "", "source": "start", "reserve_query": false, "seed": false})
        .as_object()
        .cloned()
        .unwrap()
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

fn dump_tables(path: &std::path::Path) -> Value {
    let con = Connection::open(path).unwrap();
    let mut out = Map::new();
    for name in [
        "scan",
        "urls",
        "frontier",
        "decisions",
        "resume_state",
        "context_items",
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
                    rusqlite::types::ValueRef::Null => Value::Null,
                    rusqlite::types::ValueRef::Integer(n) => json!(n),
                    rusqlite::types::ValueRef::Real(f) => json!(f),
                    rusqlite::types::ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                    rusqlite::types::ValueRef::Blob(_) => Value::Null,
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

// partial reasons, runtime state, request budget, interruption), replayed against the oracle.
#[test]
fn frontier_and_runtime_lanes_match_the_reference_oracle() {
    let expected = json_file("lease-expected.json");
    let dir = std::env::temp_dir().join(format!("sh-lease-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("native.sqlite");
    std::fs::copy(fixtures().join("lease-pre.sqlite"), &path).unwrap();
    let host = Host {
        con: Connection::open(&path).unwrap(),
    };
    host.con.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    let mut claimed: Vec<Lease> = Vec::new();
    let urls: Vec<(String, i64)> = (0..5)
        .map(|i| (format!("https://example.test/p{i}"), 1))
        .chain([
            ("https://example.test/private/a".to_string(), 1),
            ("https://example.test/private/b".to_string(), 2),
        ])
        .collect();
    let leases = |v: Vec<Lease>| Value::Array(v.iter().map(lease_json).collect());
    let mut actual: Vec<Value> = Vec::new();
    let mut step = |op: &str, value: Value| {
        let mut m = value.as_object().cloned().unwrap();
        m.insert("op".into(), json!(op));
        actual.push(Value::Object(m));
    };
    step(
        "snapshot_edges",
        answer(host.resume_snapshot(true), Value::Object),
    );
    step(
        "seed",
        answer(
            host.seed_frontier(&[seed("https://example.test/")]),
            Value::Object,
        ),
    );
    step(
        "seed_again",
        answer(
            host.seed_frontier(&[seed("https://example.test/")]),
            Value::Object,
        ),
    );
    let bad: Map<String, Value> = [("requested_url".to_string(), json!("x"))]
        .into_iter()
        .collect();
    step(
        "seed_bad",
        answer(host.seed_frontier(&[bad]), Value::Object),
    );
    step(
        "snapshot",
        answer(host.resume_snapshot(false), Value::Object),
    );
    step("enqueue", answer(host.enqueue(&urls, "enqueue"), leases));
    step(
        "enqueue_again",
        answer(host.enqueue(&urls[..2], "again"), leases),
    );
    step(
        "enqueue_bad_depth",
        answer(
            host.enqueue(&[("https://example.test/x".into(), -1)], "enqueue"),
            leases,
        ),
    );
    step("enqueue_bad_reason", answer(host.enqueue(&[], ""), leases));
    let mut claim = |n: i64| {
        let r = host.claim(n);
        if let Ok(l) = &r {
            claimed.extend(l.clone());
        }
        answer(r, leases)
    };
    step("claim_2", claim(2));
    step("claim_5_capped", claim(5));
    step("claim_bad", claim(0));
    step(
        "exclude_not_head",
        answer(
            host.exclude_lease(&claimed[1], "robots", &runtime(&[]), &[]),
            |_| Value::Null,
        ),
    );
    step(
        "exclude_head",
        answer(
            host.exclude_lease(&claimed[0], "robots", &runtime(&[]), &[]),
            |_| Value::Null,
        ),
    );
    step(
        "exclude_bad_reason",
        answer(
            host.exclude_lease(&claimed[1], "", &runtime(&[]), &[]),
            |_| Value::Null,
        ),
    );
    step(
        "snapshot_after_exclude",
        answer(host.resume_snapshot(true), Value::Object),
    );
    step("recover", answer(host.recover_inflight(), |n| json!(n)));
    step(
        "recover_none",
        answer(host.recover_inflight(), |n| json!(n)),
    );
    let classify = |url: &str| {
        if url.contains("/private/") {
            "excluded_by_pattern".to_string()
        } else {
            String::new()
        }
    };
    let counts = |m: std::collections::BTreeMap<String, i64>| json!(m);
    step(
        "narrow_scope",
        answer(
            host.narrow_scope(&classify, Some("/private/"), None),
            counts,
        ),
    );
    step(
        "narrow_none",
        answer(host.narrow_scope(&|_| String::new(), None, None), counts),
    );
    step(
        "operator",
        answer(
            host.enqueue_operator(&[
                "https://example.test/op1".into(),
                "https://example.test/p0".into(),
                "https://example.test/op2".into(),
            ]),
            counts,
        ),
    );
    let reasons = |r: &[&str]| r.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    step(
        "partial",
        answer(
            host.partial_reasons(&reasons(&["first reason", "second reason", "first reason"])),
            |_| Value::Null,
        ),
    );
    step(
        "partial_bad",
        answer(host.partial_reasons(&reasons(&[""])), |_| Value::Null),
    );
    step(
        "partial_none",
        answer(host.partial_reasons(&[]), |_| Value::Null),
    );
    step(
        "request_count",
        answer(host.record_request_count(7), |_| Value::Null),
    );
    step(
        "request_count_negative",
        answer(host.record_request_count(-1), |_| Value::Null),
    );
    step(
        "runtime_regress",
        answer(
            host.write_runtime(&runtime(&[("max_depth_reached", json!(0))]), 0),
            |_| Value::Null,
        ),
    );
    let bad_key: Map<String, Value> = [("x".to_string(), json!(1))].into_iter().collect();
    step(
        "runtime_bad_key",
        answer(host.write_runtime(&bad_key, 0), |_| Value::Null),
    );
    let wide = runtime(&[(
        "throttle",
        json!({"delay_seconds": 0.0, "concurrency": 99, "consecutive_ok": 0}),
    )]);
    step(
        "runtime_bad_throttle",
        answer(host.write_runtime(&wide, 0), |_| Value::Null),
    );
    let ok = runtime(&[
        ("max_depth_reached", json!(2)),
        ("elapsed_seconds", json!(1.5)),
        ("crawl_delay_applied", json!(2.0)),
        (
            "throttle",
            json!({"delay_seconds": 0.5, "concurrency": 2, "consecutive_ok": 1}),
        ),
    ]);
    step(
        "runtime_ok",
        answer(host.write_runtime(&ok, 0), |_| Value::Null),
    );
    step(
        "query_limit",
        answer(host.stored_query_limit(), |n| json!(n)),
    );
    step(
        "snapshot_final",
        answer(host.resume_snapshot(true), Value::Object),
    );
    step(
        "stop_by_budget",
        answer(host.stop_by_budget("url budget"), |_| Value::Null),
    );
    step(
        "interrupt",
        answer(host.interrupt("killed"), |_| Value::Null),
    );

    let mut failures = Vec::new();
    let want = expected["answers"].as_array().unwrap();
    assert_eq!(actual.len(), want.len());
    for (got, exp) in actual.iter().zip(want) {
        // unit answers carry `value: null` in reference (`None`), `ok` + `value` otherwise
        // the exception class name is the reference; the message is the contract
        let mut exp_value = exp.clone();
        if let Some(m) = exp_value.as_object_mut() {
            m.remove("type");
        }
        if let Some(d) = first_diff(got, &exp_value, &format!("[{}]", exp["op"])) {
            failures.push(d);
        }
    }
    drop(host);
    if let Some(d) = first_diff(&dump_tables(&path), &expected["tables"], "tables") {
        failures.push(d);
    }
    std::fs::remove_dir_all(&dir).ok();
    assert!(
        failures.is_empty(),
        "{} divergence(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

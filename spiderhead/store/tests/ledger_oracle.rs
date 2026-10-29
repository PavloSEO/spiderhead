//! Differential test of the remediation ledger (`storage-ledger-pack-01`, `storage-ledger-ingest`).
//!
//! the reference suite ingests `oracle/raw-scans/basic` with the reference writer, walks a scripted
//! lifecycle and records every table plus every API answer. The Rust ledger must reproduce the
//! same rows and answers once timestamps and UUIDs are normalised. The projected source is fed
//! through `ingest_prepared`, and the real `ingest_scan` path (audit.v2 reader + evidence
//! contract) is compared with that projection.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use spiderhead_store::ledger::ingest::{
    ingest_prepared, ingest_scan, note_source_missing, prepare_source, PreparedSource, Record,
};
use spiderhead_store::ledger::lifecycle::{record_verification, transition_occurrence, Transition};
use spiderhead_store::ledger::queries::{
    ledger_summary, read_cases, read_group_members, remediation_markdown, remediation_report,
    remediation_summary, write_remediation_report, CaseFilter,
};
use spiderhead_store::ledger::schema::open_ledger;
use spiderhead_store::ledger::LedgerRef;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn json_file(name: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(fixtures().join(name)).unwrap()).unwrap()
}

fn normalise(value: &Value) -> Value {
    static TS: OnceLock<Regex> = OnceLock::new();
    static UUID: OnceLock<Regex> = OnceLock::new();
    let ts = TS.get_or_init(|| Regex::new(r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d+)?Z$").unwrap());
    let uuid = UUID.get_or_init(|| {
        Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$").unwrap()
    });
    match value {
        Value::String(s) if ts.is_match(s) && s != "2026-01-02T03:04:05Z" => json!("<TS>"),
        Value::String(s) if uuid.is_match(s) && s.chars().nth(14) == Some('4') && false => {
            json!("<UUID>")
        }
        Value::String(s) if s.contains("verification-basic.json") => json!("<PATH>"),
        Value::Array(a) => Value::Array(a.iter().map(normalise).collect()),
        Value::Object(m) => {
            Value::Object(m.iter().map(|(k, v)| (k.clone(), normalise(v))).collect())
        }
        other => other.clone(),
    }
}

fn raw_scan() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../oracle/raw-scans/basic/scan.sqlite")
}

// The projection of the raw scan's audit.v2 companion equals the one the reference `ingest_scan` feeds
// its writer (scan header with effective fingerprint, audit identity, run, issues with contract
// evidence, groups with their members).
#[test]
fn prepared_source_matches_the_reference_projection() {
    let expected = json_file("ledger-prepared.json");
    let got = prepare_source(&raw_scan()).unwrap();
    let mut failures = Vec::new();
    check(
        "scan",
        &rec(got.scan.clone()),
        &expected["scan"],
        &mut failures,
    );
    check(
        "audit_row",
        &rec(got.audit_row.clone()),
        &expected["audit_row"],
        &mut failures,
    );
    check(
        "run",
        &got.document["run"],
        &expected["document"]["run"],
        &mut failures,
    );
    let issues: Vec<Value> = got.issues.map(Result::unwrap).collect();
    check(
        "issues",
        &Value::Array(issues),
        &expected["issues"],
        &mut failures,
    );
    check(
        "groups",
        &Value::Array(got.groups.clone().unwrap_or_default()),
        &expected["groups"],
        &mut failures,
    );
    assert_eq!(
        got.group_memberships_state,
        expected["group_memberships_state"].as_str().unwrap()
    );
    assert!(got.canonical_audit_sha256.is_none() && got.raw_issues.is_none());
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn prepared(v: &Value) -> PreparedSource<'static> {
    let obj = |k: &str| v[k].as_object().cloned().unwrap_or_default();
    PreparedSource {
        scan: obj("scan"),
        audit_row: obj("audit_row"),
        document: v["document"].clone(),
        canonical_audit_sha256: v["canonical_audit_sha256"].as_str().map(str::to_string),
        issues: Box::new(
            v["issues"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(Ok),
        ),
        raw_issues: v["raw_issues"].as_array().cloned(),
        groups: v["groups"].as_array().cloned(),
        representations: BTreeMap::new(),
        by_url: BTreeMap::new(),
        doc_url: BTreeMap::new(),
        group_memberships_state: v["group_memberships_state"]
            .as_str()
            .unwrap_or("complete")
            .to_string(),
    }
}

fn dump_tables(path: &Path) -> Value {
    let con = Connection::open(path).unwrap();
    let names: Vec<String> = {
        let mut stmt = con
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
        rows.map(Result::unwrap).collect()
    };
    let mut out = Map::new();
    for name in names {
        let columns: Vec<String> = {
            let mut stmt = con
                .prepare(&format!("PRAGMA table_info(\"{name}\")"))
                .unwrap();
            let rows = stmt.query_map([], |r| r.get::<_, String>(1)).unwrap();
            rows.map(Result::unwrap).collect()
        };
        let order = match con.prepare(&format!("SELECT * FROM \"{name}\" ORDER BY rowid")) {
            Ok(_) => "rowid".to_string(),
            Err(_) => (1..=columns.len())
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(","),
        };
        let mut stmt = con
            .prepare(&format!("SELECT * FROM \"{name}\" ORDER BY {order}"))
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
        out.insert(name, json!({"columns": columns, "rows": dumped}));
    }
    Value::Object(out)
}

fn rec(map: Record) -> Value {
    Value::Object(map)
}

fn first_diff(a: &Value, b: &Value, path: &str) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for (k, v) in x {
                match y.get(k) {
                    None => return Some(format!("{path}/{k}: only in rust ({v})")),
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
        _ => (a != b).then(|| format!("{path}: rust {a} vs python {b}")),
    }
}

fn check(name: &str, actual: &Value, expected: &Value, failures: &mut Vec<String>) {
    let actual = normalise(actual);
    let expected = normalise(expected);
    if let Some(d) = first_diff(&actual, &expected, "") {
        failures.push(format!("{name}: {d}"));
    }
}

// binding, summaries and reports), replayed against the reference ledger built from the raw scan.
#[test]
fn ledger_matches_the_reference_oracle() {
    let expected = json_file("ledger-expected.json");
    let answers = &expected["answers"];
    let source = json_file("ledger-prepared.json");
    let dir = std::env::temp_dir().join(format!("sh-ledger-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ledger.sqlite");
    std::fs::copy(fixtures().join("ledger-pre.sqlite"), &path).unwrap();
    let scan_sha = source["scan_sha256"].as_str().unwrap().to_string();
    let now = "2026-01-01T00:00:00Z";
    let mut failures = Vec::new();

    // the bundled empty ledger validates; ingest twice (second is idempotent)
    drop(open_ledger(&path, false).unwrap());
    {
        let con = open_ledger(&path, true).unwrap();
        // the real path: open the raw scan, project its audit.v2 companion through the evidence
        // contract, write the ledger
        let first = ingest_scan(LedgerRef::Conn(&con), &raw_scan()).unwrap();
        check("ingest", &rec(first), &answers["ingest"], &mut failures);
        // the reference-projected source, fed through the same writer, must be a no-op replay
        let replay = ingest_prepared(&con, prepared(&source), &scan_sha, now).unwrap();
        check(
            "ingest_replay_of_reference_projection",
            &rec(replay),
            &answers["ingest_again"],
            &mut failures,
        );
        let again = ingest_scan(LedgerRef::Conn(&con), &raw_scan()).unwrap();
        check(
            "ingest_again",
            &rec(again),
            &answers["ingest_again"],
            &mut failures,
        );
    }
    let lref = LedgerRef::Path(&path);
    check(
        "summary",
        &rec(ledger_summary(lref).unwrap()),
        &answers["summary"],
        &mut failures,
    );
    check(
        "remediation_summary_0",
        &rec(remediation_summary(lref).unwrap()),
        &answers["remediation_summary_0"],
        &mut failures,
    );
    let none = CaseFilter::default();
    let cases = read_cases(lref, &none, Some(1000), 0).unwrap();
    check(
        "cases_all",
        &Value::Object(cases.clone()),
        &answers["cases_all"],
        &mut failures,
    );
    check(
        "cases_page",
        &rec(read_cases(lref, &none, Some(3), 2).unwrap()),
        &answers["cases_page"],
        &mut failures,
    );
    let findings = cases["findings"].as_array().unwrap().clone();
    let first = &findings[0];
    let by_check = CaseFilter {
        check: first["check"].as_str(),
        ..Default::default()
    };
    check(
        "cases_check",
        &rec(read_cases(lref, &by_check, Some(5), 0).unwrap()),
        &answers["cases_check"],
        &mut failures,
    );
    if first["subject_type"] == "url" {
        let by_url = CaseFilter {
            url: first["subject_value"].as_str(),
            ..Default::default()
        };
        check(
            "cases_url",
            &rec(read_cases(lref, &by_url, Some(5), 0).unwrap()),
            &answers["cases_url"],
            &mut failures,
        );
    }
    let by_key = CaseFilter {
        finding_key: first["finding_key"].as_str(),
        ..Default::default()
    };
    check(
        "cases_key",
        &rec(read_cases(lref, &by_key, Some(100), 0).unwrap()),
        &answers["cases_key"],
        &mut failures,
    );
    if answers.get("group_page").is_some() {
        let member = findings
            .iter()
            .flat_map(|f| f["group_memberships"].as_array().unwrap().iter())
            .find(|m| {
                m["group_members_page"]
                    .get("total")
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
                    > 0
            })
            .unwrap_or_else(|| {
                panic!(
                    "no group member page; earlier divergences:\n{}",
                    failures.join("\n")
                )
            });
        let page = read_group_members(
            lref,
            member["source_scan_id"].as_i64().unwrap(),
            member["group_ref"].as_str().unwrap(),
            1,
            2,
            1024 * 1024,
        )
        .unwrap();
        check(
            "group_page",
            &rec(page),
            &answers["group_page"],
            &mut failures,
        );
    }

    // scripted lifecycle
    let targets: Vec<&Value> = findings
        .iter()
        .filter(|f| f["subject_type"] == "url")
        .flat_map(|f| f["occurrences"].as_array().unwrap().iter())
        .filter(|o| {
            o["observations"]
                .as_array()
                .is_some_and(|ob| !ob.is_empty() && ob[0]["role"] == "target")
        })
        .take(3)
        .collect();
    let mut revision = answers["summary"]["ledger_revision"].as_i64().unwrap();
    let mut steps = Vec::new();
    for state in ["verified", "fix_reported", "recheck_pending"] {
        for (i, occ) in targets.iter().enumerate() {
            if i == 2 && state != "verified" {
                continue;
            }
            let reason = format!("step {state}");
            let r = transition_occurrence(
                lref,
                &Transition {
                    occurrence_key: occ["occurrence_key"].as_str().unwrap(),
                    state,
                    actor: "tester",
                    reason: &reason,
                    expected_revision: revision,
                    observation_id: None,
                    decided_at: Some("2026-01-02T03:04:05Z"),
                },
            )
            .unwrap();
            revision = r["ledger_revision"].as_i64().unwrap();
            steps.push(rec(r));
        }
    }
    check(
        "transitions",
        &Value::Array(steps),
        &answers["transitions"],
        &mut failures,
    );
    let refused = |state: &str, key: &str, rev: i64| {
        transition_occurrence(
            lref,
            &Transition {
                occurrence_key: key,
                state,
                actor: "t",
                reason: "x",
                expected_revision: rev,
                observation_id: None,
                decided_at: None,
            },
        )
        .unwrap_err()
        .to_string()
    };
    let k0 = targets[0]["occurrence_key"].as_str().unwrap();
    let k1 = targets[1]["occurrence_key"].as_str().unwrap();
    check(
        "transition_refused",
        &json!(refused("resolved", k0, revision)),
        &answers["transition_refused"],
        &mut failures,
    );
    check(
        "transition_invalid",
        &json!(refused("detected", k0, revision)),
        &answers["transition_invalid"],
        &mut failures,
    );
    check(
        "transition_stale",
        &json!(refused("verified", k1, revision - 1)),
        &answers["transition_stale"],
        &mut failures,
    );

    let pending_key = answers["verification_pending_key"]
        .as_str()
        .unwrap()
        .to_string();
    let verification = record_verification(
        lref,
        &fixtures().join("verification-basic.json"),
        "tester",
        revision,
        Some(std::slice::from_ref(&pending_key)),
        "task-1",
    )
    .unwrap();
    check(
        "verification",
        &rec(verification),
        &answers["verification"],
        &mut failures,
    );
    let again = record_verification(
        lref,
        &fixtures().join("verification-basic.json"),
        "tester",
        revision + 1,
        Some(&[pending_key]),
        "unassigned",
    )
    .unwrap_err()
    .to_string();
    check(
        "verification_again",
        &json!(again),
        &answers["verification_again"],
        &mut failures,
    );
    let source_id = answers["ingest"]["source_scan_id"].as_i64().unwrap();
    check(
        "missing",
        &rec(note_source_missing(lref, source_id, "pruned in test").unwrap()),
        &answers["missing"],
        &mut failures,
    );
    check(
        "missing_again",
        &rec(note_source_missing(lref, source_id, "pruned in test").unwrap()),
        &answers["missing_again"],
        &mut failures,
    );
    check(
        "remediation_summary_1",
        &rec(remediation_summary(lref).unwrap()),
        &answers["remediation_summary_1"],
        &mut failures,
    );
    let report = remediation_report(lref, 5, 1).unwrap();
    check(
        "report",
        &Value::Object(report.clone()),
        &answers["report"],
        &mut failures,
    );
    check(
        "markdown",
        &json!(remediation_markdown(&report).unwrap()),
        &answers["markdown"],
        &mut failures,
    );
    let out_dir = dir.join("report");
    write_remediation_report(lref, &out_dir, 4, 0).unwrap();
    let files = json!({
        "remediation.json": std::fs::read_to_string(out_dir.join("remediation.json")).unwrap(),
        "remediation.md": std::fs::read_to_string(out_dir.join("remediation.md")).unwrap(),
    });
    check(
        "report_files",
        &files,
        &answers["report_files"],
        &mut failures,
    );
    check(
        "final_summary",
        &rec(ledger_summary(lref).unwrap()),
        &answers["final_summary"],
        &mut failures,
    );
    // every table, row by row
    check(
        "tables",
        &dump_tables(&path),
        &expected["tables"],
        &mut failures,
    );
    std::fs::remove_dir_all(&dir).ok();
    assert!(
        failures.is_empty(),
        "{} divergence(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

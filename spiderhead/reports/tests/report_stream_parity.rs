//! Ported tests for stream/materialized report parity
//! (reference the reference suite).
//!
//! The audit.v2 report must emit the same document whether the source is the
//! retained scan (streamed collections) or the materialized audit dict. md,
//! csv, json and docx outputs are byte-compared; xlsx is compared by extracted
//! cell matrices because the workbook embeds a real creation timestamp
//! (reference needed `frozen_office_clock` for its byte comparison).
//!
//! `test_csv_suppression_export_does_not_retain_rendered_rows` is deferred —
//! it measures `tracemalloc` peaks, a reference-only allocation probe; the Rust
//! csv writer streams rows to the sink by construction.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use calamine::{Data, Reader};
use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use spiderhead_reports::build_report::build_report;
use spiderhead_store::audit_v2::write_audit_v2;

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("stream-parity-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn raw_scan() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../oracle/raw-scans/basic/scan.sqlite")
        .canonicalize()
        .expect("raw scan fixture")
}

/// `_scan(scan)`: a real native scan plus its writer binding.
fn binding_for(scan: &Path) -> Map<String, Value> {
    let con =
        rusqlite::Connection::open_with_flags(scan, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("scan db");
    let (uuid, rev, ver, wrev): (String, i64, String, String) = con
        .query_row(
            "SELECT scan_uuid,evidence_revision,writer_version,writer_revision FROM scan WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("binding row");
    Map::from_iter([
        ("scan_uuid".into(), json!(uuid)),
        ("evidence_revision".into(), json!(rev)),
        ("analyzer_version".into(), json!(ver)),
        ("analyzer_revision".into(), json!(wrev)),
    ])
}

/// The audit document from the test: one issue plus a suppressed one, saved
/// scope/coverage notices and exclusion policy in `run`.
fn scope_audit() -> Value {
    let issue = json!({
        "id": "one",
        "check": "TITLE_MISSING",
        "severity": "warning",
        "target_url": "https://example.test/page",
        "message": "Saved title observation.",
        "occurrences_count": 1,
        "locations": [{"source_url": "https://example.test/", "link_path": "/html/body/main/a"}],
    });
    let suppressed = json!({
        "id": "excluded",
        "check": "TITLE_MISSING",
        "severity": "warning",
        "target_url": "https://example.test/excluded",
        "message": "Saved title observation.",
        "occurrences_count": 1,
        "locations": [{"source_url": "https://example.test/", "link_path": "/html/body/main/a"}],
        "suppression": {
            "rule_id": "approved",
            "reason": "Intentional sample",
            "pattern": "*/excluded",
        },
    });
    json!({
        "schema_version": "2.0",
        "run": {
            "collector": "spiderhead.crawl",
            "generated_at": "2026-10-06T00:00:00Z",
            "crawl_partial": true,
            "crawl_finish_reason": "page budget",
            "checks_disabled": [{"id": "TITLE_SHORT", "reason": "Operator choice"}],
            "checks_skipped": [{"id": "H1_MISSING", "reason": "Saved input unavailable"}],
            "finding_exclusion_policy": [
                {"id": "approved", "pattern": "*/excluded", "reason": "Intentional sample"}
            ],
        },
        "summary": {
            "totals": {"urls_crawled": 1, "issues_total": 1},
            "by_severity": {"warning": 1},
            "health_score_basis": "Saved coverage basis remains visible.",
            "health_score_scope": "One retained page only.",
            "finding_exclusions": {"suppressed_total": 1, "rules_configured": 1},
            "evidence_contract": {
                "scan_identity_state": "unavailable",
                "scan_identity_reason": "Not retained",
            },
        },
        "issues": [issue],
        "groups": [],
        "pages": [
            {"url": "https://example.test/page", "status_code": 200, "metrics": {"title": "Page"}}
        ],
        "suppressed_issues": [suppressed],
    })
}

/// `_scan + write_audit_v2`: a retained scan carrying the audit as
/// collections (issues/pages/suppressed_issues pulled out of the header).
fn retained(dir: &Path) -> (PathBuf, Value) {
    let scan = dir.join("scan.sqlite");
    std::fs::copy(raw_scan(), &scan).expect("copy scan");
    let _ = std::fs::remove_file(dir.join("scan.sqlite.audit-v2.sqlite"));
    let audit = scope_audit();
    let mut header = audit.clone();
    let mut collections: IndexMap<String, Vec<Value>> = IndexMap::new();
    for key in ["issues", "pages", "suppressed_issues"] {
        collections.insert(
            format!("/{key}"),
            header[key].as_array().expect("collection").clone(),
        );
        header[key] = json!([]);
    }
    write_audit_v2(
        &scan,
        header.as_object().expect("header"),
        &collections,
        &binding_for(&scan),
    )
    .expect("write audit v2");
    (scan, audit)
}

fn report(data: &Value, fmt: &str, target: &Path) -> Value {
    build_report(
        data,
        fmt,
        Some(target.to_str().expect("target str")),
        None,
        None,
        0,
        "en",
        None,
        None,
    )
}

/// Every worksheet's cell matrix via calamine (values only, like
/// `load_workbook(...).iter_rows(values_only=True)`).
fn xlsx_matrices(path: &Path) -> BTreeMap<String, Vec<Vec<Data>>> {
    let mut workbook: calamine::Xlsx<_> = calamine::open_workbook(path).expect("open xlsx");
    workbook
        .sheet_names()
        .into_iter()
        .map(|name| {
            let rows = workbook
                .worksheet_range(&name)
                .expect("worksheet")
                .rows()
                .map(|row| row.to_vec())
                .collect();
            (name, rows)
        })
        .collect()
}

#[test]
fn stream_preserves_scope_suppression_and_trailing_notes() {
    for fmt in ["md", "csv", "xlsx", "docx"] {
        let dir = tmp(&format!("preserve-{fmt}"));
        let (scan, audit) = retained(&dir);
        let streamed = dir.join(format!("stream.{fmt}"));
        let materialized = dir.join(format!("materialized.{fmt}"));
        let result = report(&json!(scan.to_string_lossy()), fmt, &streamed);
        assert_eq!(result["ok"], json!(true), "{fmt} streamed: {result}");
        let result = report(&audit, fmt, &materialized);
        assert_eq!(result["ok"], json!(true), "{fmt} materialized: {result}");
        if fmt == "xlsx" {
            let left = xlsx_matrices(&streamed);
            let right = xlsx_matrices(&materialized);
            if left != right {
                let mut names: Vec<&String> = left.keys().chain(right.keys()).collect();
                names.sort();
                names.dedup();
                for name in names {
                    let (l, r) = (
                        left.get(name).cloned().unwrap_or_default(),
                        right.get(name).cloned().unwrap_or_default(),
                    );
                    for (i, row) in l.iter().enumerate() {
                        if r.get(i) != Some(row) {
                            panic!(
                                "{name} row {i}: streamed {row:?} != materialized {:?}",
                                r.get(i)
                            );
                        }
                    }
                    if r.len() > l.len() {
                        panic!("{name}: materialized has extra rows {:?}", &r[l.len()..]);
                    }
                }
            }
        } else {
            assert_eq!(
                std::fs::read(&streamed).expect("streamed bytes"),
                std::fs::read(&materialized).expect("materialized bytes"),
                "{fmt} outputs differ"
            );
        }
        if fmt == "csv" {
            for suffix in ["pages.csv", "scope.csv"] {
                assert_eq!(
                    std::fs::read(streamed.with_extension(suffix)).expect("streamed sidecar"),
                    std::fs::read(materialized.with_extension(suffix))
                        .expect("materialized sidecar"),
                    "csv {suffix} differs"
                );
            }
        }
        if fmt == "md" {
            let text = std::fs::read_to_string(&streamed).expect("md text");
            for saved in [
                "Saved coverage basis remains visible.",
                "One retained page only.",
                "Saved input unavailable",
                "Operator choice",
                "Intentional sample",
                "https://example.test/excluded",
                "/html/body/main/a",
            ] {
                assert!(text.contains(saved), "md missing {saved:?}");
            }
        }
    }
}

#[test]
fn malformed_retained_rows_fail_before_publishing_incomplete_report() {
    for collection in ["issues", "pages"] {
        for fmt in ["csv", "xlsx", "json"] {
            let dir = tmp(&format!("malformed-{collection}-{fmt}"));
            let scan = dir.join("malformed.sqlite");
            std::fs::copy(raw_scan(), &scan).expect("copy scan");
            let _ = std::fs::remove_file(dir.join("malformed.sqlite.audit-v2.sqlite"));
            let header = json!({
                "schema_version": "2.0",
                "run": {},
                "summary": {},
                "issues": [],
                "pages": [],
                "groups": [],
            });
            let mut collections: IndexMap<String, Vec<Value>> = IndexMap::new();
            collections.insert("/issues".into(), vec![]);
            collections.insert("/pages".into(), vec![]);
            collections.insert(
                format!("/{collection}"),
                vec![Value::Null, json!({"url": "https://example.test/exact"})],
            );
            write_audit_v2(
                &scan,
                header.as_object().expect("header"),
                &collections,
                &binding_for(&scan),
            )
            .expect("write audit v2");
            let target = dir.join(format!("invalid.{fmt}"));
            let result = report(&json!(scan.to_string_lossy()), fmt, &target);
            assert_eq!(
                result,
                json!({
                    "ok": false,
                    "error": format!("audit.v2 /{collection} record 1 must be an object"),
                }),
                "{collection}/{fmt}"
            );
            assert!(std::fs::read_dir(&dir)
                .expect("dir")
                .filter_map(|e| e.ok())
                .all(|e| !e.file_name().to_string_lossy().starts_with("invalid.")));
        }
    }
}

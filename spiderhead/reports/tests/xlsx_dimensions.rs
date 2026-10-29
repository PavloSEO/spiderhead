//! Ported tests for `reports/xlsx.rs` (reference the reference suite):
//! workbook dimensions must stay usable by read-only consumers.
//!
//! Binary outputs are compared by extracted cell values, not bytes (the oracle
//! freezes its office clock; the Rust writer, like unfrozen reference, stamps real
//! metadata). The monkeypatch-seam tests (split row cap, injected publication
//! failures, disk budget, one-shot/interrupted iterators) are deferred.

use std::path::{Path, PathBuf};

use calamine::{open_workbook, Data, Reader, Xlsx};
use serde_json::{json, Value};
use spiderhead_reports::xlsx;

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("xlsx-dim-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn document(count: usize) -> Value {
    json!({
        "domain": "example.test",
        "findings": (0..count).map(|i| json!({
            "severity": "warning",
            "url": format!("https://example.test/{i}"),
            "client_title": "Missing title",
        })).collect::<Vec<_>>(),
        "pages": (0..count).map(|i| json!({
            "url": format!("https://example.test/{i}"),
            "status": 200,
        })).collect::<Vec<_>>(),
        "site": {
            "tech_detect": {
                "technologies": [
                    {"category": "server", "name": "Fixture", "evidence": "Saved header"}
                ]
            }
        },
        "suppressed_issues": [
            {
                "id": "excluded",
                "check": "TITLE_MISSING",
                "suppression": {"rule_id": "rule", "reason": "Intentional"},
            }
        ],
        "summary": {
            "pages_checked": count,
            "findings_total": count,
            "findings_by_severity": {"warning": count},
            "severity_note": "Saved coverage basis",
            "checks_disabled": [{"id": "H1_MISSING", "reason": "Operator choice"}],
            "tools_failed": [{"tool": "TITLE_LONG", "error": "Input unavailable"}],
            "finding_exclusion_policy": [{"id": "rule", "reason": "Intentional"}],
            "finding_exclusions": {"suppressed_total": 1},
            "project_coverage": {
                "project": {"site": "example.test", "uuid": "fixture"},
                "status": {
                    "state": "partial",
                    "revision": 1,
                    "items": [{"id": "check", "title": "Saved check", "state": "not_run"}],
                },
            },
            "evidence_contract": {
                "scan_identity_state": "unavailable",
                "scan_identity_reason": "Not retained",
            },
        },
    })
}

fn sheet_values(path: &Path) -> Vec<(String, Vec<Vec<String>>)> {
    let mut wb: Xlsx<_> = open_workbook(path).expect("open xlsx");
    let names = wb.sheet_names().to_owned();
    let mut out = Vec::new();
    for name in names {
        let range = wb.worksheet_range(&name).expect("sheet range");
        let rows: Vec<Vec<String>> = range
            .rows()
            .map(|row| {
                row.iter()
                    .map(|c| match c {
                        Data::String(s) => s.clone(),
                        Data::Float(f) => f.to_string(),
                        Data::Int(i) => i.to_string(),
                        Data::Bool(b) => b.to_string(),
                        Data::Empty => String::new(),
                        other => format!("{other:?}"),
                    })
                    .collect()
            })
            .collect();
        out.push((name, rows));
    }
    out
}

fn chart_count(path: &Path) -> usize {
    let file = std::fs::File::open(path).expect("xlsx");
    let zip = zip::ZipArchive::new(file).expect("zip");
    zip.file_names()
        .filter(|n| {
            n.as_ref()
                .map(|s| s.starts_with("xl/charts/") && s.ends_with(".xml"))
                .unwrap_or(false)
        })
        .count()
}

#[test]
fn all_sheet_dimensions_match_emitted_cells_without_read_only_recalculation() {
    for count in [0usize, 3] {
        let dir = tmp(&format!("dims{count}"));
        let document = document(count);
        let materialized = dir.join("materialized.xlsx");
        let streamed = dir.join("streamed.xlsx");
        xlsx::write(&document, &materialized).expect("write");
        xlsx::write_stream(&document, &streamed).expect("write_stream");

        // Values, not bytes: the materialized and streamed paths emit the
        // same workbook contents.
        assert_eq!(sheet_values(&materialized), sheet_values(&streamed));

        let sheets = sheet_values(&streamed);
        let names: std::collections::BTreeSet<&str> =
            sheets.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "Summary",
                "Findings",
                "Pages",
                "Technologies",
                "Finding Exclusions",
                "Suppressed Findings",
                "Project Coverage",
                "Evidence coverage",
            ]
            .into_iter()
            .collect()
        );
        let (f_rows, f_cols) = {
            let mut wb: Xlsx<_> = open_workbook(&streamed).expect("wb");
            wb.worksheet_range("Findings").expect("range").get_size()
        };
        let (p_rows, p_cols) = {
            let mut wb: Xlsx<_> = open_workbook(&streamed).expect("wb");
            wb.worksheet_range("Pages").expect("range").get_size()
        };
        assert_eq!(f_rows, count + 1, "Findings rows");
        assert_eq!(p_rows, count + 1, "Pages rows");
        assert_eq!(f_cols, 10, "Findings cols");
        assert_eq!(p_cols, 11, "Pages cols");

        // The summary chart is part of the workbook (openpyxl `_charts`).
        assert_eq!(chart_count(&streamed), 1);

        // The index sidecar publishes the completed workbook.
        let index: Value = serde_json::from_str(
            &std::fs::read_to_string(streamed.with_extension("xlsx.index.json"))
                .expect("index file"),
        )
        .expect("index json");
        assert_eq!(index["state"], json!("complete"));
        assert_eq!(
            index["source_population"],
            json!({"findings": count, "pages": count})
        );
    }
}

#[test]
fn failed_archive_open_closes_xml_generators_and_removes_tempfiles() {
    let dir = tmp("archive-open");
    // `dir` itself is the target path: not a regular file, write must refuse
    // and leave nothing behind.
    assert!(xlsx::write(&document(1), &dir).is_err());
    assert!(std::fs::read_dir(&dir).unwrap().next().is_none());
}

#[test]
fn excel_cell_overflow_is_rejected_before_openpyxl_can_clip() {
    for location in ["finding", "page", "note", "url", "failure", "technology"] {
        for value in ["x".repeat(32768), format!("={}", "x".repeat(32766))] {
            let dir = tmp(&format!("overflow-{location}"));
            let mut doc = document(1);
            match location {
                "finding" => doc["findings"][0]["client_observation"] = json!(value),
                "page" => doc["pages"][0]["title"] = json!(value),
                "note" => doc["summary"]["severity_note"] = json!(value),
                "url" => doc["url"] = json!(value),
                "failure" => doc["summary"]["tools_failed"][0]["error"] = json!(value),
                "technology" => {
                    doc["site"]["tech_detect"]["technologies"][0]["name"] = json!(value)
                }
                _ => unreachable!(),
            }
            let target = dir.join("too-wide.xlsx");
            let err = xlsx::write(&doc, &target).expect_err("overflow refused");
            let message = err.to_string();
            assert!(message.contains("32767-character limit"), "{message}");
            assert!(message.contains("CSV or JSON"), "{message}");
            assert!(std::fs::read_dir(&dir).unwrap().next().is_none());
        }
    }
}

#[test]
fn exact_cell_limit_roundtrips_after_formula_defense() {
    for value in ["x".repeat(32767), format!("={}", "x".repeat(32765))] {
        let dir = tmp("boundary");
        let mut doc = document(1);
        doc["pages"][0]["title"] = json!(value);
        let target = dir.join("boundary.xlsx");
        xlsx::write(&doc, &target).expect("write");
        let mut wb: Xlsx<_> = open_workbook(&target).expect("wb");
        let range = wb.worksheet_range("Pages").expect("Pages");
        let cell = range.get((1, 2)).expect("C2");
        let expected = if value.starts_with('=') {
            format!("'{value}")
        } else {
            value.clone()
        };
        match cell {
            Data::String(s) => {
                assert_eq!(*s, expected);
                assert_eq!(s.chars().count(), 32767);
            }
            other => panic!("C2 must be a string cell, got {other:?}"),
        }
    }
}

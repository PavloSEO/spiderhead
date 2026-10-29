//! Ported test for byte-identical Office reports from a legacy directory and
//! its scan artifact (reference the reference suite).
//!
//! The `legacy_run` directory is the shared `{audit.json, pages.jsonl,
//! links.jsonl}` fixture — the same shape the reference crawl fixture produces.
//! docx output is byte-compared like the reference; xlsx is compared by extracted
//! cell matrices because the workbook embeds a real creation timestamp
//! (reference needed `frozen_office_clock` monkeypatching for its byte compare).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use calamine::{Data, Reader};
use serde_json::json;
use spiderhead_reports::build_report::build_report;
use spiderhead_store::scan_artifact::{import_run, read_audit};

const BUILD: &str = "25fd2ed032a31d63c5811722619e35c14b631476";

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("office-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn legacy_run(dir: &Path) -> PathBuf {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../store/testdata/legacy-run")
        .canonicalize()
        .expect("legacy-run fixture");
    let run = dir.join("run");
    std::fs::create_dir_all(&run).unwrap();
    for name in ["audit.json", "pages.jsonl", "links.jsonl"] {
        std::fs::copy(fixture.join(name), run.join(name)).expect("copy fixture");
    }
    run
}

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
fn same_run_office_reports_are_byte_identical() {
    for fmt in ["xlsx", "docx"] {
        let dir = tmp(fmt);
        let run = legacy_run(&dir);
        let expected_dir = dir.join("directory");
        std::fs::create_dir_all(&expected_dir).unwrap();
        let expected = expected_dir.join(format!("report.{fmt}"));
        let audit_path = run.join("audit.json");
        assert_eq!(
            build_report(
                &json!(audit_path.to_string_lossy()),
                fmt,
                Some(expected.to_str().unwrap()),
                None,
                None,
                0,
                "en",
                None,
                None,
            )["ok"],
            json!(true),
            "{fmt} directory report"
        );

        let artifact = dir.join("scan.sqlite");
        import_run(&run, &artifact, BUILD, None).expect("import_run");
        let document = read_audit(&artifact).expect("read_audit");
        let actual_dir = dir.join("sqlite");
        std::fs::create_dir_all(&actual_dir).unwrap();
        let actual = actual_dir.join(format!("report.{fmt}"));
        assert_eq!(
            build_report(
                &document,
                fmt,
                Some(actual.to_str().unwrap()),
                None,
                None,
                0,
                "en",
                None,
                None,
            )["ok"],
            json!(true),
            "{fmt} artifact report"
        );

        if fmt == "xlsx" {
            assert_eq!(
                xlsx_matrices(&expected),
                xlsx_matrices(&actual),
                "xlsx cell matrices differ"
            );
        } else {
            assert_eq!(
                std::fs::read(&expected).expect("expected bytes"),
                std::fs::read(&actual).expect("actual bytes"),
                "docx bytes differ"
            );
        }
    }
}

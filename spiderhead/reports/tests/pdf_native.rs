//! Native PDF rendering (issue #43): the overview model becomes a valid PDF
//! with no browser involved — magic header, a page count and Cyrillic text
//! included without a panic.

use serde_json::{json, Value};
use spiderhead_reports::pdf_native::write_pdf_report_native;

fn model(lang_note: &str) -> Value {
    json!({
        "schema": "spiderhead.technical-audit-pdf/1",
        "source": {
            "kind": "site-audit",
            "domain": "example.invalid",
            "url": "https://example.invalid/",
            "generated_at": "2026-11-07T00:00:00Z",
        },
        "summary": {
            "source": {
                "pages_checked": 12,
                "findings_total": 2,
                "findings_by_severity": {"critical": 1, "warning": 1, "notice": 0},
            },
        },
        "findings": [
            {
                "record": {"severity": "critical", "url": "https://example.invalid/каталог/"},
                "display": {"check_key": "BROKEN_INTERNAL_LINK"},
            },
            {
                "record": {"severity": "warning", "url": "https://example.invalid/x"},
                "display": {"check_key": "H2_MULTIPLE"},
            },
        ],
        "note": lang_note,
    })
}

#[test]
fn native_pdf_is_a_valid_document_with_pages_and_cyrillic() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("report.pdf");
    let result = write_pdf_report_native(&model("ru-check"), &path, "ru");
    assert_eq!(result["ok"], json!(true), "{result:?}");
    assert_eq!(result["engine"], json!("native"));
    assert!(
        result["pages"].as_i64().is_some_and(|n| n >= 1),
        "{result:?}"
    );
    let bytes = std::fs::read(&path).expect("pdf bytes");
    assert!(bytes.starts_with(b"%PDF-"), "not a PDF");
    assert!(
        bytes.len() > 1_000,
        "suspiciously small pdf: {}",
        bytes.len()
    );
    // The Cyrillic title survives as embedded font data, not a panic path.
    let result_en =
        write_pdf_report_native(&model("en-check"), &dir.path().join("report-en.pdf"), "en");
    assert_eq!(result_en["ok"], json!(true), "{result_en:?}");
}

#[test]
fn native_pdf_handles_an_empty_findings_model() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("empty.pdf");
    let result = write_pdf_report_native(&json!({}), &path, "en");
    assert_eq!(result["ok"], json!(true), "{result:?}");
    assert!(std::fs::read(&path).unwrap().starts_with(b"%PDF-"));
}

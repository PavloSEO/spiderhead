//! Ported tests for `reports/build_report.rs` PDF routing (reference
//! the reference suite).
//!
//! Only `test_report_build_rejects_unknown_pdf_language_before_writing` is
//! portable: the other tests monkeypatch `build_pdf_model`,
//! `write_pdf_report`, `print_to_pdf`, and `_sanitize_metadata` seams that do
//! not exist in Rust (`write_pdf_report` calls the HTML renderer and the
//! headless-browser bridge directly). The CLI/MCP argument-surface tests are
//! deferred to the cli/mcp crates.

use serde_json::json;
use spiderhead_reports::build_report::build_report;

fn audit() -> serde_json::Value {
    json!({
        "ok": true,
        "schema": "spiderhead.site-audit/1",
        "domain": "fixture.example.invalid",
        "url": "https://fixture.example.invalid/",
        "generated_at": "2026-10-03T00:00:00Z",
        "findings": [
            {
                "id": "fixture-finding-1",
                "check": "TITLE_MISSING",
                "severity": "warning",
                "url": "https://fixture.example.invalid/",
                "text": "Synthetic title finding",
            }
        ],
        "pages": [{"url": "https://fixture.example.invalid/", "status": 200}],
        "summary": {
            "pages_checked": 1,
            "findings_total": 1,
            "findings_by_severity": {"warning": 1},
        },
    })
}

#[test]
fn report_build_rejects_unknown_pdf_language_before_writing() {
    let dir = std::env::temp_dir().join(format!("reports-pdf-lang-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let result = build_report(
        &audit(),
        "pdf",
        Some(dir.join("audit.pdf").to_str().unwrap()),
        None,
        None,
        0,
        "fr",
        None,
        None,
    );

    assert_eq!(
        result,
        json!({"ok": false, "error": "report language must be 'en' or 'ru'"})
    );
    let _ = std::fs::remove_dir_all(&dir);
}

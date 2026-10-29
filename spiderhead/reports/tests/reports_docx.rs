//! Ported tests for the DOCX writer (reference the reference suite,
//! issue #363): the client DOCX must not assert current ranking loss — the
//! `seohead.site-audit/1` contract carries no ranking measurement.
//!
//! Fixture: `tests/fixtures/site_audit_full.json` is a byte copy of the
//! oracle's `docs/examples/reports/full.json`.

use std::path::PathBuf;

use serde_json::Value;
use spiderhead_reports::build_report::build_report;

fn fixture() -> Value {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/site_audit_full.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture file"))
        .expect("fixture json")
}

/// `Document(path).paragraphs` joined by newlines: every `<w:t>` run in
/// `word/document.xml`, one paragraph per line, `<w:br/>` kept as a break.
fn docx_text(path: &std::path::Path) -> String {
    let file = std::fs::File::open(path).expect("docx file");
    let mut zip = zip::ZipArchive::new(file).expect("docx zip");
    let mut xml = String::new();
    std::io::Read::read_to_string(
        &mut zip.by_name("word/document.xml").expect("document.xml"),
        &mut xml,
    )
    .expect("document.xml read");
    let mut paragraphs = Vec::new();
    let mut pos = 0;
    while pos < xml.len() {
        if xml[pos..].starts_with("<w:p>") || xml[pos..].starts_with("<w:p ") {
            let end = xml[pos..].find("</w:p>").map(|i| pos + i).unwrap();
            let mut text = String::new();
            let mut p = pos;
            while p < end {
                if xml[p..].starts_with("<w:t ") || xml[p..].starts_with("<w:t>") {
                    let open = xml[p..].find('>').map(|i| p + i + 1).unwrap();
                    let close = xml[open..].find("</w:t>").map(|i| open + i).unwrap();
                    text.push_str(&xml[open..close]);
                    p = close + 6;
                } else if xml[p..].starts_with("<w:br/>") {
                    text.push('\n');
                    p += 7;
                } else {
                    p += 1;
                }
            }
            paragraphs.push(text);
            pos = end + 6;
        } else {
            pos += 1;
        }
    }
    paragraphs.join("\n")
}

fn rendered_docx_text(tag: &str) -> String {
    let audit = fixture();
    assert!(!serde_json::to_string(&audit)
        .expect("dump")
        .to_lowercase()
        .contains("ranking")); // sanity: no ranking evidence in the input
    let dir = std::env::temp_dir().join(format!("docx-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let output = dir.join("audit.docx");
    let result = build_report(
        &audit,
        "docx",
        Some(output.to_str().unwrap()),
        None,
        None,
        0,
        "en",
        None,
        None,
    );
    assert_eq!(result["ok"], serde_json::json!(true), "{result}");
    docx_text(&output)
}

#[test]
fn critical_section_does_not_claim_current_ranking_loss() {
    let text = rendered_docx_text("ranking").to_lowercase();
    assert!(!text.contains("preventing the site from ranking correctly now"));
    assert!(text.contains("already cost the site") || text.contains("does not measure current"));
    assert!(text.contains("these issues have already cost the site any"));
}

#[test]
fn critical_section_still_flags_severity_without_overclaiming() {
    let text = rendered_docx_text("severity").to_lowercase();
    assert!(text.contains("highest-severity issues"));
}

#[test]
fn severity_note_stays_consistent_with_the_critical_section() {
    let audit = fixture();
    let note = audit["summary"]["severity_note"].as_str().expect("note");
    assert!(note.contains("aggregator rules"));
    let text = rendered_docx_text("note").to_lowercase();
    assert!(text.contains("aggregator"));
}

#[test]
fn word_still_groups_findings_by_severity() {
    let text = rendered_docx_text("groups");
    assert!(text.contains("Critical"));
    assert!(text.contains("Warning"));
    assert!(text.contains("Notice"));
}

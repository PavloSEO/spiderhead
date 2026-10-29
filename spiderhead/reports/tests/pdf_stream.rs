//! Ported tests for `reports/pdf_stream.rs` (reference the reference suite):
//! explicit overview policy and companion/output refusal ordering.
//!
//! Tests that monkeypatch the renderer or module budgets
//! (`MAX_DISPLAY_BYTES`/`MAX_METADATA_BYTES`/`MAX_OUTPUT_BYTES` are consts, and
//! `pdf_output::write_pdf_report` has no fake seam) or drive `pypdf` internals
//! are deferred.

use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use spiderhead_reports::audit_pdf::render_audit_pdf_html;
use spiderhead_reports::build_report::build_report;
use spiderhead_reports::pdf_stream;
use spiderhead_store::audit_v2::{write_audit_v2, AuditV2Reader};

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pdf-stream-{tag}-{}", std::process::id()));
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

/// `_fixture(tmp_path, count)`: a scan.sqlite plus a complete audit.v2
/// companion holding `count` issues and pages.
fn fixture(dir: &Path, count: usize, summary_extra: &Map<String, Value>) -> (PathBuf, Vec<Value>) {
    let scan = dir.join("scan.sqlite");
    std::fs::copy(raw_scan(), &scan).expect("copy scan");
    let _ = std::fs::remove_file(dir.join("scan.sqlite.audit-v2.sqlite"));
    let binding = {
        let con = rusqlite::Connection::open_with_flags(
            &scan,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .expect("scan db");
        let (uuid, rev, ver, wrev): (String, i64, String, String) = con
            .query_row(
                "SELECT scan_uuid,evidence_revision,writer_version,writer_revision FROM scan WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .expect("binding row");
        json!({
            "scan_uuid": uuid,
            "evidence_revision": rev,
            "analyzer_version": ver,
            "analyzer_revision": wrev,
        })
    };
    let issues: Vec<Value> = (0..count)
        .map(|i| {
            json!({
                "id": format!("issue-{i}"),
                "check": "TITLE_MISSING",
                "severity": "warning",
                "target_url": format!("https://example.test/{i}"),
                "message": format!("Saved observation {i}."),
                "occurrences_count": 1,
            })
        })
        .collect();
    let pages: Vec<Value> = (0..count)
        .map(|i| {
            json!({
                "url": format!("https://example.test/{i}"),
                "status_code": 200,
                "metrics": {"title": format!("Page {i}")},
            })
        })
        .collect();
    let mut summary = json!({
        "totals": {"urls_crawled": count, "issues_total": count},
        "by_severity": {"critical": 0, "warning": count, "notice": 0},
        "sitemap": {"missing": []},
        "evidence_contract": {"capability_rows": []},
    });
    for (k, v) in summary_extra {
        summary[k.clone()] = v.clone();
    }
    let header = json!({
        "schema_version": "2.0",
        "run": {
            "source": "https://example.test/",
            "collector": "spiderhead.crawl",
            "crawl_valid": true,
            "crawl_partial": true,
            "crawl_finish_reason": "Page budget reached",
            "checks_skipped": [],
        },
        "summary": summary,
        "issues": [],
        "pages": [],
        "groups": [],
        "suppressed_issues": [],
    });
    let mut collections: IndexMap<String, Vec<Value>> = IndexMap::new();
    collections.insert("/issues".into(), issues.clone());
    collections.insert("/pages".into(), pages.clone());
    collections.insert(
        "/groups".into(),
        vec![json!({"id": "group-1", "members": ["https://example.test/0"]})],
    );
    collections.insert(
        "/suppressed_issues".into(),
        vec![json!({
            "id": "suppressed-1",
            "check": "TITLE_MISSING",
            "severity": "warning",
            "target_url": "https://example.test/0",
            "message": "Saved observation 0.",
            "occurrences_count": 1,
            "suppression": {"rule_id": "approved", "reason": "Intentional"},
        })],
    );
    collections.insert(
        "/run/checks_skipped".into(),
        vec![json!({"id": "H1_MISSING", "reason": "Input unavailable"})],
    );
    collections.insert(
        "/summary/sitemap/missing".into(),
        vec![json!({"url": "https://example.test/sitemap-only"})],
    );
    collections.insert(
        "/summary/evidence_contract/capability_rows".into(),
        vec![
            json!({"check": "TITLE_MISSING", "state": "measured", "reason": "Saved title evidence"}),
            json!({"check": "H1_MISSING", "state": "unavailable", "reason": "Input unavailable"}),
        ],
    );
    write_audit_v2(
        &scan,
        header.as_object().expect("header"),
        &collections,
        binding.as_object().expect("binding"),
    )
    .expect("write audit v2");
    (scan, issues)
}

fn report(data: &Value, fmt: &str, target: &Path, pdf_policy: Option<&str>) -> Value {
    report_lang(data, fmt, target, pdf_policy, "en")
}

fn report_lang(
    data: &Value,
    fmt: &str,
    target: &Path,
    pdf_policy: Option<&str>,
    lang: &str,
) -> Value {
    build_report(
        data,
        fmt,
        Some(target.to_str().expect("target str")),
        None,
        None,
        0,
        lang,
        pdf_policy,
        None,
    )
}

#[test]
fn policy_is_explicit_and_only_applies_to_streamed_pdf() {
    let dir = tmp("policy");
    let (scan, _) = fixture(&dir, 40, &Map::new());
    let target = dir.join("report.pdf");
    let scan_value = json!(scan.to_string_lossy());

    let result = report(&scan_value, "pdf", &target, None);
    assert_eq!(result["ok"], json!(false));
    assert!(result["error"]
        .as_str()
        .expect("error")
        .contains("explicit pdf_policy"));

    for (fmt, policy) in [("csv", "overview-v1"), ("pdf", "first-ten")] {
        let result = report(&scan_value, fmt, &target, Some(policy));
        assert_eq!(result["ok"], json!(false), "{fmt}/{policy}");
        assert!(result["error"]
            .as_str()
            .expect("error")
            .contains("pdf_policy"));
    }

    let reader = AuditV2Reader::new(&scan, true, false, None).expect("reader");
    let document = reader
        .materialize_legacy(pdf_stream::MAX_OUTPUT_BYTES as i64)
        .expect("materialize");
    let result = report(&document, "pdf", &target, Some("overview-v1"));
    assert_eq!(result["ok"], json!(false));
    assert!(result["error"]
        .as_str()
        .expect("error")
        .contains("requires a streamed audit.v2"));
    assert!(!target.exists() && !target.with_extension("files").exists());
}

#[test]
fn overview_refuses_existing_output_without_changing_it() {
    let dir = tmp("existing");
    let (scan, _) = fixture(&dir, 40, &Map::new());
    let target = dir.join("report.pdf");
    std::fs::write(&target, b"prior report").unwrap();
    let result = report(
        &json!(scan.to_string_lossy()),
        "pdf",
        &target,
        Some("overview-v1"),
    );
    assert_eq!(result["ok"], json!(false));
    assert_eq!(std::fs::read(&target).unwrap(), b"prior report");
}

#[test]
fn overview_banner_precedes_full_source_metric_cards() {
    let dir = tmp("banner");
    let (scan, _) = fixture(&dir, 40, &Map::new());
    let reader = AuditV2Reader::new(&scan, true, false, None).expect("reader");
    let mut model = pdf_stream::build_overview_model(&reader, None).expect("model");
    model["artifacts"] = json!(["audit.json", "manifest.json"]);
    let html = render_audit_pdf_html(&model, "en", None).expect("render");
    let overview_at = html.find("PDF overview").expect("banner");
    let grid_at = html.find("<div class=\"metric-grid\">").expect("grid");
    assert!(overview_at < grid_at);
    assert!(html.contains("not a representative sample"));
    assert!(html.contains("Full source") && html.contains("Omitted from PDF"));
    assert!(html.contains("32 rows; 2097152 bytes"));
    assert!(html.contains("<strong>40</strong>"));
    assert!(html.contains(&format!("{}audit.json", pdf_stream::ARTIFACT_LINK_PREFIX)));
}

#[test]
fn overview_refuses_existing_companions_and_preserves_them() {
    let dir = tmp("companions");
    let (scan, _) = fixture(&dir, 40, &Map::new());
    let target = dir.join("report.pdf");
    let bundle = target.with_extension("files");
    std::fs::create_dir(&bundle).unwrap();
    let prior = bundle.join("audit.json");
    std::fs::write(&prior, "prior evidence").unwrap();
    let result = report(
        &json!(scan.to_string_lossy()),
        "pdf",
        &target,
        Some("overview-v1"),
    );
    assert_eq!(result["ok"], json!(false));
    assert_eq!(std::fs::read_to_string(&prior).unwrap(), "prior evidence");
    assert!(!target.exists());
}

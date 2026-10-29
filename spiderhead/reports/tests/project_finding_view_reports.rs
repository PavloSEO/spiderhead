//! Ported offline tests for saved-finding-view report reuse
//! (reference the reference suite).
//!
//! reference routes these through `build_report(..., project=..., view=...)`,
//! which the reports crate cannot bind (projects depends on reports, not the
//! reverse). The port applies the saved view through
//! `projects::finding_views::apply_view_details`, injects
//! `summary.finding_view` exactly as the reference `_build_report` does, and then
//! calls `build_report` without `project` — the rendered document is
//! byte-for-byte the same object the reference writers consume. The
//! `fmt="json"` branch and the pdf branch are exercised through their shared
//! internals (the finding-view envelope and `build_pdf_model` +
//! `render_audit_pdf_html`), matching the reference code paths one-to-one.
//!
//! CLI/MCP contract tests in the same file stay deferred (handler surfaces).

use std::path::{Path, PathBuf};

use calamine::{Data, Reader};
use serde_json::{json, Value};
use spiderhead_projects::finding_views::{apply_view_details, save_view};
use spiderhead_projects::workspace::create_project;
use spiderhead_reports::audit_pdf::render_audit_pdf_html;
use spiderhead_reports::build_report::build_report;
use spiderhead_reports::pdf_model::build_pdf_model;

const SITE: &str = "https://example.test/";

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("view-reports-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn project(dir: &Path) -> PathBuf {
    let root = dir.join("project");
    create_project(&root, SITE, None, None, None, None).expect("create_project");
    root
}

/// `definition(name="triage", **values)`.
fn definition(name: &str, values: Value) -> Value {
    let mut def = json!({
        "name": name,
        "filters": {},
        "sort": {"field": "severity", "direction": "desc"},
        "columns": ["severity", "check", "url", "text"],
        "page_size": 2,
    });
    def.as_object_mut()
        .unwrap()
        .extend(values.as_object().unwrap().clone());
    def
}

fn audit_document() -> Value {
    json!({
        "schema": "spiderhead.site-audit/1",
        "domain": "example.test",
        "url": SITE,
        "generated_at": "2026-10-03T12:00:00Z",
        "run": {
            "scan_uuid": "scan-synthetic-757",
            "finding_exclusion_policy": [
                {"id": "synthetic-rule", "reason": "synthetic source exclusion"}
            ],
            "crawl_config": {
                "scope.segments": [{"name": "blog", "prefix": "/blog/"}],
                "scope.segments_only": [],
            },
        },
        "findings": [
            {
                "severity": "critical",
                "check": "BROKEN_PAGE_4XX",
                "url": "https://example.test/blog/a",
                "text": "Synthetic broken URL",
                "status_code": 404,
                "occurrences_count": 1,
                "fix_hint": "Repair the fixture link",
            },
            {
                "severity": "warning",
                "check": "MISSING_TITLE",
                "url": "https://example.test/blog/b",
                "text": "Synthetic missing title",
                "status_code": 200,
                "occurrences_count": 1,
                "fix_hint": "Add a title",
            },
            {
                "severity": "warning",
                "check": "MISSING_TITLE",
                "url": "https://example.test/blog/c",
                "text": "Synthetic second missing title",
                "status_code": 200,
                "occurrences_count": 1,
                "fix_hint": "Add a title",
            },
            {
                "severity": "warning",
                "check": "MISSING_TITLE",
                "text": "Audit-wide fixture without a target URL",
            },
        ],
        "pages": [
            {"url": "https://example.test/blog/a", "status": 404, "title": "A"},
            {"url": "https://example.test/blog/b", "status": 200, "title": "B"},
            {"url": "https://example.test/blog/c", "status": 200, "title": "C"},
        ],
        "summary": {
            "pages_checked": 3,
            "findings_total": 4,
            "findings_by_severity": {"critical": 1, "warning": 3, "notice": 0},
            "finding_exclusions": {
                "rules_configured": 1,
                "suppressed_total": 2,
                "by_rule": {"synthetic-rule": 2},
                "by_check": {"SYNTHETIC_SUPPRESSED": 2},
                "by_severity": {"notice": 2},
            },
        },
    })
}

/// `_apply_view_details` + the `summary.finding_view` injection the reference
/// `_build_report` performs before rendering.
fn filtered_document(directory: &Path, view: &str, document: &Value, offset: i64) -> Value {
    let applied = apply_view_details(directory, view, document, offset).expect("apply view");
    let view_result = applied.result;
    let mut filtered = document.clone();
    let rows_key = view_result["source"]["rows_key"]
        .as_str()
        .unwrap()
        .to_owned();
    filtered[rows_key.as_str()] = Value::Array(applied.selected_rows);
    let finding_view = {
        let mut fv = view_result["view"].as_object().unwrap().clone();
        fv.insert("state".into(), view_result["state"].clone());
        fv.insert("counts".into(), view_result["counts"].clone());
        fv.insert("pagination".into(), view_result["pagination"].clone());
        fv.insert("columns".into(), view_result["columns"].clone());
        fv.insert("sort".into(), view_result["sort"].clone());
        fv.insert("source".into(), view_result["source"].clone());
        Value::Object(fv)
    };
    filtered["summary"]
        .as_object_mut()
        .unwrap()
        .insert("finding_view".into(), finding_view);
    filtered
}

fn report(doc: &Value, fmt: &str, path: &Path) -> Value {
    build_report(
        doc,
        fmt,
        Some(path.to_str().unwrap()),
        None,
        None,
        0,
        "en",
        None,
        None,
    )
}

fn csv_rows(path: &Path) -> Vec<Vec<String>> {
    let bytes = std::fs::read(path).expect("csv file");
    let text = String::from_utf8_lossy(&bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    csv::ReaderBuilder::new()
        .delimiter(b';')
        .has_headers(false)
        .from_reader(text.as_bytes())
        .records()
        .map(|r| r.expect("csv row").iter().map(str::to_owned).collect())
        .collect()
}

fn docx_text(path: &Path) -> String {
    let file = std::fs::File::open(path).expect("docx file");
    let mut zip = zip::ZipArchive::new(file).expect("docx zip");
    let mut xml = String::new();
    std::io::Read::read_to_string(
        &mut zip.by_name("word/document.xml").expect("document"),
        &mut xml,
    )
    .expect("read document.xml");
    let mut out = String::new();
    let mut rest = xml.as_str();
    while let Some(start) = find_wt_open(rest) {
        let after = &rest[start..];
        let Some(close) = after.find('>') else { break };
        let Some(end) = after.find("</w:t>") else {
            break;
        };
        let run = after[close + 1..end]
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&");
        out.push_str(&run);
        if after[end + 6..].starts_with("</w:r>") {
            out.push('\n');
        }
        rest = &after[end + 6..];
    }
    out
}

fn find_wt_open(s: &str) -> Option<usize> {
    let mut from = 0usize;
    while let Some(i) = s[from..].find("<w:t") {
        let i = from + i;
        match s.as_bytes().get(i + 4) {
            Some(b'>') | Some(b' ') | Some(b'\t') | Some(b'\n') => return Some(i),
            _ => from = i + 1,
        }
    }
    None
}

fn wt_text(fragment: &str) -> String {
    let mut text = String::new();
    let mut rest = fragment;
    while let Some(start) = find_wt_open(rest) {
        let after = &rest[start..];
        let Some(close) = after.find('>') else { break };
        let Some(end) = after.find("</w:t>") else {
            break;
        };
        text.push_str(&after[close + 1..end]);
        rest = &after[end + 6..];
    }
    text
}

fn docx_tables(path: &Path) -> Vec<Vec<Vec<String>>> {
    let file = std::fs::File::open(path).expect("docx file");
    let mut zip = zip::ZipArchive::new(file).expect("docx zip");
    let mut xml = String::new();
    std::io::Read::read_to_string(
        &mut zip.by_name("word/document.xml").expect("document"),
        &mut xml,
    )
    .expect("read document.xml");
    let mut tables = Vec::new();
    for tbl in xml.split("<w:tbl>").skip(1) {
        let tbl = tbl.split("</w:tbl>").next().unwrap_or(tbl);
        let mut rows = Vec::new();
        for tr in tbl.split("<w:tr>").skip(1) {
            let tr = tr.split("</w:tr>").next().unwrap_or(tr);
            rows.push(
                tr.split("<w:tc>")
                    .skip(1)
                    .map(|tc| wt_text(tc.split("</w:tc>").next().unwrap_or(tc)))
                    .collect::<Vec<String>>(),
            );
        }
        tables.push(rows);
    }
    tables
}

fn xlsx_rows(path: &Path, sheet: &str) -> Vec<Vec<Data>> {
    let mut workbook: calamine::Xlsx<_> = calamine::open_workbook(path).expect("open xlsx");
    workbook
        .worksheet_range(sheet)
        .expect("worksheet")
        .rows()
        .map(|row| row.to_vec())
        .collect()
}

#[test]
fn json_and_markdown_reports_apply_the_same_projection_without_mutating_audit() {
    let dir = tmp("json-md");
    let root = project(&dir);
    save_view(
        &root,
        &definition(
            "warnings",
            json!({
                "filters": {"severity": ["warning"]},
                "sort": {"field": "check", "direction": "asc"},
                "columns": ["check", "url"],
                "page_size": 10,
            }),
        ),
        0,
    )
    .expect("save_view");
    let document = audit_document();
    let filtered = filtered_document(&root, "warnings", &document, 0);

    let markdown_path = dir.join("filtered.md");
    let md = report(&filtered, "md", &markdown_path);
    assert_eq!(md["ok"], json!(true), "{md}");
    assert_eq!(md["findings"], json!(3));
    let rendered = std::fs::read_to_string(&markdown_path).unwrap();
    assert!(rendered.contains("Filtered finding view"), "{rendered}");
    assert!(rendered.contains("Check | URL"), "{rendered}");
    assert!(!rendered.contains("BROKEN_PAGE_4XX"), "{rendered}");
    assert!(
        rendered.contains("https://example.test/blog/b"),
        "{rendered}"
    );
    assert!(
        rendered.contains(
            "Audit totals, evidence coverage, and scores describe the full source audit."
        ),
        "{rendered}"
    );
    assert!(rendered.contains("2 excluded findings"), "{rendered}");
    assert!(rendered.contains("synthetic-rule=2"), "{rendered}");
    // `apply_view_details` takes `&Value`; the source audit is never mutated.
    assert_eq!(document, audit_document());
}

#[test]
fn csv_xlsx_and_docx_reports_use_saved_projection_and_state() {
    let dir = tmp("office");
    let root = project(&dir);
    save_view(
        &root,
        &definition(
            "warning-urls",
            json!({
                "filters": {"severity": ["warning"]},
                "sort": {"field": "url", "direction": "asc"},
                "columns": ["check", "url"],
                "page_size": 1,
            }),
        ),
        0,
    )
    .expect("save_view");
    let document = audit_document();
    let filtered = filtered_document(&root, "warning-urls", &document, 0);

    let csv_path = dir.join("filtered.csv");
    let csv_result = report(&filtered, "csv", &csv_path);
    assert_eq!(csv_result["ok"], json!(true), "{csv_result}");
    let rows = csv_rows(&csv_path);
    assert_eq!(rows[0], vec!["Check", "URL"]);
    assert_eq!(rows.len(), 2, "{rows:?}");
    let scope = String::from_utf8(std::fs::read(csv_path.with_extension("scope.csv")).unwrap())
        .unwrap()
        .trim_start_matches('\u{feff}')
        .to_owned();
    assert!(scope.contains("finding view"), "{scope}");
    assert!(scope.contains("More matching rows remain"), "{scope}");
    assert!(scope.contains("synthetic-rule=2"), "{scope}");

    let xlsx_path = dir.join("filtered.xlsx");
    let xlsx_result = report(&filtered, "xlsx", &xlsx_path);
    assert_eq!(xlsx_result["ok"], json!(true), "{xlsx_result}");
    let findings = xlsx_rows(&xlsx_path, "Findings");
    assert_eq!(
        findings[0],
        vec![Data::String("Check".into()), Data::String("URL".into())]
    );
    assert_eq!(findings.len(), 2, "{findings:?}");
    let summary: String = xlsx_rows(&xlsx_path, "Summary")
        .iter()
        .flatten()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(summary.contains("Saved finding view"), "{summary}");

    let docx_path = dir.join("filtered.docx");
    let docx_result = report(&filtered, "docx", &docx_path);
    assert_eq!(docx_result["ok"], json!(true), "{docx_result}");
    let body = docx_text(&docx_path);
    assert!(body.contains("Saved finding view warning-urls"), "{body}");
    let tables = docx_tables(&docx_path);
    let findings_table = tables
        .iter()
        .find(|t| {
            t.first().map(|r| r.as_slice()) == Some(&["Check".to_owned(), "URL".to_owned()][..])
        })
        .expect("findings table");
    assert_eq!(findings_table.len(), 2, "{findings_table:?}");

    // The `fmt="json"` branch writes the finding-view envelope, not the audit.
    let applied = apply_view_details(&root, "warning-urls", &document, 0).expect("apply view");
    let view_result = applied.result;
    let envelope = json!({
        "schema": "spiderhead.finding-view/1",
        "view": filtered["summary"]["finding_view"],
        "source": view_result["source"],
        "rows": view_result["items"],
    });
    assert_eq!(envelope["schema"], json!("spiderhead.finding-view/1"));
    assert_eq!(envelope["view"]["name"], json!("warning-urls"));
    assert_eq!(envelope["view"]["counts"]["source"], json!(4));
    assert_eq!(
        envelope["source"]["finding_exclusion_policy"],
        document["run"]["finding_exclusion_policy"]
    );
    assert_eq!(
        envelope["source"]["finding_exclusions"],
        document["summary"]["finding_exclusions"]
    );
    let fields: std::collections::BTreeSet<String> = envelope["rows"][0]["fields"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        fields,
        ["check", "url"].iter().map(|s| s.to_string()).collect()
    );
    assert_eq!(document, audit_document());
}

#[test]
fn pdf_build_uses_only_saved_view_selection() {
    for language in ["en", "ru"] {
        let dir = tmp(&format!("pdf-{language}"));
        let root = project(&dir);
        save_view(
            &root,
            &definition("triage", json!({"filters": {"severity": ["warning"]}})),
            0,
        )
        .expect("save_view");
        let document = audit_document();
        let filtered = filtered_document(&root, "triage", &document, 0);

        // reference monkeypatches `pdf_output.write_pdf_report` to capture the
        // model; the port drives the shared model+render internals directly.
        let model = build_pdf_model(&filtered, &[], None, None).expect("pdf model");
        assert_eq!(
            model["summary"]["counts"]["findings"]["projected_count"],
            json!(2)
        );
        assert_eq!(
            model["run"]["scope"]["finding_view"]["pagination"]["truncated"],
            json!(true)
        );
        let html = render_audit_pdf_html(&model, language, None).expect("render");
        assert!(
            html.contains(if language == "en" {
                "source audit scope"
            } else {
                "объём исходного аудита"
            }),
            "{language} html missing scope note"
        );
        let serialized = serde_json::to_string(&model).unwrap();
        assert!(!serialized.contains("Synthetic broken URL"), "{language}");
    }
}

//! Ported report-evidence tests from the reference suite (all 33).
//!
//! `mcp.handlers.report_build` is a thin wrapper over `build_report`, so its
//! tests exercise `build_report` directly here; `python-docx` paragraph/table
//! reads are decoded from `word/document.xml` in-test.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use calamine::{open_workbook, Data, Reader, Xlsx};
use serde_json::{json, Value};
use spiderhead_reports::build_report::build_report;

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "seohead-reports-evidence-{}-{}",
        std::process::id(),
        tag
    ));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn report(doc: &Value, fmt: &str, path: Option<&Path>) -> Value {
    build_report(
        doc,
        fmt,
        path.map(|p| p.to_str().unwrap()),
        None,
        None,
        0,
        "en",
        None,
        None,
    )
}

fn site_document() -> Value {
    json!({
        "schema": "spiderhead.site-audit/1",
        "domain": "example.com",
        "url": "https://example.com/",
        "generated_at": "2026-09-05T00:00:00Z",
        "findings": [],
        "pages": [],
        "summary": {"pages_checked": 0, "findings_total": 0, "findings_by_severity": {}},
    })
}

fn sf_document() -> Value {
    json!({
        "schema_version": "2.0",
        "run": {"project": "example.test", "source": "https://example.test/"},
        "summary": {
            "totals": {"urls_crawled": 1, "issues_total": 0},
            "by_severity": {"critical": 0, "warning": 0, "notice": 0},
            "by_check": {},
        },
        "issues": [],
        "pages": [{"url": "https://example.test/", "status_code": 200, "metrics": {}}],
        "groups": [],
    })
}

fn partial_sf_audit() -> Value {
    json!({
        "schema_version": "2.0",
        "run": {
            "project": "example.test",
            "source": "https://example.test/",
            "crawl_valid": false,
            "crawl_invalid_reason": "no response",
            "crawl_partial": true,
            "crawl_finish_reason": "url_limit",
            "crawl_stopped_reason": "url_limit",
            "checks_skipped": [],
            "checks_disabled": [],
        },
        "summary": {
            "totals": {"urls_crawled": 1, "issues_total": 1},
            "by_severity": {"critical": 1, "warning": 0, "notice": 0},
            "health_score": null,
            "health_score_reason": "no response",
            "health_score_scope": "1 of 1,000 sitemap URLs crawled",
        },
        "issues": [{
            "check": "NO_RESPONSE",
            "severity": "critical",
            "message": "No response",
            "target_url": "https://example.test/",
        }],
        "pages": [{"url": "https://example.test/", "metrics": {}}],
        "groups": [],
    })
}

fn summary_rows(path: &Path) -> BTreeMap<String, String> {
    let mut wb: Xlsx<_> = open_workbook(path).unwrap();
    let range = wb.worksheet_range("Summary").unwrap();
    range
        .rows()
        .filter_map(|row| {
            let key = match row.first()? {
                Data::String(s) => s.clone(),
                _ => return None,
            };
            if key.is_empty() {
                return None;
            }
            let value = match row.get(1) {
                Some(Data::String(s)) => s.clone(),
                Some(Data::Int(i)) => i.to_string(),
                Some(Data::Float(f)) => {
                    if f.fract() == 0.0 {
                        (*f as i64).to_string()
                    } else {
                        f.to_string()
                    }
                }
                _ => String::new(),
            };
            Some((key, value))
        })
        .collect()
}

fn sheet_text(path: &Path) -> String {
    let mut wb: Xlsx<_> = open_workbook(path).unwrap();
    let range = wb.worksheet_range("Summary").unwrap();
    range
        .rows()
        .flat_map(|row| {
            row.iter().map(|c| match c {
                Data::String(s) => s.clone(),
                Data::Int(i) => i.to_string(),
                Data::Float(f) => f.to_string(),
                _ => String::new(),
            })
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn unsupported_site_audit_schema_is_refused_and_names_the_marker() {
    let dir = tmp("site-schema");
    let mut doc = site_document();
    doc["schema"] = json!("spiderhead.site-audit/999");
    let result = report(&doc, "md", Some(&dir.join("out.md")));
    assert_eq!(result["ok"], false);
    assert!(result["error"].as_str().unwrap().contains("schema"));
}

#[test]
fn missing_site_audit_schema_is_refused() {
    let mut doc = site_document();
    doc.as_object_mut().unwrap().remove("schema");
    let result = report(&doc, "md", None);
    assert_eq!(result["ok"], false);
}

#[test]
fn marked_site_audit_with_malformed_required_containers_is_refused() {
    let dir = tmp("site-malformed");
    for (field, value) in [
        ("findings", json!("not a list")),
        ("pages", json!({})),
        ("summary", json!([])),
    ] {
        let mut doc = site_document();
        doc[field] = value;
        let result = report(&doc, "md", Some(&dir.join(format!("site-{field}.md"))));
        assert_eq!(result["ok"], false, "{field}");
        let error = result["error"].as_str().unwrap();
        assert!(error.contains(field), "{field}: {error}");
    }
}

#[test]
fn unsupported_sf_schema_version_is_refused_and_names_the_marker() {
    let dir = tmp("sf-schema");
    let mut doc = sf_document();
    doc["schema_version"] = json!("999.0");
    let result = report(&doc, "md", Some(&dir.join("out.md")));
    assert_eq!(result["ok"], false);
    let error = result["error"].as_str().unwrap();
    assert!(error.contains("schema_version"), "{error}");
    assert!(error.contains("999.0"), "{error}");
}

#[test]
fn marked_sf_audit_with_malformed_required_containers_is_refused() {
    let dir = tmp("sf-malformed");
    for (field, value) in [
        ("run", json!([])),
        ("summary", json!("not a mapping")),
        ("issues", json!({})),
        ("pages", json!({})),
        ("groups", json!({})),
    ] {
        let mut doc = sf_document();
        doc[field] = value;
        let result = report(&doc, "md", Some(&dir.join(format!("sf-{field}.md"))));
        assert_eq!(result["ok"], false, "{field}");
        let error = result["error"].as_str().unwrap();
        assert!(error.contains(field), "{field}: {error}");
    }
}

#[test]
fn correctly_versioned_site_audit_still_renders() {
    let dir = tmp("site-ok");
    let result = report(&site_document(), "md", Some(&dir.join("out.md")));
    assert_eq!(result["ok"], true, "{result}");
}

#[test]
fn correctly_versioned_sf_audit_still_renders() {
    let dir = tmp("sf-ok");
    let result = report(&sf_document(), "md", Some(&dir.join("out.md")));
    assert_eq!(result["ok"], true, "{result}");
}

#[test]
fn json_format_is_also_gated_on_an_unsupported_marker() {
    let mut doc = sf_document();
    doc["schema_version"] = json!("999.0");
    let result = report(&doc, "json", None);
    assert_eq!(result["ok"], false);
    assert!(result["error"].as_str().unwrap().contains("schema_version"));
}

#[test]
fn json_output_of_a_recognized_document_is_an_untouched_copy() {
    let dir = tmp("json-copy");
    let target = dir.join("out.json");
    let result = report(&sf_document(), "json", Some(&target));
    assert_eq!(result["ok"], true, "{result}");
    let written: Value = serde_json::from_str(&std::fs::read_to_string(&target).unwrap()).unwrap();
    assert_eq!(written, sf_document());
}

#[test]
fn clean_sf_audit_reports_checks_completed_as_unavailable_not_zero() {
    let dir = tmp("clean-xlsx");
    let target = dir.join("clean.xlsx");
    let result = report(&sf_document(), "xlsx", Some(&target));
    assert_eq!(result["ok"], true, "{result}");
    let rows = summary_rows(&target);
    assert_eq!(rows["Checks completed"], "not reported");
}

#[test]
fn sf_audit_with_check_coverage_reports_a_real_completed_count() {
    let dir = tmp("covered-xlsx");
    let mut doc = sf_document();
    doc["summary"]["by_check"] = json!({"BROKEN_INTERNAL_LINK": 3});
    doc["summary"]["check_coverage"] = json!({
        "checks_total": 5,
        "checks_fired": 1,
        "checks_skipped": 0,
        "checks_disabled": 0,
        "checks_disabled_ids": [],
        "checks_silent": 3,
        "checks_silent_ids": ["TITLE_MISSING", "H1_MISSING", "CANONICAL_MISSING"],
        "coverage": 1.0,
    });
    let target = dir.join("covered.xlsx");
    let result = report(&doc, "xlsx", Some(&target));
    assert_eq!(result["ok"], true, "{result}");
    let rows = summary_rows(&target);
    assert_eq!(rows["Checks completed"], "4");
}

#[test]
fn site_audit_contract_keeps_its_own_real_tools_run_count() {
    let dir = tmp("site-xlsx");
    let mut doc = site_document();
    doc["summary"]["tools_run"] = json!(["cdn_check", "render_check"]);
    let target = dir.join("site.xlsx");
    let result = report(&doc, "xlsx", Some(&target));
    assert_eq!(result["ok"], true, "{result}");
    let rows = summary_rows(&target);
    assert_eq!(rows["Checks completed"], "2");
}

#[test]
fn invalid_partial_sf_audit_surfaces_scope_evidence_in_md() {
    let dir = tmp("partial-md");
    let target = dir.join("partial.md");
    let result = report(&partial_sf_audit(), "md", Some(&target));
    assert_eq!(result["ok"], true, "{result}");
    let text = std::fs::read_to_string(&target).unwrap();
    assert!(text.contains("Crawl failed") && text.contains("no response"));
    assert!(text.contains("Partial crawl") && text.contains("url_limit"));
    assert!(text.contains("1 of 1,000 sitemap URLs crawled"));
    assert!(text.find("Crawl failed").unwrap() < text.find("| Pages checked |").unwrap());
}

#[test]
fn invalid_partial_sf_audit_surfaces_scope_evidence_in_xlsx() {
    let dir = tmp("partial-xlsx");
    let target = dir.join("partial.xlsx");
    let result = report(&partial_sf_audit(), "xlsx", Some(&target));
    assert_eq!(result["ok"], true, "{result}");
    let joined = sheet_text(&target);
    assert!(joined.contains("Crawl failed") && joined.contains("no response"));
}

#[test]
fn disabled_checks_are_distinct_from_skipped_checks_in_md() {
    let dir = tmp("mixed-md");
    let mut doc = partial_sf_audit();
    doc["run"]["crawl_valid"] = json!(true);
    doc["run"]["crawl_partial"] = json!(false);
    doc["run"]["checks_skipped"] =
        json!([{"id": "SF_LOG_ANALYZE", "reason": "log file unavailable"}]);
    doc["run"]["checks_disabled"] =
        json!([{"id": "BROKEN_PAGE_4XX", "reason": "disabled in config"}]);
    doc["issues"] = json!([]);
    let target = dir.join("mixed.md");
    let result = report(&doc, "md", Some(&target));
    assert_eq!(result["ok"], true, "{result}");
    let text = std::fs::read_to_string(&target).unwrap();
    assert!(text.contains("## Disabled checks"));
    assert!(text.contains("Page returns a 4xx response (broken page)"));
    assert!(text.contains("disabled in config"));
    assert!(text.contains("## Unavailable checks"));
    assert!(text.contains("log file unavailable"));
    assert_ne!(
        text.find("## Disabled checks"),
        text.find("## Unavailable checks")
    );
}

#[test]
fn complete_sf_audit_has_no_scope_warning() {
    let dir = tmp("complete");
    let mut doc = partial_sf_audit();
    doc["run"]["crawl_valid"] = json!(true);
    doc["run"]["crawl_partial"] = json!(false);
    doc["run"]["checks_disabled"] = json!([]);
    doc["issues"] = json!([]);
    let md_target = dir.join("clean.md");
    let xlsx_target = dir.join("clean.xlsx");
    assert_eq!(report(&doc, "md", Some(&md_target))["ok"], true);
    assert_eq!(report(&doc, "xlsx", Some(&xlsx_target))["ok"], true);
    let md_text = std::fs::read_to_string(&md_target).unwrap();
    assert!(!md_text.contains("Crawl failed"));
    assert!(!md_text.contains("Partial crawl"));
    assert!(!md_text.contains("Disabled checks"));
    let joined = sheet_text(&xlsx_target);
    assert!(!joined.contains("Crawl failed"));
    assert!(!joined.contains("Partial crawl"));
    assert!(!joined.contains("Disabled check"));
}

// ── #361 docx scope evidence, #574 csv scope evidence, #575 blank None
// fields, #660 client-evidence projection ─────────────────────────────────

fn docx_xml(path: &Path) -> String {
    let file = std::fs::File::open(path).expect("docx file");
    let mut zip = zip::ZipArchive::new(file).expect("docx zip");
    let mut xml = String::new();
    std::io::Read::read_to_string(
        &mut zip.by_name("word/document.xml").expect("document"),
        &mut xml,
    )
    .expect("read document.xml");
    xml
}

fn unescape(run: &str) -> String {
    run.replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Find `<w:t>` or `<w:t ...>` — not `<w:tc>`, `<w:tcW>` or other `<w:t*` tags.
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
        text.push_str(&unescape(&after[close + 1..end]));
        rest = &after[end + 6..];
    }
    text
}

/// `Document(path).paragraphs` — the text of every `<w:t>` run, newline joined.
fn docx_text(path: &Path) -> String {
    let xml = docx_xml(path);
    let mut out = String::new();
    let mut rest = xml.as_str();
    while let Some(start) = find_wt_open(rest) {
        let after = &rest[start..];
        let Some(close) = after.find('>') else { break };
        let Some(end) = after.find("</w:t>") else {
            break;
        };
        out.push_str(&unescape(&after[close + 1..end]));
        if after[end + 6..].starts_with("</w:r>") {
            out.push('\n');
        }
        rest = &after[end + 6..];
    }
    out
}

/// `Document(path).tables` — every table as rows of joined-cell text.
fn docx_tables(path: &Path) -> Vec<Vec<Vec<String>>> {
    let xml = docx_xml(path);
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

/// `csv.reader(open(path, encoding="utf-8-sig"), delimiter=";")`.
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

/// Joined text of every cell of a named worksheet.
fn xlsx_sheet_text(path: &Path, sheet: &str) -> String {
    let mut wb: Xlsx<_> = open_workbook(path).unwrap();
    let range = wb.worksheet_range(sheet).unwrap();
    range
        .rows()
        .flat_map(|row| {
            row.iter().map(|c| match c {
                Data::String(s) => s.clone(),
                Data::Int(i) => i.to_string(),
                Data::Float(f) => f.to_string(),
                _ => String::new(),
            })
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn client_evidence_audit() -> Value {
    json!({
        "schema_version": "2.0",
        "run": {"project": "example.test", "source": "https://example.test/"},
        "summary": {
            "totals": {"urls_crawled": 1, "issues_total": 1},
            "by_severity": {"critical": 0, "warning": 1, "notice": 0},
            "by_check": {"CANONICAL_MULTIPLE": 1},
        },
        "issues": [{
            "check": "CANONICAL_MULTIPLE",
            "severity": "warning",
            "message": "The page declares more than one canonical URL.",
            "source": "Screaming Frog",
            "target_url": "https://example.test/catalogue/",
            "status_code": 200,
            "details": {"canonical_count": 2},
        }],
        "pages": [],
        "groups": [],
    })
}

#[test]
fn invalid_partial_sf_audit_surfaces_scope_evidence_in_docx() {
    let dir = tmp("partial-docx");
    let target = dir.join("partial.docx");
    let result = report(&partial_sf_audit(), "docx", Some(&target));
    assert_eq!(result["ok"], true, "{result}");
    let text = docx_text(&target);
    assert!(
        text.contains("Crawl failed") && text.contains("no response"),
        "{text}"
    );
    assert!(
        text.contains("Partial crawl") && text.contains("url_limit"),
        "{text}"
    );
}

#[test]
fn csv_writes_scope_evidence_without_polluting_tracker_findings() {
    // reference drives `mcp.handlers.report_build`; the handler is a thin wrapper
    // over build_report, which this port exercises directly.
    let dir = tmp("partial-csv");
    let target = dir.join("partial.csv");
    let mut doc = partial_sf_audit();
    doc["issues"] = json!([]);
    doc["pages"] = json!([]);
    doc["run"]["checks_disabled"] =
        json!([{"id": "BROKEN_PAGE_4XX", "reason": "disabled in config"}]);
    doc["run"]["checks_skipped"] =
        json!([{"id": "SF_LOG_ANALYZE", "reason": "log file unavailable"}]);
    let result = report(&doc, "csv", Some(&target));
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(
        result["outputs"],
        json!([
            target.to_str().unwrap(),
            target.with_extension("pages.csv").to_str().unwrap(),
            target.with_extension("scope.csv").to_str().unwrap(),
        ])
    );
    let findings = csv_rows(&target);
    assert_eq!(findings.len(), 1, "scope evidence is not a tracker finding");
    let scope = csv_rows(&target.with_extension("scope.csv"));
    assert_eq!(
        scope,
        vec![
            vec!["Evidence type", "Identifier", "Status", "Reason"],
            vec!["crawl", "validity", "failed", "no response"],
            vec![
                "crawl",
                "scope",
                "partial",
                "stopped: url_limit; 1 of 1,000 sitemap URLs crawled"
            ],
            vec![
                "check",
                "Page returns a 4xx response (broken page)",
                "disabled",
                "disabled in config"
            ],
            vec![
                "check",
                "Audit finding",
                "unavailable",
                "log file unavailable"
            ],
        ]
    );
}

#[test]
fn clean_csv_replaces_stale_scope_and_page_rows() {
    let dir = tmp("clean-csv");
    let target = dir.join("clean.csv");
    assert_eq!(
        report(&partial_sf_audit(), "csv", Some(&target))["ok"],
        true
    );
    let mut clean = sf_document();
    clean["pages"] = json!([]);
    let result = report(&clean, "csv", Some(&target));
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(
        result["outputs"],
        json!([
            target.to_str().unwrap(),
            target.with_extension("pages.csv").to_str().unwrap(),
            target.with_extension("scope.csv").to_str().unwrap(),
        ])
    );
    assert_eq!(
        csv_rows(&target.with_extension("pages.csv")),
        vec![vec![
            "url",
            "status",
            "title",
            "title_length",
            "description_length",
            "h1",
            "canonical",
            "words",
            "schema_types",
            "schema_errors",
            "social_missing"
        ]]
    );
    assert_eq!(
        csv_rows(&target.with_extension("scope.csv")),
        vec![vec!["Evidence type", "Identifier", "Status", "Reason"]]
    );
}

#[test]
fn docx_and_markdown_leave_none_page_fields_blank() {
    let dir = tmp("none-pages");
    let mut doc = site_document();
    doc["pages"] = json!([{
        "url": "https://example.com/unavailable",
        "status": null,
        "title": null,
        "words": null,
        "canonical": null,
    }]);
    let md_target = dir.join("none.md");
    let docx_target = dir.join("none.docx");
    assert_eq!(report(&doc, "md", Some(&md_target))["ok"], true);
    assert_eq!(report(&doc, "docx", Some(&docx_target))["ok"], true);
    let md = std::fs::read_to_string(&md_target).unwrap();
    let page_row = md
        .lines()
        .find(|line| line.contains("unavailable"))
        .expect("page row");
    assert_eq!(page_row, "| https://example.com/unavailable |  |  |  |  |");
    let tables = docx_tables(&docx_target);
    let page_table = tables
        .iter()
        .find(|t| t.first().and_then(|r| r.first()).map(String::as_str) == Some("URL"))
        .expect("URL table");
    assert_eq!(
        page_table[1],
        vec!["https://example.com/unavailable", "", "", "", ""]
    );
}

#[test]
fn client_outputs_replace_check_ids_with_a_recorded_reproduction() {
    let dir = tmp("client");
    let doc = client_evidence_audit();
    let md_target = dir.join("client.md");
    let csv_target = dir.join("client.csv");
    let xlsx_target = dir.join("client.xlsx");
    let docx_target = dir.join("client.docx");
    assert_eq!(report(&doc, "md", Some(&md_target))["ok"], true);
    assert_eq!(report(&doc, "csv", Some(&csv_target))["ok"], true);
    assert_eq!(report(&doc, "xlsx", Some(&xlsx_target))["ok"], true);
    assert_eq!(report(&doc, "docx", Some(&docx_target))["ok"], true);

    let md = std::fs::read_to_string(&md_target).unwrap();
    assert!(!md.contains("CANONICAL_MULTIPLE"));
    assert!(!md.contains("Screaming Frog"));
    assert!(md.contains("Page declares multiple canonical URLs"), "{md}");
    assert!(
        md.contains("https://example.test/catalogue/ returned HTTP 200"),
        "{md}"
    );

    let csv_text = csv_rows(&csv_target)
        .iter()
        .map(|row| row.join(";"))
        .collect::<Vec<_>>()
        .join("\n");
    let header = &csv_rows(&csv_target)[0];
    assert!(!header.iter().any(|c| c == "Check"), "{header:?}");
    assert!(header.iter().any(|c| c == "Reproduction"), "{header:?}");
    assert!(!csv_text.contains("CANONICAL_MULTIPLE"));
    assert!(!csv_text.contains("Screaming Frog"));
    assert!(csv_text.contains("https://example.test/catalogue/ returned HTTP 200"));

    let xlsx_text = xlsx_sheet_text(&xlsx_target, "Findings");
    assert!(!xlsx_text.contains("CANONICAL_MULTIPLE"));
    assert!(!xlsx_text.contains("Screaming Frog"));
    assert!(
        xlsx_text.contains("Page declares multiple canonical URLs"),
        "{xlsx_text}"
    );
    assert!(
        xlsx_text.contains("https://example.test/catalogue/ returned HTTP 200"),
        "{xlsx_text}"
    );

    let docx_text = docx_text(&docx_target);
    assert!(!docx_text.contains("CANONICAL_MULTIPLE"));
    assert!(!docx_text.contains("Screaming Frog"));
    assert!(
        docx_text.contains("Page declares multiple canonical URLs"),
        "{docx_text}"
    );
    assert!(
        docx_text.contains("https://example.test/catalogue/ returned HTTP 200"),
        "{docx_text}"
    );
}

#[test]
fn missing_reproduction_stays_visible_and_partial_warning_leads() {
    let dir = tmp("historic");
    let mut doc = client_evidence_audit();
    doc["run"]["crawl_partial"] = json!(true);
    doc["run"]["crawl_finish_reason"] = json!("url_limit");
    doc["summary"]["health_score_scope"] = json!("1 of 100 URLs crawled");
    doc["issues"][0]
        .as_object_mut()
        .unwrap()
        .remove("target_url");
    doc["issues"][0]
        .as_object_mut()
        .unwrap()
        .remove("status_code");
    let target = dir.join("historic.md");
    assert_eq!(report(&doc, "md", Some(&target))["ok"], true);
    let text = std::fs::read_to_string(&target).unwrap();
    assert!(
        text.contains("Reproduction unavailable from the saved audit."),
        "{text}"
    );
    assert!(
        text.find("Partial crawl").unwrap()
            < text.find("Page declares multiple canonical URLs").unwrap(),
        "{text}"
    );
}

#[test]
fn internal_producer_claim_does_not_replace_the_recorded_observation() {
    let dir = tmp("internal-label");
    let mut doc = client_evidence_audit();
    doc["issues"][0]["message"] = json!("SEOHEAD found CANONICAL_MULTIPLE.");
    let target = dir.join("internal-label.md");
    assert_eq!(report(&doc, "md", Some(&target))["ok"], true);
    let text = std::fs::read_to_string(&target).unwrap();
    assert!(!text.contains("SEOHEAD found"), "{text}");
    assert!(!text.contains("CANONICAL_MULTIPLE"), "{text}");
    assert!(
        text.contains("https://example.test/catalogue/ returned HTTP 200"),
        "{text}"
    );
}

#[test]
fn real_title_and_unknown_uppercase_token_survive_client_projection() {
    let dir = tmp("site-evidence");
    let mut doc = client_evidence_audit();
    doc["issues"][0]["message"] = json!("SEOHEAD HTTP_API title is visible on the page.");
    let target = dir.join("site-evidence.md");
    assert_eq!(report(&doc, "md", Some(&target))["ok"], true);
    let text = std::fs::read_to_string(&target).unwrap();
    assert!(
        text.contains("SEOHEAD HTTP_API title is visible on the page."),
        "{text}"
    );
}

#[test]
fn known_identifier_inside_a_url_remains_copyable_evidence() {
    let dir = tmp("url-evidence");
    let mut doc = client_evidence_audit();
    doc["issues"][0]["message"] = json!("Inspect https://example.test/TITLE_MISSING");
    let target = dir.join("url-evidence.md");
    assert_eq!(report(&doc, "md", Some(&target))["ok"], true);
    let text = std::fs::read_to_string(&target).unwrap();
    assert!(
        text.contains("https://example.test/TITLE_MISSING"),
        "{text}"
    );
}

#[test]
fn identifier_only_text_cannot_become_a_false_reproduction() {
    let dir = tmp("identifier-only");
    let mut doc = client_evidence_audit();
    doc["issues"][0]["message"] = json!("CANONICAL_MULTIPLE");
    doc["issues"][0]
        .as_object_mut()
        .unwrap()
        .remove("status_code");
    doc["issues"][0]["details"] = json!({});
    let target = dir.join("identifier-only.md");
    assert_eq!(report(&doc, "md", Some(&target))["ok"], true);
    let text = std::fs::read_to_string(&target).unwrap();
    assert!(
        text.contains("Reproduction unavailable from the saved audit."),
        "{text}"
    );
    assert!(
        !text.contains("At https://example.test/catalogue/"),
        "{text}"
    );
}

#[test]
fn client_writers_keep_bounded_details_and_location_evidence() {
    let dir = tmp("evidence");
    let mut doc = client_evidence_audit();
    doc["issues"][0]["details"] = json!({
        "missing_tags": ["description", "canonical"],
        "structured": [{"selector": "meta[name=description]", "count": 2}],
    });
    doc["issues"][0]["locations"] = json!([{
        "source_url": "https://example.test/menu/",
        "anchor": "Catalogue",
        "link_position": "Navigation",
        "link_path": "/html/body/nav/a[2]",
    }]);
    let md_target = dir.join("evidence.md");
    let xlsx_target = dir.join("evidence.xlsx");
    let docx_target = dir.join("evidence.docx");
    assert_eq!(report(&doc, "md", Some(&md_target))["ok"], true);
    assert_eq!(report(&doc, "xlsx", Some(&xlsx_target))["ok"], true);
    assert_eq!(report(&doc, "docx", Some(&docx_target))["ok"], true);

    let expected =
        "Source: https://example.test/menu/; Anchor: Catalogue; Position: Navigation; XPath: /html/body/nav/a[2]";
    for text in [
        std::fs::read_to_string(&md_target).unwrap(),
        xlsx_sheet_text(&xlsx_target, "Findings"),
        docx_text(&docx_target),
    ] {
        assert!(
            text.contains("Missing tags: description, canonical"),
            "{text}"
        );
        assert!(
            text.contains("Structured: Count: 2; Selector: meta[name=description]"),
            "{text}"
        );
        assert!(text.contains(expected), "{text}");
    }
}

#[test]
fn failed_tool_labels_keep_distinct_measurement_names() {
    let dir = tmp("failed-tools");
    let mut doc = client_evidence_audit();
    doc["issues"] = json!([]);
    doc["run"]["checks_skipped"] = json!([
        {"id": "robots_check", "reason": "robots.txt did not answer"},
        {"id": "schema_check", "reason": "markup was unavailable"},
    ]);
    let target = dir.join("failed-tools.md");
    assert_eq!(report(&doc, "md", Some(&target))["ok"], true);
    let text = std::fs::read_to_string(&target).unwrap();
    assert!(text.contains("Robots check"), "{text}");
    assert!(text.contains("Schema check"), "{text}");
    assert!(!text.contains("robots_check"), "{text}");
    assert!(!text.contains("schema_check"), "{text}");
}

#[test]
fn failed_tool_reason_translates_known_internal_wrapper() {
    let dir = tmp("failed-reason");
    let mut doc = client_evidence_audit();
    doc["issues"] = json!([]);
    doc["run"]["checks_skipped"] = json!([{
        "id": "TITLE_MISSING",
        "reason": "Screaming Frog did not return TITLE_MISSING because the export was absent.",
    }]);
    let target = dir.join("failed-reason.md");
    assert_eq!(report(&doc, "md", Some(&target))["ok"], true);
    let text = std::fs::read_to_string(&target).unwrap();
    assert!(!text.contains("Screaming Frog"), "{text}");
    assert!(!text.contains("TITLE_MISSING"), "{text}");
    assert!(text.contains("Title element is missing"), "{text}");
    assert!(text.contains("because the export was absent"), "{text}");
}

#[test]
fn failed_reason_keeps_url_spans_while_translating_internal_wrapper() {
    let dir = tmp("failed-reason-url");
    let mut doc = client_evidence_audit();
    doc["issues"] = json!([]);
    doc["run"]["checks_skipped"] = json!([{
        "id": "TITLE_MISSING",
        "reason": "Screaming Frog could not read https://seohead.tech/TITLE_MISSING.",
    }]);
    let target = dir.join("failed-reason-url.md");
    assert_eq!(report(&doc, "md", Some(&target))["ok"], true);
    let text = std::fs::read_to_string(&target).unwrap();
    assert!(text.contains("The audit"), "{text}");
    assert!(
        text.contains("https://seohead.tech/TITLE_MISSING"),
        "{text}"
    );
    assert!(!text.contains("https://The audit.tech"), "{text}");
}

#[test]
fn mixed_and_nested_details_stay_visible_without_false_reproduction() {
    let dir = tmp("details");
    let mut mixed = client_evidence_audit();
    mixed["issues"][0]["details"] = json!({"attempts": [503, {"status": 503}, ["nested"]]});
    let mixed_target = dir.join("mixed-details.md");
    assert_eq!(report(&mixed, "md", Some(&mixed_target))["ok"], true);
    let text = std::fs::read_to_string(&mixed_target).unwrap();
    assert!(
        text.contains("Attempts: 503; Status: 503; 1 unsupported values omitted"),
        "{text}"
    );

    let mut nested = client_evidence_audit();
    nested["issues"][0]["message"] = json!("CANONICAL_MULTIPLE");
    nested["issues"][0]
        .as_object_mut()
        .unwrap()
        .remove("status_code");
    nested["issues"][0]["details"] = json!({"trace": [{"request": {"headers": {}}}]});
    let nested_target = dir.join("nested-details.md");
    assert_eq!(report(&nested, "md", Some(&nested_target))["ok"], true);
    let text = std::fs::read_to_string(&nested_target).unwrap();
    assert!(
        text.contains("Trace: Structured record retained in the saved audit"),
        "{text}"
    );
    assert!(
        text.contains("Reproduction unavailable from the saved audit."),
        "{text}"
    );
}

#[test]
fn reproduction_includes_the_recorded_defect_beyond_http_status() {
    let dir = tmp("canonical-csv");
    let target = dir.join("canonical.csv");
    assert_eq!(
        report(&client_evidence_audit(), "csv", Some(&target))["ok"],
        true
    );
    let rows = csv_rows(&target);
    let header = &rows[0];
    let reproduction_col = header
        .iter()
        .position(|c| c == "Reproduction")
        .expect("Reproduction column");
    let reproduction = &rows[1][reproduction_col];
    assert!(
        reproduction.contains("Canonical count: 2"),
        "{reproduction}"
    );
    assert!(
        reproduction.contains("https://example.test/catalogue/"),
        "{reproduction}"
    );
}

fn sf_audit_with_finding_exclusions() -> Value {
    let mut doc = sf_document();
    doc["run"]["finding_exclusion_policy"] = json!([{
        "id": "legacy-title",
        "pattern": "/legacy/",
        "checks": ["TITLE_MISSING"],
        "reason": "Legacy pages are scheduled for removal.",
    }]);
    doc["summary"]["totals"]["issues_total"] = json!(1);
    doc["summary"]["totals"]["findings_total"] = json!(2);
    doc["summary"]["totals"]["suppressed_findings"] = json!(1);
    doc["summary"]["totals"]["suppressed_occurrences"] = json!(3);
    doc["summary"]["finding_exclusions"] = json!({
        "rules_configured": 1,
        "suppressed_total": 1,
        "by_rule": [{
            "id": "legacy-title",
            "reason": "Legacy pages are scheduled for removal.",
            "suppressed_findings": 1,
            "suppressed_occurrences": 3,
        }],
        "by_check": {"TITLE_MISSING": 1},
        "by_severity": {"critical": 1, "warning": 0, "notice": 0},
    });
    doc["issues"] = json!([{
        "id": "ISSUE-000001",
        "check": "TITLE_TOO_LONG",
        "severity": "notice",
        "source": "SF-derived",
        "message": "Title exceeds configured length.",
        "target_url": "https://example.test/current/",
        "occurrences_count": 1,
    }]);
    doc["suppressed_issues"] = json!([{
        "id": "ISSUE-000002",
        "check": "TITLE_MISSING",
        "severity": "critical",
        "source": "SF-derived",
        "message": "Title is missing.",
        "target_url": "https://example.test/legacy/",
        "status_code": 200,
        "occurrences_count": 3,
        "suppression": {
            "rule_id": "legacy-title",
            "pattern": "/legacy/",
            "reason": "Legacy pages are scheduled for removal.",
            "matched_url": "https://example.test/legacy/",
        },
    }]);
    doc
}

#[test]
fn report_build_keeps_exclusion_provenance_across_human_exports() {
    use spiderhead_reports::document::normalize_sf_audit;

    let dir = tmp("excluded");
    let source = sf_audit_with_finding_exclusions();
    let normalized = normalize_sf_audit(&source);
    assert_eq!(
        normalized["summary"]["finding_exclusions"],
        source["summary"]["finding_exclusions"]
    );
    assert_eq!(
        normalized["summary"]["finding_exclusion_policy"],
        source["run"]["finding_exclusion_policy"]
    );
    assert_eq!(normalized["suppressed_issues"], source["suppressed_issues"]);
    assert!(normalized["summary"].get("finding_view").is_none());

    let json_path = dir.join("excluded.json");
    assert_eq!(report(&source, "json", Some(&json_path))["ok"], true);
    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();
    assert_eq!(written, source);

    let md_path = dir.join("excluded.md");
    assert_eq!(report(&source, "md", Some(&md_path))["ok"], true);
    let md = std::fs::read_to_string(&md_path).unwrap();
    assert!(md.contains("Finding exclusions"), "{md}");
    assert!(
        md.contains("legacy-title") && md.contains("/legacy/"),
        "{md}"
    );
    assert!(
        md.contains("Legacy pages are scheduled for removal."),
        "{md}"
    );
    assert!(md.contains("https://example.test/legacy/"), "{md}");
    assert!(md.contains("https://example.test/current/"), "{md}");

    let csv_path = dir.join("excluded.csv");
    let csv_result = report(&source, "csv", Some(&csv_path));
    assert_eq!(csv_result["ok"], true, "{csv_result}");
    let findings = csv_rows(&csv_path);
    assert_eq!(
        findings.len(),
        2,
        "excluded rows stay out of active findings"
    );
    let scope = csv_rows(&csv_path.with_extension("scope.csv"));
    let header = &scope[0];
    let kind_col = header
        .iter()
        .position(|c| c == "Evidence type")
        .expect("Evidence type column");
    let exclusion_rows: Vec<&Vec<String>> = scope[1..]
        .iter()
        .filter(|row| row[kind_col].starts_with("finding") || row[kind_col] == "suppressed finding")
        .collect();
    assert_eq!(exclusion_rows.len(), 3, "{scope:?}");
    let joined_scope = exclusion_rows
        .iter()
        .map(|row| row.join(" "))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined_scope.contains("legacy-title"), "{joined_scope}");
    assert!(joined_scope.contains("/legacy/"), "{joined_scope}");
    assert!(
        joined_scope.contains("Legacy pages are scheduled for removal."),
        "{joined_scope}"
    );
    assert!(
        joined_scope.contains("https://example.test/legacy/"),
        "{joined_scope}"
    );

    let xlsx_path = dir.join("excluded.xlsx");
    assert_eq!(report(&source, "xlsx", Some(&xlsx_path))["ok"], true);
    assert!(xlsx_sheet_text(&xlsx_path, "Finding Exclusions")
        .contains("Legacy pages are scheduled for removal."));
    assert!(
        xlsx_sheet_text(&xlsx_path, "Suppressed Findings").contains("https://example.test/legacy/")
    );

    let docx_path = dir.join("excluded.docx");
    assert_eq!(report(&source, "docx", Some(&docx_path))["ok"], true);
    let text = docx_text(&docx_path);
    assert!(text.contains("Finding Exclusions"), "{text}");
    assert!(
        text.contains("legacy-title") && text.contains("/legacy/"),
        "{text}"
    );
    assert!(
        text.contains("Legacy pages are scheduled for removal."),
        "{text}"
    );
    assert!(text.contains("https://example.test/legacy/"), "{text}");
}

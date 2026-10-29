//! Client-facing Word report organized as a narrative document.
//!
//! Findings appear from highest to lowest severity; the page table is limited and
//! placed at the end because Word is not used for interactive sorting. Output is a
//! minimal OOXML package; tests check extracted text and structure, not bytes.

use std::io::Write;
use std::path::Path;

use serde_json::{json, Value};
use thiserror::Error;

use crate::client_findings::{
    check_title, finding_exclusion_report, finding_view_columns, finding_view_label,
    finding_view_notice,
};
use crate::document::ReportDoc;
use crate::evidence_summary;
use crate::project_coverage::{priority_text, value_text};
use crate::py;

const MAX_PAGES_IN_TABLE: usize = 60;
const MAX_FINDINGS_PER_LEVEL: usize = 40;
const MAX_SUPPRESSED_FINDINGS: usize = 40;

/// Failure writing the DOCX artifact.
#[derive(Debug, Error)]
pub enum DocxError {
    /// Zip/XML assembly failed.
    #[error("docx assembly failed: {0}")]
    Zip(#[from] zip::result::ZipError),
    /// Filesystem write failed.
    #[error("io failed: {0}")]
    Io(#[from] std::io::Error),
}

fn esc(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

struct Doc {
    body: String,
}

impl Doc {
    fn new() -> Self {
        Self {
            body: String::new(),
        }
    }

    fn run(text: &str, bold: bool, color: Option<&str>, size_half_pt: Option<i64>) -> String {
        let mut rpr = String::new();
        if bold {
            rpr.push_str("<w:b/>");
        }
        if let Some(c) = color {
            rpr.push_str(&format!("<w:color w:val=\"{c}\"/>"));
        }
        if let Some(sz) = size_half_pt {
            rpr.push_str(&format!("<w:sz w:val=\"{sz}\"/><w:szCs w:val=\"{sz}\"/>"));
        }
        let rpr = if rpr.is_empty() {
            String::new()
        } else {
            format!("<w:rPr>{rpr}</w:rPr>")
        };
        format!(
            "<w:r>{rpr}<w:t xml:space=\"preserve\">{}</w:t></w:r>",
            esc(text)
        )
    }

    fn paragraph(&mut self, text: &str, style: Option<&str>) {
        let ppr = style
            .map(|s| format!("<w:pPr><w:pStyle w:val=\"{s}\"/></w:pPr>"))
            .unwrap_or_default();
        // Word paragraphs hold one line each; embedded newlines become <w:br/>.
        let mut runs = String::new();
        for (i, line) in text.split('\n').enumerate() {
            if i > 0 {
                runs.push_str("<w:r><w:br/></w:r>");
            }
            runs.push_str(&Self::run(line, false, None, None));
        }
        self.body.push_str(&format!("<w:p>{ppr}{runs}</w:p>"));
    }

    fn paragraph_run(
        &mut self,
        text: &str,
        style: Option<&str>,
        bold: bool,
        color: Option<&str>,
        size: Option<i64>,
    ) {
        let ppr = style
            .map(|s| format!("<w:pPr><w:pStyle w:val=\"{s}\"/></w:pPr>"))
            .unwrap_or_default();
        self.body.push_str(&format!(
            "<w:p>{ppr}{}</w:p>",
            Self::run(text, bold, color, size)
        ));
    }

    fn heading(&mut self, text: &str, level: i64) {
        let style = if level == 0 {
            "Title".to_owned()
        } else {
            format!("Heading{level}")
        };
        self.paragraph(text, Some(&style));
    }

    fn table(&mut self, style: &str, rows: &[Vec<String>], has_header: bool) {
        self.body.push_str(&format!(
            "<w:tbl><w:tblPr><w:tblStyle w:val=\"{style}\"/>\
             <w:tblW w:w=\"0\" w:type=\"auto\"/>\
             <w:tblLook w:val=\"04A0\" w:firstRow=\"1\" w:lastRow=\"0\" w:firstColumn=\"1\" \
             w:lastColumn=\"0\" w:noHBand=\"0\" w:noVBand=\"1\"/></w:tblPr>"
        ));
        let _ = has_header;
        for row in rows {
            self.body.push_str("<w:tr>");
            for cell in row {
                self.body.push_str(&format!(
                    "<w:tc><w:tcPr><w:tcW w:w=\"0\" w:type=\"auto\"/></w:tcPr>\
                     <w:p>{}</w:p></w:tc>",
                    Self::run(cell, false, None, None)
                ));
            }
            self.body.push_str("</w:tr>");
        }
        self.body.push_str("</w:tbl>");
    }
}

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#;

const RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;

const DOC_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;

const STYLES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri"/><w:sz w:val="22"/></w:rPr></w:rPrDefault></w:docDefaults>
<w:style w:type="paragraph" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
<w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/><w:pPr><w:jc w:val="left"/></w:pPr><w:rPr><w:b/><w:sz w:val="56"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:pPr><w:keepNext/><w:outlineLvl w:val="0"/></w:pPr><w:rPr><w:b/><w:sz w:val="32"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Heading2"><w:name w:val="heading 2"/><w:pPr><w:keepNext/><w:outlineLvl w:val="1"/></w:pPr><w:rPr><w:b/><w:sz w:val="28"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="ListBullet"><w:name w:val="List Bullet"/><w:pPr><w:ind w:left="720"/></w:pPr></w:style>
<w:style w:type="paragraph" w:styleId="ListBullet2"><w:name w:val="List Bullet 2"/><w:pPr><w:ind w:left="1440"/></w:pPr></w:style>
<w:style w:type="table" w:styleId="LightGridAccent1"><w:name w:val="Light Grid Accent 1"/><w:tblPr><w:tblBorders><w:top w:val="single" w:sz="4" w:color="BFBFBF"/><w:left w:val="single" w:sz="4" w:color="BFBFBF"/><w:bottom w:val="single" w:sz="4" w:color="BFBFBF"/><w:right w:val="single" w:sz="4" w:color="BFBFBF"/><w:insideH w:val="single" w:sz="4" w:color="BFBFBF"/><w:insideV w:val="single" w:sz="4" w:color="BFBFBF"/></w:tblBorders></w:tblPr></w:style>
</w:styles>"#;

fn write_package(doc: &Doc, path: &Path) -> Result<(), DocxError> {
    let file = std::fs::File::create(path)?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::FileOptions::<()>::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("[Content_Types].xml", opts)?;
    zip.write_all(CONTENT_TYPES.as_bytes())?;
    zip.start_file("_rels/.rels", opts)?;
    zip.write_all(RELS.as_bytes())?;
    zip.start_file("word/_rels/document.xml.rels", opts)?;
    zip.write_all(DOC_RELS.as_bytes())?;
    zip.start_file("word/styles.xml", opts)?;
    zip.write_all(STYLES.as_bytes())?;
    zip.start_file("word/document.xml", opts)?;
    zip.write_all(
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
             <w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
             <w:body>{}<w:sectPr/></w:body></w:document>",
            doc.body
        )
        .as_bytes(),
    )?;
    zip.finish()?;
    Ok(())
}

/// Write `document` to `path`.
pub fn write(document: &dyn ReportDoc, path: &Path) -> Result<(), DocxError> {
    let mut doc = Doc::new();
    let summary = document.get("summary").unwrap_or(json!({}));
    let empty = serde_json::Map::new();
    let by_sev = py::get(&summary, "findings_by_severity")
        .and_then(Value::as_object)
        .unwrap_or(&empty);

    doc.heading(
        &format!(
            "SEO Audit: {}",
            py::str_value(&document.get("domain").unwrap_or(Value::Null))
        ),
        0,
    );
    doc.paragraph(
        &py::str_value(&document.get("url").unwrap_or(Value::Null)),
        None,
    );
    doc.paragraph_run(
        &format!(
            "Generated: {}",
            py::str_value(&document.get("generated_at").unwrap_or(Value::Null))
        ),
        None,
        false,
        None,
        Some(18),
    );

    // A failed or partial crawl must appear before the executive summary, so a reader
    // never mistakes it for a clean, site-wide audit.
    if summary.get("crawl_valid") == Some(&Value::Bool(false)) {
        let reason = py::get(&summary, "crawl_invalid_reason")
            .filter(|v| py::truthy(Some(*v)))
            .map(py::str_value)
            .unwrap_or_else(|| "the crawl produced no usable data".into());
        doc.paragraph_run(
            &format!("Crawl failed — no health score. {reason}"),
            None,
            true,
            Some("C00000"),
            None,
        );
    }
    if py::truthy(summary.get("crawl_partial")) {
        let mut bits: Vec<String> = Vec::new();
        if let Some(finish) = summary
            .get("crawl_finish_reason")
            .filter(|v| py::truthy(Some(*v)))
        {
            bits.push(format!("stopped: {}", py::str_value(finish)));
        }
        if let Some(scope) = summary
            .get("crawl_scope_note")
            .filter(|v| py::truthy(Some(*v)))
        {
            bits.push(py::str_value(scope));
        }
        let detail = if bits.is_empty() {
            String::new()
        } else {
            format!(" {}", bits.join("; "))
        };
        doc.paragraph_run(
            &format!("Partial crawl — scope is limited.{detail}"),
            None,
            true,
            Some("C00000"),
            None,
        );
    }
    if let Some(notice) = finding_view_notice(&summary) {
        doc.paragraph(&notice, None);
    }

    doc.heading("Executive Summary", 1);
    let metrics: Vec<Vec<String>> = [
        (
            "Pages checked",
            summary.get("pages_checked").cloned().unwrap_or(json!(0)),
        ),
        (
            "Critical issues",
            by_sev.get("critical").cloned().unwrap_or(json!(0)),
        ),
        (
            "Warnings",
            by_sev.get("warning").cloned().unwrap_or(json!(0)),
        ),
        ("Notices", by_sev.get("notice").cloned().unwrap_or(json!(0))),
    ]
    .iter()
    .map(|(name, value)| vec![name.to_string(), py::str_value(value)])
    .collect();
    doc.table("LightGridAccent1", &metrics, false);

    let evidence = evidence_summary::rows(&summary);
    if !evidence.is_empty() {
        doc.heading("Saved evidence coverage", 1);
        let mut rows = vec![vec![
            "Kind".into(),
            "Measurement".into(),
            "State".into(),
            "Scope or reason".into(),
        ]];
        rows.extend(evidence);
        doc.table("TableGrid", &rows, true);
    }

    let suppressed = document.suppressed_issues();
    let exclusions = finding_exclusion_report(&summary, Some(&suppressed));
    if let Some(exclusions) = &exclusions {
        doc.heading("Finding Exclusions", 1);
        let total = py::get(exclusions, "suppressed_total")
            .and_then(py::to_i64)
            .unwrap_or(0);
        let finding_label = if total == 1 { "finding" } else { "findings" };
        let rule_count = py::get(exclusions, "rules_configured")
            .and_then(py::to_i64)
            .unwrap_or(0);
        let rule_label = if rule_count == 1 { "rule" } else { "rules" };
        doc.paragraph(
            &format!(
                "The source audit records {total} excluded {finding_label} across {rule_count} \
                 configured URL {rule_label}, representing {} occurrences. They are absent from \
                 the active findings below.",
                py::str_value(
                    exclusions
                        .get("suppressed_occurrences")
                        .unwrap_or(&Value::Null)
                )
            ),
            None,
        );
        let mut rows = vec![vec![
            "Rule".into(),
            "Pattern".into(),
            "Checks".into(),
            "Findings".into(),
            "Occurrences".into(),
            "Reason".into(),
        ]];
        for rule in py::get_list(exclusions, "rules") {
            let checks = py::get_list(rule, "checks")
                .iter()
                .map(py::str_value)
                .collect::<Vec<_>>()
                .join(", ");
            rows.push(vec![
                py::str_value(py::get(rule, "id").unwrap_or(&Value::Null)),
                py::str_value(py::get(rule, "pattern").unwrap_or(&Value::Null)),
                if checks.is_empty() {
                    "all checks".into()
                } else {
                    checks
                },
                py::str_value(py::get(rule, "suppressed_findings").unwrap_or(&Value::Null)),
                py::str_value(py::get(rule, "suppressed_occurrences").unwrap_or(&Value::Null)),
                py::str_value(py::get(rule, "reason").unwrap_or(&Value::Null)),
            ]);
        }
        doc.table("LightGridAccent1", &rows, true);

        let excluded = py::get_list(exclusions, "issues");
        if !excluded.is_empty() {
            doc.heading("Suppressed Findings", 2);
            let mut rows = vec![vec![
                "Check".into(),
                "Severity".into(),
                "URL".into(),
                "Rule".into(),
                "Reason".into(),
            ]];
            for issue in excluded.iter().take(MAX_SUPPRESSED_FINDINGS) {
                let marker = py::get_map(issue, "suppression");
                rows.push(vec![
                    check_title(py::get(issue, "check").unwrap_or(&Value::Null)),
                    py::str_value(py::get(issue, "severity").unwrap_or(&Value::Null)),
                    py::str_value(py::get(issue, "target_url").unwrap_or(&Value::Null)),
                    py::str_value(marker.get("rule_id").unwrap_or(&Value::Null)),
                    py::str_value(marker.get("reason").unwrap_or(&Value::Null)),
                ]);
            }
            doc.table("LightGridAccent1", &rows, true);
            if excluded.len() > MAX_SUPPRESSED_FINDINGS {
                doc.paragraph(
                    &format!(
                        "Showing {MAX_SUPPRESSED_FINDINGS} of {} suppressed findings; the \
                         complete records remain in the source audit JSON.",
                        excluded.len()
                    ),
                    None,
                );
            }
        }
    }

    if let Some(coverage) = py::get(&summary, "project_coverage").filter(|v| v.is_object()) {
        let project = py::get_map(coverage, "project");
        let checklist = py::get_map(coverage, "status");
        doc.heading("Project Checklist Coverage", 1);
        doc.paragraph(
            &format!(
                "Project: {}\nProject UUID: {}\nChecklist state: {}\nRevision: {}",
                py::str_value(project.get("site").unwrap_or(&Value::Null)),
                py::str_value(project.get("uuid").unwrap_or(&Value::Null)),
                py::str_value(checklist.get("state").unwrap_or(&Value::Null)),
                py::str_value(checklist.get("revision").unwrap_or(&Value::Null))
            ),
            None,
        );
        let counts = checklist.get("counts");
        if let Some(counts) = counts.and_then(Value::as_object) {
            let rows: Vec<Vec<String>> = [
                "total",
                "complete",
                "remaining",
                "run",
                "not_applicable",
                "not_run",
                "stale",
                "disabled",
            ]
            .iter()
            .map(|name| {
                let label = name.replace('_', " ");
                let mut chars = label.chars();
                let label = match chars.next() {
                    Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
                    None => label,
                };
                vec![
                    label,
                    py::str_value(counts.get(*name).unwrap_or(&json!(""))),
                ]
            })
            .collect();
            doc.table("LightGridAccent1", &rows, false);
        } else if checklist.get("reason").is_some_and(|v| py::truthy(Some(v))) {
            doc.paragraph(
                &format!(
                    "Reason: {}",
                    py::str_value(checklist.get("reason").unwrap())
                ),
                None,
            );
        }
        let checklist_v = Value::Object(checklist.clone());
        let task_kpis = py::get_map(&checklist_v, "task_kpis");
        if !task_kpis.is_empty() {
            let task_kpis_v = Value::Object(task_kpis.clone());
            let affected = py::get_map(&task_kpis_v, "affected_urls");
            let count = match affected.get("count") {
                Some(Value::Null) | None => "unavailable".to_owned(),
                Some(v) => py::str_value(v),
            };
            doc.paragraph(
                &format!(
                    "Custom tasks: {} complete / {} total; {} assigned; affected URLs: {} ({}; \
                     {} measured, {} unmeasured).",
                    py::str_value(task_kpis.get("complete").unwrap_or(&json!(0))),
                    py::str_value(task_kpis.get("total").unwrap_or(&json!(0))),
                    py::str_value(task_kpis.get("assigned").unwrap_or(&json!(0))),
                    count,
                    affected
                        .get("state")
                        .map(py::str_value)
                        .unwrap_or_else(|| "unavailable".into()),
                    py::str_value(affected.get("measured_tasks").unwrap_or(&json!(0))),
                    py::str_value(affected.get("unmeasured_tasks").unwrap_or(&json!(0)))
                ),
                None,
            );
        }
        for item in py::get_list(&checklist_v, "items") {
            if !item.is_object() {
                continue;
            }
            let title = py::get(item, "title")
                .filter(|v| py::truthy(Some(*v)))
                .cloned()
                .unwrap_or_else(|| py::get(item, "id").cloned().unwrap_or(json!("")));
            doc.paragraph_run(&py::str_value(&title), Some("ListBullet"), true, None, None);
            doc.paragraph(
                &[
                    format!(
                        "ID: {}",
                        py::str_value(py::get(item, "id").unwrap_or(&Value::Null))
                    ),
                    format!(
                        "Kind: {}",
                        py::str_value(py::get(item, "kind").unwrap_or(&Value::Null))
                    ),
                    format!(
                        "Execution: {}",
                        py::str_value(py::get(item, "execution_kind").unwrap_or(&Value::Null))
                    ),
                    format!("Priority: {}", priority_text(item)),
                    format!(
                        "State: {}",
                        py::str_value(py::get(item, "state").unwrap_or(&Value::Null))
                    ),
                    format!(
                        "Attempt: {}",
                        py::str_value(py::get(item, "attempt_status").unwrap_or(&Value::Null))
                    ),
                    format!(
                        "Complete: {}",
                        py::str_value(py::get(item, "complete").unwrap_or(&Value::Null))
                    ),
                    format!("Blocked by: {}", value_text(py::get(item, "blocked_by"))),
                    format!(
                        "Enabled: {}",
                        py::str_value(py::get(item, "enabled").unwrap_or(&Value::Null))
                    ),
                    format!(
                        "Stale: {}",
                        py::str_value(py::get(item, "stale").unwrap_or(&Value::Null))
                    ),
                    format!("Scope: {}", value_text(py::get(item, "scope"))),
                    format!("Measurement: {}", value_text(py::get(item, "measurement"))),
                    format!("Task: {}", value_text(py::get(item, "task"))),
                    format!(
                        "Affected URLs: {}",
                        value_text(py::get(item, "affected_urls"))
                    ),
                    format!(
                        "Reason: {}",
                        py::get(item, "reason")
                            .filter(|v| py::truthy(Some(*v)))
                            .map(py::str_value)
                            .unwrap_or_else(|| py::str_value(
                                checklist.get("reason").unwrap_or(&Value::Null)
                            ))
                    ),
                ]
                .join(" · "),
                Some("ListBullet2"),
            );
        }
    }

    let disabled = py::get_list(&summary, "checks_disabled");
    if !disabled.is_empty() {
        doc.heading("Disabled Checks", 1);
        doc.paragraph(
            "These checks were deliberately turned off and did not run. Their silence is a \
             configuration choice, not a clean result:",
            None,
        );
        for item in disabled {
            doc.paragraph(
                &format!(
                    "{} — {}",
                    check_title(py::get(item, "id").unwrap_or(&Value::Null)),
                    py::str_value(py::get(item, "reason").unwrap_or(&Value::Null))
                ),
                Some("ListBullet"),
            );
        }
    }

    let failed = py::get_list(&summary, "tools_failed");
    if !failed.is_empty() {
        doc.heading("Unavailable Checks", 1);
        doc.paragraph(
            "These checks did not complete, so their evidence is absent from the report. Their \
             silence does not mean that no issues were found:",
            None,
        );
        for item in failed {
            doc.paragraph(
                &format!(
                    "{} — {}",
                    check_title(py::get(item, "tool").unwrap_or(&Value::Null)),
                    py::str_value(py::get(item, "error").unwrap_or(&Value::Null))
                ),
                Some("ListBullet"),
            );
        }
    }

    let view_columns = finding_view_columns(&summary);
    if let Some(columns) = view_columns {
        doc.heading("Saved Finding View", 1);
        let mut rows = vec![columns
            .iter()
            .map(|c| finding_view_label(c))
            .collect::<Vec<_>>()];
        for finding in document.findings(None) {
            let values = py::get_map(&finding, "view_fields");
            rows.push(
                columns
                    .iter()
                    .map(|column| py::str_value(values.get(column).unwrap_or(&Value::Null)))
                    .collect(),
            );
        }
        doc.table("LightGridAccent1", &rows, true);
    } else {
        let streaming = document.is_streaming();
        for level in ["critical", "warning", "notice"] {
            let count = if streaming {
                py::to_i64(by_sev.get(level).unwrap_or(&json!(0))).unwrap_or(0)
            } else {
                document.findings(Some(level)).len() as i64
            };
            if count == 0 {
                continue;
            }
            doc.heading(&format!("{} — {}", crate::severity_title(level), count), 1);
            if level == "critical" {
                doc.paragraph_run(
                    "These are the highest-severity issues this audit found, by the aggregator's \
                     severity rules. This report does not measure current search rankings, so it \
                     does not say whether these issues have already cost the site any.",
                    None,
                    true,
                    Some("C00000"),
                    None,
                );
            }
            for finding in document
                .findings(Some(level))
                .iter()
                .take(MAX_FINDINGS_PER_LEVEL)
            {
                let title = py::get(finding, "client_title")
                    .map(py::str_value)
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "Audit finding".into());
                doc.paragraph_run(&title, Some("ListBullet"), true, None, None);
                if let Some(observation) =
                    py::get(finding, "client_observation").filter(|v| py::truthy(Some(*v)))
                {
                    doc.paragraph(
                        &format!("Observation: {}", py::str_value(observation)),
                        Some("ListBullet2"),
                    );
                }
                doc.paragraph(
                    &format!(
                        "Reproduction: {}",
                        py::str_value(
                            py::get(finding, "client_reproduction").unwrap_or(&Value::Null)
                        )
                    ),
                    Some("ListBullet2"),
                );
                for detail in py::get_list(finding, "client_details") {
                    doc.paragraph(
                        &format!("Evidence: {}", py::str_value(detail)),
                        Some("ListBullet2"),
                    );
                }
                for location in py::get_list(finding, "client_locations") {
                    doc.paragraph(
                        &format!("Location: {}", py::str_value(location)),
                        Some("ListBullet2"),
                    );
                }
            }
            if count > MAX_FINDINGS_PER_LEVEL as i64 {
                doc.paragraph(
                    &format!(
                        "…and {} more. See the Excel version of this audit for the complete list.",
                        count - MAX_FINDINGS_PER_LEVEL as i64
                    ),
                    None,
                );
            }
        }
    }

    let page_count = document
        .page_count()
        .unwrap_or_else(|| document.pages().len() as i64);
    if page_count > 0 {
        doc.heading("Pages", 1);
        let mut rows = vec![vec![
            "URL".into(),
            "Status".into(),
            "Title".into(),
            "Words".into(),
            "Canonical".into(),
        ]];
        for page in document.pages().iter().take(MAX_PAGES_IN_TABLE) {
            let cell = |name: &str, limit: Option<usize>| {
                let text = py::str_value(py::get(page, name).unwrap_or(&Value::Null));
                match limit {
                    Some(limit) => text.chars().take(limit).collect(),
                    None => text,
                }
            };
            rows.push(vec![
                cell("url", None),
                cell("status", None),
                cell("title", Some(120)),
                cell("words", None),
                cell("canonical", Some(120)),
            ]);
        }
        doc.table("LightGridAccent1", &rows, true);
        if page_count > MAX_PAGES_IN_TABLE as i64 {
            doc.paragraph(
                &format!("Showing the first {MAX_PAGES_IN_TABLE} of {page_count} pages."),
                None,
            );
        }
    }

    if let Some(note) = summary
        .get("severity_note")
        .filter(|v| py::truthy(Some(*v)))
    {
        doc.paragraph("", None);
        doc.paragraph_run(&py::str_value(note), None, false, Some("808080"), Some(16));
    }

    write_package(&doc, path)
}

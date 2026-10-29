//! Excel report with Summary, Findings, Pages, and Technologies worksheets.
//!
//! One metadata layout, with the two unbounded tables (findings, pages) replayed to
//! measure widths and then streamed into constant-memory worksheets split at Excel's row limit.
//! Publication is atomic: the workbook and a sha256 index sidecar replace existing outputs
//! together or not at all.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use rust_xlsxwriter::{Chart, ChartType, Format, FormatAlign, Workbook, Worksheet};
use serde_json::{json, Map, Value};
use thiserror::Error;

use spiderhead_core::tabular::neutralize_formula;

use crate::client_findings::{
    check_title, finding_exclusion_report, finding_view_columns, finding_view_label,
    finding_view_notice,
};
use crate::document::ReportDoc;
use crate::evidence_summary;
use crate::project_coverage::value_text;
use crate::py;
use crate::{checks_completed_display, severity_title};
use crate::{DEFAULT_MAX_OUTPUT_BYTES, MIN_FREE_DISK_BYTES};

const HEAD_CRITICAL: u32 = 0xC00000;
const HEAD_WARNING: u32 = 0xBF8F00;
const HEAD_NOTICE: u32 = 0x808080;

const MAX_SUPPRESSED_FINDINGS: usize = 10_000;
const EXCEL_MAX_ROWS: usize = 1_048_576;
const EXCEL_MAX_CELL_CHARS: usize = 32_767;
const MAX_DATA_ROWS: usize = EXCEL_MAX_ROWS - 1;
const MAX_WORKSHEETS: usize = 10_000;

const PAGE_COLUMNS: [&str; 11] = [
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
    "social_missing",
];

/// XLSX generation failure: invalid input, a filesystem error, or an engine error.
#[derive(Debug, Error)]
pub enum XlsxReportError {
    /// A cell, row budget, or row-conservation check failed.
    #[error("{0}")]
    Value(String),
    /// A filesystem operation failed.
    #[error("{0}")]
    Io(#[from] io::Error),
    /// The XLSX engine rejected a write.
    #[error("{0}")]
    Xlsx(#[from] rust_xlsxwriter::XlsxError),
}

/// A typed cell value after `cell` validation.
#[derive(Clone, Debug)]
enum Cell {
    /// Empty cell.
    Blank,
    /// String cell.
    Str(String),
    /// Numeric cell.
    Num(f64),
    /// Boolean cell.
    Bool(bool),
}

impl Cell {
    /// Character length of the cell's text, for width measurement.
    fn width_len(&self) -> usize {
        match self {
            Cell::Blank => 0,
            Cell::Str(s) => s.chars().count(),
            Cell::Num(n) => py_num_text(*n).chars().count(),
            Cell::Bool(b) => {
                if *b {
                    4
                } else {
                    5
                }
            }
        }
    }
}

fn py_num_text(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{}", n)
    }
}

/// `neutralize_formula` on strings; other values pass through untouched.
fn neutralize_value(value: &Value) -> Value {
    match value {
        Value::String(s) => Value::String(neutralize_formula(s)),
        other => other.clone(),
    }
}

/// `cell`: neutralize formula-leading text and enforce the cell-char limit.
fn cell(value: &Value) -> Result<Cell, XlsxReportError> {
    match neutralize_value(value) {
        Value::Null => Ok(Cell::Blank),
        Value::Bool(b) => Ok(Cell::Bool(b)),
        Value::Number(n) => Ok(Cell::Num(n.as_f64().unwrap_or(0.0))),
        Value::String(s) => {
            if s.chars().count() > EXCEL_MAX_CELL_CHARS {
                Err(XlsxReportError::Value(
                    "XLSX cell exceeds Excel's 32767-character limit; use complete CSV or JSON output"
                        .to_owned(),
                ))
            } else {
                Ok(Cell::Str(s))
            }
        }
        other => Ok(Cell::Str(py::str_value(&other))),
    }
}

fn severity_color(level: &str) -> Option<u32> {
    match level {
        "critical" => Some(HEAD_CRITICAL),
        "warning" => Some(HEAD_WARNING),
        "notice" => Some(HEAD_NOTICE),
        _ => None,
    }
}

/// One planned worksheet: rows of validated cells plus style metadata.
struct SheetDef {
    title: String,
    rows: Vec<Vec<Cell>>,
    /// (row, col) → format, applied while the row is written.
    styles: HashMap<(u32, u16), Format>,
    /// 0-indexed row to freeze above.
    freeze_row: Option<u32>,
    /// Optional bar chart anchored at (row, col), 0-indexed.
    chart: Option<(u32, u16, Chart)>,
    /// Autofilter (first_row, last_row, last_col), 0-indexed.
    autofilter: Option<(u32, u32, u16)>,
}

impl SheetDef {
    fn new(title: &str) -> SheetDef {
        SheetDef {
            title: title.to_owned(),
            rows: Vec::new(),
            styles: HashMap::new(),
            freeze_row: None,
            chart: None,
            autofilter: None,
        }
    }
}

fn header_format() -> Format {
    Format::new()
        .set_bold()
        .set_font_color(rust_xlsxwriter::Color::RGB(0xFFFFFF))
        .set_background_color(rust_xlsxwriter::Color::RGB(0x1F3864))
        .set_align(FormatAlign::VerticalCenter)
        .set_text_wrap()
}

/// `style_header`: style non-empty cells of `row` and freeze beneath it.
fn style_header(sheet: &mut SheetDef, row: u32) {
    if let Some(cells) = sheet.rows.get(row as usize) {
        for (col, item) in cells.iter().enumerate() {
            if matches!(item, Cell::Blank) {
                continue;
            }
            sheet.styles.insert((row, col as u16), header_format());
        }
    }
    sheet.freeze_row = Some(row + 1);
}

/// `autofit`: `min(max(longest + 2, 10), limit)` per column.
fn autofit(rows: &[Vec<Cell>], limits: &[(usize, f64)]) -> Vec<f64> {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0usize; cols];
    for row in rows {
        for (index, item) in row.iter().enumerate() {
            widths[index] = widths[index].max(item.width_len());
        }
    }
    widths
        .iter()
        .enumerate()
        .map(|(index, longest)| {
            let cap = limits
                .iter()
                .find(|(col, _)| *col == index + 1)
                .map(|(_, cap)| *cap)
                .unwrap_or(60.0);
            ((longest + 2).max(10) as f64).min(cap)
        })
        .collect()
}

fn get_obj<'a>(value: &'a Value, key: &str) -> &'a Map<String, Value> {
    static EMPTY: std::sync::LazyLock<Map<String, Value>> = std::sync::LazyLock::new(Map::new);
    py::get(value, key)
        .and_then(Value::as_object)
        .unwrap_or(&EMPTY)
}

/// `_metadata_workbook`'s Summary sheet plus the derived exclusions record.
fn build_summary(document: &Value) -> Result<(SheetDef, Option<Value>), XlsxReportError> {
    let mut sheet = SheetDef::new("Summary");
    let summary_v = py::get(document, "summary").cloned().unwrap_or(Value::Null);
    let summary = &summary_v;
    let by_sev = get_obj(summary, "findings_by_severity");

    sheet
        .styles
        .insert((0, 0), Format::new().set_bold().set_font_size(16.0));
    sheet.rows.push(vec![cell(&Value::String(format!(
        "SEO Audit: {}",
        py::str_value(document.get("domain").unwrap_or(&Value::Null))
    )))?]);
    sheet
        .rows
        .push(vec![cell(document.get("url").unwrap_or(&Value::Null))?]);
    sheet.rows.push(vec![cell(&Value::String(format!(
        "Generated: {}",
        py::str_value(document.get("generated_at").unwrap_or(&Value::Null))
    )))?]);

    // Scope rows must precede the metrics so a reader sees the coverage limits first.
    let mut scope_rows: Vec<String> = Vec::new();
    if summary.get("crawl_valid") == Some(&Value::Bool(false)) {
        let reason = {
            let value = summary.get("crawl_invalid_reason").unwrap_or(&Value::Null);
            if py::truthy(Some(value)) {
                py::str_value(value)
            } else {
                "the crawl produced no usable data".to_owned()
            }
        };
        scope_rows.push(format!("Crawl failed -- no health score. {reason}"));
    }
    if py::truthy(summary.get("crawl_partial")) {
        let finish = summary.get("crawl_finish_reason").unwrap_or(&Value::Null);
        let scope = summary.get("crawl_scope_note").unwrap_or(&Value::Null);
        let mut bits: Vec<String> = Vec::new();
        if py::truthy(Some(finish)) {
            bits.push(format!("stopped: {}", py::str_value(finish)));
        }
        if py::truthy(Some(scope)) {
            bits.push(py::str_value(scope));
        }
        scope_rows.push(format!(
            "Partial crawl -- scope is limited.{}",
            if bits.is_empty() {
                String::new()
            } else {
                format!(" {}", bits.join("; "))
            }
        ));
    }
    for item in py::get_list(summary, "checks_disabled") {
        scope_rows.push(format!(
            "Disabled check {} -- {}",
            check_title(py::get(item, "id").unwrap_or(&Value::Null)),
            py::str_value(py::get(item, "reason").unwrap_or(&Value::Null))
        ));
    }
    if let Some(notice) = finding_view_notice(summary) {
        scope_rows.push(notice);
    }
    let suppressed = document.get("suppressed_issues").and_then(Value::as_array);
    let exclusions = finding_exclusion_report(summary, suppressed.map(Vec::as_slice));
    if let Some(exclusions) = exclusions.as_ref() {
        let count =
            py::to_i64(exclusions.get("suppressed_total").unwrap_or(&Value::Null)).unwrap_or(0);
        let occurrences = py::to_i64(
            exclusions
                .get("suppressed_occurrences")
                .unwrap_or(&Value::Null),
        )
        .unwrap_or(0);
        let rule_count =
            py::to_i64(exclusions.get("rules_configured").unwrap_or(&Value::Null)).unwrap_or(0);
        scope_rows.push(format!(
            "Finding exclusions: {} {} and {} {} suppressed by {} configured URL {}; see Finding Exclusions.",
            count,
            if count == 1 { "finding" } else { "findings" },
            occurrences,
            if occurrences == 1 { "occurrence" } else { "occurrences" },
            rule_count,
            if rule_count == 1 { "rule" } else { "rules" },
        ));
    }

    let scope_format = Format::new()
        .set_bold()
        .set_font_color(rust_xlsxwriter::Color::RGB(HEAD_CRITICAL));
    for (index, text) in scope_rows.iter().enumerate() {
        let row = 3 + index as u32;
        sheet.styles.insert((row, 0), scope_format.clone());
        sheet.rows.push(vec![cell(&Value::String(text.clone()))?]);
    }
    let offset = scope_rows.len() as u32;
    sheet.rows.push(Vec::new()); // Python leaves the row after scope empty

    let metrics: Vec<(&str, Value)> = vec![
        (
            "Pages checked",
            summary.get("pages_checked").cloned().unwrap_or(json!(0)),
        ),
        (
            "Total findings",
            summary.get("findings_total").cloned().unwrap_or(json!(0)),
        ),
        (
            "Critical findings",
            by_sev.get("critical").cloned().unwrap_or(json!(0)),
        ),
        (
            "Warnings",
            by_sev.get("warning").cloned().unwrap_or(json!(0)),
        ),
        ("Notices", by_sev.get("notice").cloned().unwrap_or(json!(0))),
        ("Checks completed", checks_completed_display(summary)),
        (
            "Checks unavailable",
            json!(py::get_list(summary, "tools_failed").len()),
        ),
    ];
    let header_row = 4 + offset; // 0-indexed
    sheet.rows.push(vec![
        Cell::Str("Metric".to_owned()),
        Cell::Str("Value".to_owned()),
    ]);
    style_header(&mut sheet, header_row);
    for (name, value) in &metrics {
        sheet
            .rows
            .push(vec![Cell::Str((*name).to_owned()), cell(value)?]);
    }

    // Severity bar chart over the three severity metric rows.
    let sev_start = header_row + 3; // Critical findings, 0-indexed
    let mut chart = Chart::new(ChartType::Bar);
    chart.title().set_name("Findings by Severity");
    chart.y_axis().set_name("Count");
    chart
        .add_series()
        .set_values(("Summary", sev_start, 1, sev_start + 2, 1))
        .set_categories(("Summary", sev_start, 0, sev_start + 2, 0));
    chart.legend().set_hidden();
    chart.set_width(453).set_height(265);
    sheet.chart = Some((header_row, 3, chart));

    let failed = py::get_list(summary, "tools_failed");
    if !failed.is_empty() {
        let start = header_row + 1 + metrics.len() as u32 + 1;
        while sheet.rows.len() <= start as usize {
            sheet.rows.push(Vec::new());
        }
        sheet.rows[start as usize] = vec![Cell::Str(
            "Unavailable checks -- evidence is absent from report".to_owned(),
        )];
        sheet.styles.insert((start, 0), scope_format.clone());
        for (index, item) in failed.iter().enumerate() {
            let row = start + 1 + index as u32;
            while sheet.rows.len() <= row as usize {
                sheet.rows.push(Vec::new());
            }
            sheet.rows[row as usize] = vec![
                cell(&Value::String(check_title(
                    py::get(item, "tool").unwrap_or(&Value::Null),
                )))?,
                cell(py::get(item, "error").unwrap_or(&Value::Null))?,
            ];
        }
    }
    let note = summary.get("severity_note").unwrap_or(&Value::Null);
    if py::truthy(Some(note)) {
        let row = header_row + 1 + metrics.len() as u32 + failed.len() as u32 + 3;
        while sheet.rows.len() <= row as usize {
            sheet.rows.push(Vec::new());
        }
        sheet.rows[row as usize] = vec![cell(note)?];
        sheet.styles.insert(
            (row, 0),
            Format::new()
                .set_italic()
                .set_font_size(9.0)
                .set_font_color(rust_xlsxwriter::Color::RGB(0x808080)),
        );
    }
    Ok((sheet, exclusions))
}

fn exclusion_sheets(exclusions: &Value) -> Result<Vec<SheetDef>, XlsxReportError> {
    let mut sheets = Vec::new();
    let mut rules_sheet = SheetDef::new("Finding Exclusions");
    rules_sheet.rows.push(
        [
            "Rule",
            "Pattern",
            "Checks",
            "Suppressed findings",
            "Occurrences",
            "Reason",
        ]
        .iter()
        .map(|s| Cell::Str((*s).to_owned()))
        .collect(),
    );
    style_header(&mut rules_sheet, 0);
    for rule in py::get_list(exclusions, "rules") {
        let checks = py::get_list(rule, "checks")
            .iter()
            .map(py::str_value)
            .collect::<Vec<_>>()
            .join(", ");
        rules_sheet.rows.push(vec![
            cell(py::get(rule, "id").unwrap_or(&Value::Null))?,
            cell(py::get(rule, "pattern").unwrap_or(&Value::Null))?,
            cell(&Value::String(if checks.is_empty() {
                "all checks".to_owned()
            } else {
                checks
            }))?,
            cell(py::get(rule, "suppressed_findings").unwrap_or(&Value::Null))?,
            cell(py::get(rule, "suppressed_occurrences").unwrap_or(&Value::Null))?,
            cell(py::get(rule, "reason").unwrap_or(&Value::Null))?,
        ]);
    }
    sheets.push(rules_sheet);

    let suppressed = py::get_list(exclusions, "issues");
    if !suppressed.is_empty() {
        let mut sheet = SheetDef::new("Suppressed Findings");
        sheet.rows.push(
            [
                "Issue ID",
                "Check",
                "Severity",
                "URL",
                "Occurrences",
                "Rule",
                "Reason",
            ]
            .iter()
            .map(|s| Cell::Str((*s).to_owned()))
            .collect(),
        );
        style_header(&mut sheet, 0);
        for issue in suppressed.iter().take(MAX_SUPPRESSED_FINDINGS) {
            let empty = Value::Null;
            let issue = if issue.is_object() { issue } else { &empty };
            let marker = get_obj(issue, "suppression");
            sheet.rows.push(vec![
                cell(issue.get("id").unwrap_or(&Value::Null))?,
                cell(&Value::String(check_title(
                    issue.get("check").unwrap_or(&Value::Null),
                )))?,
                cell(issue.get("severity").unwrap_or(&Value::Null))?,
                cell(issue.get("target_url").unwrap_or(&Value::Null))?,
                cell(issue.get("occurrences_count").unwrap_or(&Value::Null))?,
                cell(marker.get("rule_id").unwrap_or(&Value::Null))?,
                cell(marker.get("reason").unwrap_or(&Value::Null))?,
            ]);
        }
        if suppressed.len() > MAX_SUPPRESSED_FINDINGS {
            sheet.rows.push(vec![Cell::Str(format!(
                "Showing {} of {}; see source audit JSON for all records",
                MAX_SUPPRESSED_FINDINGS,
                suppressed.len()
            ))]);
        }
        sheets.push(sheet);
    }
    Ok(sheets)
}

fn data_sheet(title: &str, titles: &[&str], freeze: bool) -> SheetDef {
    let mut sheet = SheetDef::new(title);
    sheet
        .rows
        .push(titles.iter().map(|t| Cell::Str((*t).to_owned())).collect());
    if freeze {
        style_header(&mut sheet, 0);
    }
    sheet
}

fn technologies_sheet(document: &Value) -> Result<SheetDef, XlsxReportError> {
    let mut sheet = data_sheet(
        "Technologies",
        &["Category", "Detected Technology", "Evidence"],
        true,
    );
    let site = py::get(document, "site").cloned().unwrap_or(Value::Null);
    let tech = py::get(&site, "tech_detect")
        .cloned()
        .unwrap_or(Value::Null);
    for item in py::get_list(&tech, "technologies") {
        sheet.rows.push(vec![
            cell(py::get(item, "category").unwrap_or(&Value::Null))?,
            cell(py::get(item, "name").unwrap_or(&Value::Null))?,
            cell(py::get(item, "evidence").unwrap_or(&Value::Null))?,
        ]);
    }
    let domain_profile = py::get(&site, "domain_profile")
        .cloned()
        .unwrap_or(Value::Null);
    let registration = get_obj(&domain_profile, "registration");
    if !registration.is_empty() {
        sheet.rows.push(Vec::new());
        for field in ["registrar", "created", "expires"] {
            sheet.rows.push(vec![
                Cell::Str("domain".to_owned()),
                Cell::Str(field.to_owned()),
                cell(registration.get(field).unwrap_or(&Value::Null))?,
            ]);
        }
        sheet.rows.push(vec![
            Cell::Str("domain".to_owned()),
            Cell::Str("age in years".to_owned()),
            cell(registration.get("age_years").unwrap_or(&Value::Null))?,
        ]);
    }
    Ok(sheet)
}

fn coverage_sheet(summary: &Value) -> Result<Option<SheetDef>, XlsxReportError> {
    let coverage = py::get(summary, "project_coverage")
        .cloned()
        .unwrap_or(Value::Null);
    if !coverage.is_object() {
        return Ok(None);
    }
    let project = py::get(&coverage, "project")
        .cloned()
        .unwrap_or(Value::Null);
    let checklist = py::get(&coverage, "status").cloned().unwrap_or(Value::Null);
    let mut sheet = SheetDef::new("Project Coverage");
    for (label, value) in [
        (
            "Project",
            project.get("site").cloned().unwrap_or(Value::Null),
        ),
        (
            "Project UUID",
            project.get("uuid").cloned().unwrap_or(Value::Null),
        ),
        (
            "Checklist state",
            checklist.get("state").cloned().unwrap_or(Value::Null),
        ),
        (
            "Revision",
            checklist.get("revision").cloned().unwrap_or(Value::Null),
        ),
        ("Counts", Value::String(value_text(checklist.get("counts")))),
        (
            "Task KPIs",
            Value::String(value_text(checklist.get("task_kpis"))),
        ),
    ] {
        sheet
            .rows
            .push(vec![Cell::Str(label.to_owned()), cell(&value)?]);
    }
    sheet.rows.push(Vec::new());
    sheet.rows.push(
        [
            "Item ID",
            "Item",
            "Kind",
            "Execution",
            "Priority",
            "Priority origin",
            "Priority reason",
            "State",
            "Attempt",
            "Complete",
            "Blocked by",
            "Enabled",
            "Stale",
            "Scope",
            "Measurement",
            "Task assignee",
            "Task description",
            "Acceptance criterion",
            "Affected URLs",
            "Affected URL state",
            "Reason",
        ]
        .iter()
        .map(|s| Cell::Str((*s).to_owned()))
        .collect(),
    );
    style_header(&mut sheet, 7);
    for item in py::get_list(&checklist, "items") {
        if !item.is_object() {
            continue;
        }
        let task = py::get(item, "task").cloned().unwrap_or(Value::Null);
        let affected = py::get(item, "affected_urls")
            .cloned()
            .unwrap_or(Value::Null);
        sheet.rows.push(vec![
            cell(item.get("id").unwrap_or(&Value::Null))?,
            cell(item.get("title").unwrap_or(&Value::Null))?,
            cell(item.get("kind").unwrap_or(&Value::Null))?,
            cell(item.get("execution_kind").unwrap_or(&Value::Null))?,
            cell(item.get("priority").unwrap_or(&Value::Null))?,
            cell(item.get("priority_origin").unwrap_or(&Value::Null))?,
            cell(item.get("priority_reason").unwrap_or(&Value::Null))?,
            cell(item.get("state").unwrap_or(&Value::Null))?,
            cell(item.get("attempt_status").unwrap_or(&Value::Null))?,
            cell(item.get("complete").unwrap_or(&Value::Null))?,
            cell(&Value::String(value_text(item.get("blocked_by"))))?,
            cell(item.get("enabled").unwrap_or(&Value::Null))?,
            cell(item.get("stale").unwrap_or(&Value::Null))?,
            cell(&Value::String(value_text(item.get("scope"))))?,
            cell(&Value::String(value_text(item.get("measurement"))))?,
            cell(&Value::String(value_text(task.get("assignee"))))?,
            cell(&Value::String(value_text(task.get("description"))))?,
            cell(&Value::String(value_text(task.get("acceptance_criterion"))))?,
            cell(affected.get("count").unwrap_or(&Value::Null))?,
            cell(&Value::String(
                affected
                    .get("state")
                    .and_then(Value::as_str)
                    .unwrap_or("unavailable")
                    .to_owned(),
            ))?,
            cell(&Value::String(
                item.get("reason")
                    .or_else(|| checklist.get("reason"))
                    .map(py::str_value)
                    .unwrap_or_default(),
            ))?,
        ]);
    }
    if sheet.rows.len() > 8 {
        sheet.autofilter = Some((7, sheet.rows.len() as u32 - 1, 20));
    }
    Ok(Some(sheet))
}

fn evidence_sheet(summary: &Value) -> Result<Option<SheetDef>, XlsxReportError> {
    let rows = evidence_summary::rows(summary);
    if rows.is_empty() {
        return Ok(None);
    }
    let mut sheet = data_sheet(
        "Evidence coverage",
        &["Kind", "Measurement", "State", "Scope or reason"],
        true,
    );
    for row in &rows {
        sheet.rows.push(
            row.iter()
                .map(|v| cell(&Value::String(v.clone())))
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    let last = sheet.rows.len() as u32 - 1;
    sheet.autofilter = Some((0, last, 3));
    Ok(Some(sheet))
}

/// Replayable row source for the two unbounded tables.
fn data_rows(
    title: &str,
    document: &dyn ReportDoc,
    view_columns: &Option<Vec<String>>,
) -> Vec<(Vec<Value>, Option<u32>)> {
    if title == "Findings" {
        return document
            .findings(None)
            .iter()
            .map(|finding| {
                if let Some(columns) = view_columns {
                    let fields = py::get(finding, "view_fields")
                        .cloned()
                        .unwrap_or(Value::Null);
                    (
                        columns
                            .iter()
                            .map(|column| {
                                neutralize_value(
                                    fields.get(column).unwrap_or(&Value::String(String::new())),
                                )
                            })
                            .collect(),
                        None,
                    )
                } else {
                    let details = py::get_list(finding, "client_details")
                        .iter()
                        .map(py::str_value)
                        .collect::<Vec<_>>()
                        .join("; ");
                    let locations = py::get_list(finding, "client_locations")
                        .iter()
                        .map(py::str_value)
                        .collect::<Vec<_>>()
                        .join("; ");
                    let severity = py::get(finding, "severity")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    (
                        vec![
                            Value::String(severity_title(severity).to_owned()),
                            neutralize_value(finding.get("url").unwrap_or(&Value::Null)),
                            neutralize_value(
                                finding
                                    .get("client_title")
                                    .unwrap_or(&Value::String("Audit finding".to_owned())),
                            ),
                            neutralize_value(
                                finding.get("client_observation").unwrap_or(&Value::Null),
                            ),
                            neutralize_value(
                                finding.get("client_reproduction").unwrap_or(&Value::Null),
                            ),
                            finding.get("status_code").cloned().unwrap_or(Value::Null),
                            finding
                                .get("occurrences_count")
                                .cloned()
                                .unwrap_or(Value::Null),
                            neutralize_value(&Value::String(details)),
                            neutralize_value(&Value::String(locations)),
                            neutralize_value(finding.get("fix_hint").unwrap_or(&Value::Null)),
                        ],
                        severity_color(severity),
                    )
                }
            })
            .collect();
    }
    document
        .pages()
        .iter()
        .map(|page| {
            (
                PAGE_COLUMNS
                    .iter()
                    .map(|column| neutralize_value(page.get(*column).unwrap_or(&Value::Null)))
                    .collect(),
                None,
            )
        })
        .collect()
}

fn free_disk_bytes(directory: &Path) -> u64 {
    #[cfg(unix)]
    {
        spiderhead_core::filesystem::free_space(directory).unwrap_or(u64::MAX)
    }
    #[cfg(not(unix))]
    {
        let _ = directory;
        u64::MAX
    }
}

fn check_disk_free(path: &Path) -> Result<(), XlsxReportError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if free_disk_bytes(parent) < MIN_FREE_DISK_BYTES {
        return Err(XlsxReportError::Io(io::Error::other(
            "insufficient free disk for XLSX output; use complete CSV or JSON",
        )));
    }
    Ok(())
}

fn write_cells(
    sheet: &mut Worksheet,
    row: u32,
    cells: &[Cell],
    styles: Option<&HashMap<(u32, u16), Format>>,
    severity_col: Option<u16>,
    color: Option<u32>,
) -> Result<(), XlsxReportError> {
    let color_format = color.map(|c| {
        Format::new()
            .set_bold()
            .set_font_color(rust_xlsxwriter::Color::RGB(c))
    });
    for (col, value) in cells.iter().enumerate() {
        let col = col as u16;
        let format = styles
            .and_then(|s| s.get(&(row, col)))
            .or(if severity_col == Some(col) {
                color_format.as_ref()
            } else {
                None
            });
        match (value, format) {
            (Cell::Blank, Some(f)) => {
                sheet.write_blank(row, col, f)?;
            }
            (Cell::Blank, None) => {}
            (Cell::Str(s), Some(f)) => {
                sheet.write_string_with_format(row, col, s, f)?;
            }
            (Cell::Str(s), None) => {
                sheet.write_string(row, col, s)?;
            }
            (Cell::Num(n), Some(f)) => {
                sheet.write_number_with_format(row, col, *n, f)?;
            }
            (Cell::Num(n), None) => {
                sheet.write_number(row, col, *n)?;
            }
            (Cell::Bool(b), Some(f)) => {
                sheet.write_boolean_with_format(row, col, *b, f)?;
            }
            (Cell::Bool(b), None) => {
                sheet.write_boolean(row, col, *b)?;
            }
        }
    }
    Ok(())
}

/// Column widths for the static part of a sheet, or measured data widths.
fn sheet_widths(
    defs: &SheetDef,
    title: &str,
    view_columns: &Option<Vec<String>>,
    data_rows: &[(Vec<Value>, Option<u32>)],
    is_data: bool,
) -> Result<Vec<f64>, XlsxReportError> {
    let limits: Vec<(usize, f64)> = match title {
        "Summary" => vec![(2, 40.0)],
        "Findings" => {
            if view_columns.is_none() {
                vec![(4, 100.0), (8, 100.0), (9, 60.0)]
            } else {
                Vec::new()
            }
        }
        "Pages" => vec![(1, 70.0), (3, 60.0), (6, 40.0)],
        "Technologies" => vec![(3, 70.0)],
        "Project Coverage" => vec![
            (2, 45.0),
            (7, 60.0),
            (14, 70.0),
            (15, 70.0),
            (16, 45.0),
            (17, 60.0),
            (18, 60.0),
            (19, 60.0),
            (21, 70.0),
        ],
        "Evidence coverage" => vec![(2, 80.0), (4, 100.0)],
        _ => Vec::new(),
    };
    if is_data {
        let mut widths = vec![0usize; defs.rows.first().map(Vec::len).unwrap_or(0)];
        for (values, _color) in data_rows {
            for (index, value) in values.iter().enumerate() {
                if index >= widths.len() {
                    break;
                }
                widths[index] = widths[index].max(cell(value)?.width_len());
            }
        }
        return Ok(widths
            .iter()
            .enumerate()
            .map(|(index, longest)| {
                let cap = limits
                    .iter()
                    .find(|(col, _)| *col == index + 1)
                    .map(|(_, cap)| *cap)
                    .unwrap_or(60.0);
                ((longest + 2).max(10) as f64).min(cap)
            })
            .collect());
    }
    Ok(autofit(&defs.rows, &limits))
}

/// `write`/`write_stream`: build the workbook and publish it atomically.
pub fn write(document: &dyn ReportDoc, path: &Path) -> Result<Value, XlsxReportError> {
    let path = path.to_path_buf();
    let index_path = index_path_for(&path);
    for candidate in [&path, &index_path] {
        if let Ok(meta) = fs::symlink_metadata(candidate) {
            if meta.file_type().is_symlink() || !meta.is_file() {
                return Err(XlsxReportError::Io(io::Error::other(format!(
                    "XLSX output and index must be regular files: {}",
                    candidate.display()
                ))));
            }
        }
    }

    // `_metadata_workbook` consumes only the bounded fields.
    let meta_document = {
        let mut object = Map::new();
        for key in ["domain", "url", "generated_at", "summary", "site"] {
            object.insert(key.to_owned(), document.get(key).unwrap_or(Value::Null));
        }
        // `get("suppressed_issues")` returns the lazy collection marker on a
        // streamed audit.v2 view; the trait resolves rows for both impls.
        object.insert(
            "suppressed_issues".to_owned(),
            Value::Array(document.suppressed_issues()),
        );
        Value::Object(object)
    };
    let summary = py::get(&meta_document, "summary")
        .cloned()
        .unwrap_or(Value::Null);
    let view_columns = finding_view_columns(&summary);
    let severity_column: Option<u16> = match &view_columns {
        Some(columns) => columns
            .iter()
            .position(|c| c == "severity")
            .map(|p| p as u16),
        None => Some(0),
    };

    let mut sheet_defs: Vec<SheetDef> = Vec::new();
    let (summary_sheet, exclusions) = build_summary(&meta_document)?;
    sheet_defs.push(summary_sheet);
    if let Some(exclusions) = exclusions.as_ref() {
        sheet_defs.extend(exclusion_sheets(exclusions)?);
    }
    let findings_titles: Vec<String> = match &view_columns {
        Some(columns) => columns.iter().map(|c| finding_view_label(c)).collect(),
        None => [
            "Severity",
            "URL",
            "Finding",
            "Observation",
            "Reproduction",
            "Status",
            "Occurrences",
            "Evidence",
            "Locations",
            "Fix Hint",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect(),
    };
    sheet_defs.push(data_sheet(
        "Findings",
        &findings_titles
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        true,
    ));
    sheet_defs.push(data_sheet(
        "Pages",
        &[
            "URL",
            "Status",
            "Title",
            "Title Length",
            "Description Length",
            "H1",
            "Canonical",
            "Words",
            "Schema Types",
            "Schema Errors",
            "Missing Social Tags",
        ],
        true,
    ));
    sheet_defs.push(technologies_sheet(&meta_document)?);
    if let Some(sheet) = coverage_sheet(&summary)? {
        sheet_defs.push(sheet);
    }
    if let Some(sheet) = evidence_sheet(&summary)? {
        sheet_defs.push(sheet);
    }

    check_disk_free(&path)?;
    let mut workbook = Workbook::new();
    let mut ranges: Vec<Value> = Vec::new();
    let mut total_sheets = 0usize;

    for defs in &sheet_defs {
        let is_data = defs.title == "Findings" || defs.title == "Pages";
        let rows = if is_data {
            data_rows(&defs.title, document, &view_columns)
        } else {
            Vec::new()
        };
        let count = rows.len();
        if !is_data && defs.rows.len() > EXCEL_MAX_ROWS {
            return Err(XlsxReportError::Value(
                "XLSX metadata exceeds Excel's row limit; use complete CSV or JSON".to_owned(),
            ));
        }
        let parts = if is_data {
            count.div_ceil(MAX_DATA_ROWS).max(1)
        } else {
            1
        };
        if total_sheets + parts > MAX_WORKSHEETS {
            return Err(XlsxReportError::Value(
                "XLSX worksheet count exceeds its bound; use complete CSV or JSON".to_owned(),
            ));
        }
        let widths = sheet_widths(defs, &defs.title, &view_columns, &rows, is_data)?;

        let mut written = 0usize;
        for part in 0..parts {
            let title = if part == 0 {
                defs.title.clone()
            } else {
                format!("{} {}", defs.title, part + 1)
            };
            let sheet = if is_data {
                workbook.add_worksheet_with_constant_memory()?
            } else {
                workbook.add_worksheet()
            };
            sheet.set_name(&title)?;
            for (index, width) in widths.iter().enumerate() {
                sheet.set_column_width(index as u16, *width)?;
            }
            if let Some(freeze) = defs.freeze_row {
                sheet.set_freeze_panes(freeze, 0)?;
            }
            let part_rows = (count - written).min(MAX_DATA_ROWS);
            if is_data {
                if part_rows > 0 {
                    sheet.autofilter(0, 0, part_rows as u32, widths.len() as u16 - 1)?;
                }
                ranges.push(json!({
                    "table": defs.title,
                    "worksheet": title,
                    "first_record": if part_rows > 0 { json!(written + 1) } else { Value::Null },
                    "last_record": if part_rows > 0 { json!(written + part_rows) } else { Value::Null },
                    "rows": part_rows,
                    "header_rows": 1,
                }));
            } else if let Some((first, last, last_col)) = defs.autofilter {
                sheet.autofilter(first, 0, last, last_col)?;
            }
            for (row_index, cells) in defs.rows.iter().enumerate() {
                write_cells(
                    sheet,
                    row_index as u32,
                    cells,
                    Some(&defs.styles),
                    None,
                    None,
                )?;
            }
            if let Some((anchor_row, anchor_col, chart)) = &defs.chart {
                sheet.insert_chart(*anchor_row, *anchor_col, chart)?;
            }
            let header_len = defs.rows.len() as u32;
            for (offset, (values, color)) in rows.iter().skip(written).take(part_rows).enumerate() {
                let cells: Vec<Cell> = values.iter().map(cell).collect::<Result<Vec<_>, _>>()?;
                write_cells(
                    sheet,
                    header_len + offset as u32,
                    &cells,
                    None,
                    if defs.title == "Findings" {
                        severity_column
                    } else {
                        None
                    },
                    *color,
                )?;
                written += 1;
            }
            total_sheets += 1;
        }
        if written != count {
            return Err(XlsxReportError::Value(
                "XLSX row conservation failed; source rows must be replayable".to_owned(),
            ));
        }
    }

    check_disk_free(&path)?;
    let stage = Stage::new(path.parent().unwrap_or_else(|| Path::new(".")))?;
    let staged = stage.dir().join("report.xlsx");
    workbook.save(&staged)?;
    let staged_size = fs::metadata(&staged)?.len();
    if staged_size > DEFAULT_MAX_OUTPUT_BYTES {
        return Err(XlsxReportError::Value(
            "XLSX exceeds its byte bound; use complete CSV or JSON output".to_owned(),
        ));
    }
    let digest = sha256_file(&staged)?;
    let suppressed = document.suppressed_issues();
    let coverage_worksheets: Vec<String> = sheet_defs
        .iter()
        .map(|d| d.title.clone())
        .filter(|title| {
            !ranges
                .iter()
                .any(|r| r.get("worksheet").and_then(Value::as_str) == Some(title.as_str()))
        })
        .collect();
    let index = json!({
        "format": "spiderhead.report-xlsx-index.v1",
        "state": "complete",
        "workbook": path.file_name().and_then(|n| n.to_str()).unwrap_or(""),
        "workbook_sha256": digest,
        "workbook_bytes": staged_size,
        "ranges": ranges,
        "finding_view": summary.get("finding_view").cloned().unwrap_or(Value::Null),
        "source_population": {
            "findings": summary.get("findings_total").cloned().unwrap_or(Value::Null),
            "pages": summary.get("pages_checked").cloned().unwrap_or(Value::Null),
        },
        "coverage_worksheets": coverage_worksheets,
        "omissions": {
            "suppressed_findings": suppressed.len().saturating_sub(MAX_SUPPRESSED_FINDINGS),
            "fallback": "Complete CSV scope output or source JSON",
        },
        "limits": {
            "data_rows_per_sheet": MAX_DATA_ROWS,
            "cell_characters": EXCEL_MAX_CELL_CHARS,
            "max_output_bytes": DEFAULT_MAX_OUTPUT_BYTES,
        },
    });
    let staged_index = stage.dir().join("index.json");
    let mut index_text =
        serde_json::to_string_pretty(&index).map_err(|e| XlsxReportError::Value(e.to_string()))?;
    index_text.push('\n');
    fs::write(&staged_index, index_text)?;
    publish(
        &stage,
        &[(staged, path.clone()), (staged_index, index_path.clone())],
    )?;
    Ok(json!({
        "index": index_path.to_string_lossy(),
        "worksheets": total_sheets,
    }))
}

/// Alias for [`write`], kept for callers that use the streaming name.
pub fn write_stream(document: &dyn ReportDoc, path: &Path) -> Result<Value, XlsxReportError> {
    write(document, path)
}

/// `path.with_suffix(path.suffix + ".index.json")` — appends `.index.json`.
fn index_path_for(path: &Path) -> PathBuf {
    let file = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    path.with_file_name(format!("{file}.index.json"))
}

fn sha256_file(path: &Path) -> Result<String, XlsxReportError> {
    use sha2::Digest;
    let mut file = fs::File::open(path)?;
    let mut digest = sha2::Sha256::new();
    io::copy(&mut file, &mut digest)?;
    Ok(format!("{:x}", digest.finalize()))
}

/// Temporary directory guard that removes itself on drop.
struct Stage(PathBuf);

impl Stage {
    fn new(parent: &Path) -> Result<Stage, XlsxReportError> {
        for attempt in 0..100u32 {
            let dir = parent.join(format!(".seohead-xlsx-{}-{}", std::process::id(), attempt));
            match fs::create_dir(&dir) {
                Ok(()) => return Ok(Stage(dir)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err(XlsxReportError::Io(io::Error::other(
            "could not create XLSX staging directory",
        )))
    }

    fn dir(&self) -> &Path {
        &self.0
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Replace destinations with staged files; roll back on partial failure.
fn publish(stage: &Stage, publications: &[(PathBuf, PathBuf)]) -> Result<(), XlsxReportError> {
    let mut backups: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (position, (_new, destination)) in publications.iter().enumerate() {
        if destination.exists() {
            let backup = stage.dir().join(format!("previous-{position}"));
            fs::hard_link(destination, &backup)?;
            backups.push((destination.clone(), backup));
        }
    }
    let mut published: Vec<PathBuf> = Vec::new();
    for (new, destination) in publications {
        if let Err(error) = fs::rename(new, destination) {
            for destination in published.iter().rev() {
                if let Some((_, backup)) = backups.iter().find(|(d, _)| d == destination) {
                    let _ = fs::rename(backup, destination);
                } else {
                    let _ = fs::remove_file(destination);
                }
            }
            return Err(error.into());
        }
        published.push(destination.clone());
    }
    Ok(())
}

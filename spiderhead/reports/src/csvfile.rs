//! Flat CSV records for a task tracker or downstream database.
//!
//! One file represents one entity, so the renderer writes adjacent files:
//! `<name>.csv` findings, `<name>.pages.csv` page facts, `<name>.scope.csv`
//! run-evidence caveats, and `<name>.coverage.csv` when project coverage is attached.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
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

/// Failure writing the CSV artifacts.
#[derive(Debug, Error)]
pub enum CsvError {
    /// csv/zip machinery failed.
    #[error("csv write failed: {0}")]
    Csv(#[from] csv::Error),
    /// Filesystem write failed.
    #[error("io failed: {0}")]
    Io(#[from] std::io::Error),
}

/// File factory: callers inject their own sink (for example the PDF overview's
/// budgeted writer) through [`write_with`].
pub type SinkFactory<'a> = dyn FnMut(&Path) -> Result<Box<dyn Write>, CsvError> + 'a;

fn default_sink(path: &Path) -> Result<Box<dyn Write>, CsvError> {
    Ok(Box::new(std::fs::File::create(path)?))
}

/// BOM (`utf-8-sig`) so Excel detects UTF-8, then the shared `;`/CRLF dialect.
fn open_csv<W: Write>(mut sink: W) -> Result<csv::Writer<W>, CsvError> {
    sink.write_all(b"\xef\xbb\xbf")?;
    Ok(csv::WriterBuilder::new()
        .delimiter(b';')
        .terminator(csv::Terminator::CRLF)
        .from_writer(sink))
}

fn scope_rows(summary: &Value, suppressed_issues: Option<&[Value]>) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = evidence_summary::rows(summary);
    if summary.get("crawl_valid") == Some(&Value::Bool(false)) {
        rows.push(vec![
            "crawl".into(),
            "validity".into(),
            "failed".into(),
            py::get(summary, "crawl_invalid_reason")
                .filter(|v| py::truthy(Some(*v)))
                .map(py::str_value)
                .unwrap_or_else(|| "the crawl produced no usable data".into()),
        ]);
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
        rows.push(vec![
            "crawl".into(),
            "scope".into(),
            "partial".into(),
            bits.join("; "),
        ]);
    }
    for item in py::get_list(summary, "checks_disabled") {
        rows.push(vec![
            "check".into(),
            check_title(py::get(item, "id").unwrap_or(&Value::Null)),
            "disabled".into(),
            py::str_value(py::get(item, "reason").unwrap_or(&Value::Null)),
        ]);
    }
    for item in py::get_list(summary, "tools_failed") {
        rows.push(vec![
            "check".into(),
            check_title(py::get(item, "tool").unwrap_or(&Value::Null)),
            "unavailable".into(),
            py::str_value(py::get(item, "error").unwrap_or(&Value::Null)),
        ]);
    }
    if let Some(notice) = finding_view_notice(summary) {
        let view = py::get_map(summary, "finding_view");
        rows.push(vec![
            "finding view".into(),
            py::str_value(view.get("name").unwrap_or(&Value::Null)),
            view.get("state")
                .map(py::str_value)
                .unwrap_or_else(|| "measured".into()),
            notice,
        ]);
    }
    let exclusions = finding_exclusion_report(summary, suppressed_issues);
    if let Some(exclusions) = &exclusions {
        let count = py::get(exclusions, "suppressed_total")
            .and_then(py::to_i64)
            .unwrap_or(0);
        let finding_label = if count == 1 { "finding" } else { "findings" };
        let occurrences = py::get(exclusions, "suppressed_occurrences")
            .and_then(py::to_i64)
            .unwrap_or(0);
        let occurrence_label = if occurrences == 1 {
            "occurrence"
        } else {
            "occurrences"
        };
        let rule_count = py::get(exclusions, "rules_configured")
            .and_then(py::to_i64)
            .unwrap_or(0);
        let rule_label = if rule_count == 1 { "rule" } else { "rules" };
        rows.push(vec![
            "finding exclusions".into(),
            "source audit".into(),
            "recorded".into(),
            format!(
                "{count} {finding_label} suppressed across {rule_count} configured URL \
                 {rule_label} ({occurrences} {occurrence_label})"
            ),
        ]);
        for rule in py::get_list(exclusions, "rules") {
            let rule_count = py::get(rule, "suppressed_findings")
                .and_then(py::to_i64)
                .unwrap_or(0);
            let rule_occurrences = py::get(rule, "suppressed_occurrences")
                .and_then(py::to_i64)
                .unwrap_or(0);
            let rule_finding_label = if rule_count == 1 {
                "finding"
            } else {
                "findings"
            };
            let rule_occurrence_label = if rule_occurrences == 1 {
                "occurrence"
            } else {
                "occurrences"
            };
            let checks = py::get_list(rule, "checks")
                .iter()
                .map(py::str_value)
                .collect::<Vec<_>>()
                .join(", ");
            let checks = if checks.is_empty() {
                "all checks".to_owned()
            } else {
                checks
            };
            rows.push(vec![
                "finding exclusion rule".into(),
                py::str_value(py::get(rule, "id").unwrap_or(&Value::Null)),
                format!("{rule_count} {rule_finding_label} / {rule_occurrences} {rule_occurrence_label}"),
                format!(
                    "Pattern: {}; checks: {}; reason: {}",
                    py::str_value(py::get(rule, "pattern").unwrap_or(&Value::Null)),
                    checks,
                    py::str_value(py::get(rule, "reason").unwrap_or(&Value::Null))
                ),
            ]);
        }
    }
    if let Some(exclusions) = &exclusions {
        for issue in py::get_list(exclusions, "issues") {
            let empty = Value::Object(Default::default());
            let issue = if issue.is_object() { issue } else { &empty };
            let marker = py::get_map(issue, "suppression");
            let details = [
                format!(
                    "Check: {}",
                    check_title(py::get(issue, "check").unwrap_or(&Value::Null))
                ),
                format!(
                    "Severity: {}",
                    py::str_value(py::get(issue, "severity").unwrap_or(&Value::Null))
                ),
                format!(
                    "URL: {}",
                    py::str_value(py::get(issue, "target_url").unwrap_or(&Value::Null))
                ),
                format!(
                    "Pattern: {}",
                    py::str_value(marker.get("pattern").unwrap_or(&Value::Null))
                ),
                format!(
                    "Reason: {}",
                    py::str_value(marker.get("reason").unwrap_or(&Value::Null))
                ),
            ];
            rows.push(vec![
                "suppressed finding".into(),
                py::get(issue, "id")
                    .filter(|v| py::truthy(Some(*v)))
                    .map(py::str_value)
                    .unwrap_or_else(|| {
                        py::str_value(py::get(issue, "check").unwrap_or(&Value::Null))
                    }),
                "excluded".into(),
                details.join("; "),
            ]);
        }
    }
    rows
}

fn write_project_coverage(
    summary: &Value,
    path: &Path,
    sink: &mut SinkFactory<'_>,
) -> Result<Option<usize>, CsvError> {
    let coverage = py::get(summary, "project_coverage").filter(|v| v.is_object());
    let Some(coverage) = coverage else {
        return Ok(None);
    };
    let project = py::get_map(coverage, "project");
    let status = py::get_map(coverage, "status");
    let destination = with_suffix(path, ".coverage.csv");
    let mut writer = open_csv(sink(&destination)?)?;
    writer.write_record([
        "Project UUID",
        "Project site",
        "Checklist revision",
        "Checklist state",
        "Counts",
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
        "Reason",
    ])?;
    let status_v = Value::Object(status.clone());
    let items = py::get_list(&status_v, "items");
    let items: Vec<&Value> = if items.is_empty() {
        vec![&Value::Null]
    } else {
        items.iter().collect()
    };
    let coverage_rows = items.len();
    for item in items {
        writer.write_record([
            neutralize_formula(&py::str_value(project.get("uuid").unwrap_or(&Value::Null))),
            neutralize_formula(&py::str_value(project.get("site").unwrap_or(&Value::Null))),
            py::str_value(status.get("revision").unwrap_or(&Value::Null)),
            neutralize_formula(&py::str_value(status.get("state").unwrap_or(&Value::Null))),
            neutralize_formula(&value_text(status.get("counts"))),
            neutralize_formula(&py::str_value(py::get(item, "id").unwrap_or(&Value::Null))),
            neutralize_formula(&py::str_value(
                py::get(item, "title").unwrap_or(&Value::Null),
            )),
            neutralize_formula(&py::str_value(
                py::get(item, "kind").unwrap_or(&Value::Null),
            )),
            neutralize_formula(&py::str_value(
                py::get(item, "execution_kind").unwrap_or(&Value::Null),
            )),
            neutralize_formula(&py::str_value(
                py::get(item, "priority").unwrap_or(&Value::Null),
            )),
            neutralize_formula(&py::str_value(
                py::get(item, "priority_origin").unwrap_or(&Value::Null),
            )),
            neutralize_formula(&py::str_value(
                py::get(item, "priority_reason").unwrap_or(&Value::Null),
            )),
            neutralize_formula(&py::str_value(
                py::get(item, "state").unwrap_or(&Value::Null),
            )),
            neutralize_formula(&py::str_value(
                py::get(item, "attempt_status").unwrap_or(&Value::Null),
            )),
            py::str_value(py::get(item, "complete").unwrap_or(&Value::Null)),
            neutralize_formula(&value_text(py::get(item, "blocked_by"))),
            py::str_value(py::get(item, "enabled").unwrap_or(&Value::Null)),
            py::str_value(py::get(item, "stale").unwrap_or(&Value::Null)),
            neutralize_formula(&value_text(py::get(item, "scope"))),
            neutralize_formula(&value_text(py::get(item, "measurement"))),
            neutralize_formula(
                &py::get(item, "reason")
                    .filter(|v| !v.is_null())
                    .map(py::str_value)
                    .unwrap_or_else(|| py::str_value(status.get("reason").unwrap_or(&Value::Null))),
            ),
        ])?;
    }
    writer.flush()?;
    Ok(Some(coverage_rows))
}

/// `path.with_suffix(".scope.csv")`: `report.csv` -> `report.scope.csv`.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    path.with_extension(suffix.trim_start_matches('.'))
}

/// Data-row counts per emitted file (excluding the header row), in the order
/// the files are written: findings, scope, pages, then optional coverage.
#[derive(Debug, Default)]
pub struct CsvFileRows {
    /// Rows in the main findings CSV.
    pub findings: usize,
    /// Rows in the `.scope.csv` companion.
    pub scope: usize,
    /// Rows in the `.pages.csv` companion.
    pub pages: usize,
    /// Rows in the optional `.coverage.csv` companion.
    pub coverage: Option<usize>,
}

/// Write `<name>.csv` + `.scope.csv` + `.pages.csv` (+ `.coverage.csv`).
pub fn write(document: &dyn ReportDoc, path: &Path) -> Result<(), CsvError> {
    write_with(document, path, &mut default_sink).map(|_| ())
}

/// Like [`write`], but every file is written through the caller-supplied `sink`.
/// The per-file data-row counts are returned.
pub fn write_with(
    document: &dyn ReportDoc,
    path: &Path,
    sink: &mut SinkFactory<'_>,
) -> Result<CsvFileRows, CsvError> {
    let summary = document.get("summary").unwrap_or(json!({}));
    let mut rows = CsvFileRows::default();

    {
        let mut writer = open_csv(sink(path)?)?;
        let view_columns = finding_view_columns(&summary);
        if let Some(columns) = view_columns {
            writer.write_record(columns.iter().map(|c| finding_view_label(c)))?;
            for finding in document.findings(None) {
                let values = py::get_map(&finding, "view_fields");
                writer.write_record(columns.iter().map(|column| {
                    neutralize_formula(&py::str_value(values.get(column).unwrap_or(&Value::Null)))
                }))?;
                rows.findings += 1;
            }
        } else {
            writer.write_record([
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
            ])?;
            for finding in document.findings(None) {
                let severity = py::str_value(py::get(&finding, "severity").unwrap_or(&Value::Null));
                let title = crate::SEVERITY_TITLES
                    .get(severity.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or(severity);
                let join = |key: &str| {
                    py::get_list(&finding, key)
                        .iter()
                        .map(py::str_value)
                        .collect::<Vec<_>>()
                        .join("; ")
                };
                writer.write_record([
                    title,
                    neutralize_formula(
                        &py::get(&finding, "url")
                            .map(py::str_value)
                            .unwrap_or_default(),
                    ),
                    neutralize_formula(
                        &py::get(&finding, "client_title")
                            .map(py::str_value)
                            .unwrap_or_else(|| "Audit finding".into()),
                    ),
                    neutralize_formula(
                        &py::get(&finding, "client_observation")
                            .map(py::str_value)
                            .unwrap_or_default(),
                    ),
                    neutralize_formula(
                        &py::get(&finding, "client_reproduction")
                            .map(py::str_value)
                            .unwrap_or_default(),
                    ),
                    py::str_value(py::get(&finding, "status_code").unwrap_or(&Value::Null)),
                    py::str_value(py::get(&finding, "occurrences_count").unwrap_or(&Value::Null)),
                    neutralize_formula(&join("client_details")),
                    neutralize_formula(&join("client_locations")),
                    neutralize_formula(
                        &py::get(&finding, "fix_hint")
                            .map(py::str_value)
                            .unwrap_or_default(),
                    ),
                ])?;
                rows.findings += 1;
            }
        }
        writer.flush()?;
    }

    let scope = scope_rows(&summary, Some(&document.suppressed_issues()));
    {
        let mut writer = open_csv(sink(&with_suffix(path, ".scope.csv"))?)?;
        writer.write_record(["Evidence type", "Identifier", "Status", "Reason"])?;
        for row in &scope {
            writer.write_record(row.iter().map(|value| neutralize_formula(value)))?;
            rows.scope += 1;
        }
        writer.flush()?;
    }

    const COLUMNS: [&str; 11] = [
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
    {
        let mut writer = open_csv(sink(&with_suffix(path, ".pages.csv"))?)?;
        writer.write_record(COLUMNS)?;
        for page in document.pages() {
            writer.write_record(COLUMNS.iter().map(|c| {
                neutralize_formula(&py::str_value(py::get(&page, c).unwrap_or(&Value::Null)))
            }))?;
            rows.pages += 1;
        }
        writer.flush()?;
    }

    rows.coverage = write_project_coverage(&summary, path, sink)?;
    Ok(rows)
}

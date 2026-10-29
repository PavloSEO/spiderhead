//! Portable Markdown report for editors and version control.

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

const MAX_SUPPRESSED_ROWS: usize = 100;

/// Failure writing the Markdown artifact.
#[derive(Debug, Error)]
#[error("io failed: {0}")]
pub struct MdError(#[from] std::io::Error);

fn field(value: Option<&Value>, limit: Option<usize>) -> String {
    let text = value.map(py::str_value).unwrap_or_default();
    let escaped = text.replace('|', "\\|");
    match limit {
        Some(limit) => escaped.chars().take(limit).collect(),
        None => escaped,
    }
}

fn coverage_field(value: Option<&Value>) -> String {
    field(value, None).replace(['\r', '\n'], " ")
}

/// Emit `document` as Markdown to `writer`.
pub fn render(document: &dyn ReportDoc, out: &mut impl Write) -> Result<(), MdError> {
    let mut lines = Lines {
        stream: out,
        first: true,
    };
    let summary = document.get("summary").unwrap_or(json!({}));
    let empty = serde_json::Map::new();
    let by_sev = py::get(&summary, "findings_by_severity")
        .and_then(Value::as_object)
        .unwrap_or(&empty);

    lines.extend([
        format!(
            "# SEO Audit: {}",
            py::str_value(&document.get("domain").unwrap_or(Value::Null))
        ),
        String::new(),
        format!(
            "{} · Generated {}",
            py::str_value(&document.get("url").unwrap_or(Value::Null)),
            py::str_value(&document.get("generated_at").unwrap_or(Value::Null))
        ),
        String::new(),
    ]);

    if summary.get("crawl_valid") == Some(&Value::Bool(false)) {
        let reason = py::get(&summary, "crawl_invalid_reason")
            .filter(|v| py::truthy(Some(*v)))
            .map(py::str_value)
            .unwrap_or_else(|| "the crawl produced no usable data".into());
        lines.extend([
            format!("> **Crawl failed — no health score.** {reason}"),
            String::new(),
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
        let detail = if bits.is_empty() {
            String::new()
        } else {
            format!(" {}", bits.join("; "))
        };
        lines.extend([
            format!("> **Partial crawl — scope is limited.**{detail}"),
            String::new(),
        ]);
    }
    if let Some(notice) = finding_view_notice(&summary) {
        lines.extend([
            format!("> **Filtered finding view.** {notice}"),
            String::new(),
        ]);
    }
    let not_measured = py::get_list(&summary, "pages_not_measured");
    if !not_measured.is_empty() {
        let requested = py::get(&summary, "pages_checked")
            .and_then(py::to_i64)
            .map(|n| n.to_string())
            .unwrap_or_else(|| not_measured.len().to_string());
        lines.extend([
            format!(
                "> **{} of {} requested pages could not be measured** — error or blocked \
                 responses, no findings were taken from their bodies:",
                not_measured.len(),
                requested
            ),
            String::new(),
        ]);
        for row in not_measured.iter().take(10) {
            lines.push(format!(
                "> - {} — {} (status {})",
                field(py::get(row, "url"), None),
                field(py::get(row, "reason"), None),
                field(py::get(row, "status"), None)
            ));
        }
        if not_measured.len() > 10 {
            lines.push(format!("> - … {} more", not_measured.len() - 10));
        }
        lines.push("");
    }

    let evidence = evidence_summary::rows(&summary);
    if !evidence.is_empty() {
        lines.extend([
            "## Saved evidence coverage".to_owned(),
            String::new(),
            "| Kind | Measurement | State | Scope or reason |".to_owned(),
            "|---|---|---|---|".to_owned(),
        ]);
        for row in &evidence {
            lines.push(format!(
                "| {} |",
                row.iter()
                    .map(|v| coverage_field(Some(&json!(v))))
                    .collect::<Vec<_>>()
                    .join(" | ")
            ));
        }
        lines.push("");
    }

    let coverage = py::get(&summary, "project_coverage").filter(|v| v.is_object());
    if let Some(coverage) = coverage {
        let project = py::get_map(coverage, "project");
        let status = py::get_map(coverage, "status");
        lines.extend([
            "## Project checklist coverage".to_owned(),
            String::new(),
            format!(
                "Project: {} · UUID: {}",
                py::str_value(project.get("site").unwrap_or(&Value::Null)),
                py::str_value(project.get("uuid").unwrap_or(&Value::Null))
            ),
            format!(
                "Checklist state: {} · Revision: {}",
                py::str_value(status.get("state").unwrap_or(&Value::Null)),
                py::str_value(status.get("revision").unwrap_or(&Value::Null))
            ),
            String::new(),
        ]);
        let status_v = Value::Object(status.clone());
        let counts = status.get("counts");
        let task_kpis = py::get_map(&status_v, "task_kpis");
        if !task_kpis.is_empty() {
            let task_kpis_v = Value::Object(task_kpis.clone());
            let affected = py::get_map(&task_kpis_v, "affected_urls");
            lines.extend([
                format!(
                    "Custom tasks: {} complete / {} total; {} assigned; affected URLs {} ({}).",
                    py::str_value(task_kpis.get("complete").unwrap_or(&json!(0))),
                    py::str_value(task_kpis.get("total").unwrap_or(&json!(0))),
                    py::str_value(task_kpis.get("assigned").unwrap_or(&json!(0))),
                    field(affected.get("count"), None),
                    field(affected.get("state"), None)
                ),
                String::new(),
            ]);
        }
        if let Some(counts) = counts.and_then(Value::as_object) {
            lines.extend([
                "| Total | Complete | Remaining | Run | Not applicable | Not run | Stale | Disabled |".to_owned(),
                "|---|---|---|---|---|---|---|---|".to_owned(),
                format!(
                    "| {} | {} | {} | {} | {} | {} | {} | {} |",
                    field(counts.get("total"), None),
                    field(counts.get("complete"), None),
                    field(counts.get("remaining"), None),
                    field(counts.get("run"), None),
                    field(counts.get("not_applicable"), None),
                    field(counts.get("not_run"), None),
                    field(counts.get("stale"), None),
                    field(counts.get("disabled"), None)
                ),
                String::new(),
            ]);
        } else if status.get("reason").is_some_and(|v| py::truthy(Some(v))) {
            lines.extend([
                format!("Reason: {}", field(status.get("reason"), None)),
                String::new(),
            ]);
        }
        let items = py::get_list(&status_v, "items");
        if !items.is_empty() {
            lines.extend([
                "| Item | Kind | Execution | Priority | State | Attempt | Complete | Blocked by | Enabled | Scope | Measurement | Task assignee | Affected URLs | Reason |".to_owned(),
                "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|".to_owned(),
            ]);
            for item in items {
                let task = py::get_map(item, "task");
                lines.push(format!(
                    "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                    coverage_field(
                        py::get(item, "title")
                            .filter(|v| py::truthy(Some(*v)))
                            .or_else(|| py::get(item, "id"))
                    ),
                    coverage_field(py::get(item, "kind")),
                    coverage_field(py::get(item, "execution_kind")),
                    coverage_field(Some(&json!(priority_text(item)))),
                    coverage_field(py::get(item, "state")),
                    coverage_field(py::get(item, "attempt_status")),
                    coverage_field(py::get(item, "complete")),
                    coverage_field(Some(&json!(value_text(py::get(item, "blocked_by"))))),
                    coverage_field(py::get(item, "enabled")),
                    coverage_field(Some(&json!(value_text(py::get(item, "scope"))))),
                    coverage_field(Some(&json!(value_text(py::get(item, "measurement"))))),
                    coverage_field(Some(&json!(value_text(task.get("assignee"))))),
                    coverage_field(Some(&json!(value_text(py::get(item, "affected_urls"))))),
                    coverage_field(py::get(item, "reason")),
                ));
            }
            lines.push("");
        }
    }

    lines.extend([
        "| Metric | Value |".to_owned(),
        "|---|---|".to_owned(),
        format!(
            "| Pages checked | {} |",
            py::str_value(summary.get("pages_checked").unwrap_or(&json!(0)))
        ),
        format!(
            "| Critical findings | {} |",
            py::str_value(by_sev.get("critical").unwrap_or(&json!(0)))
        ),
        format!(
            "| Warnings | {} |",
            py::str_value(by_sev.get("warning").unwrap_or(&json!(0)))
        ),
        format!(
            "| Notices | {} |",
            py::str_value(by_sev.get("notice").unwrap_or(&json!(0)))
        ),
        String::new(),
    ]);

    let disabled = py::get_list(&summary, "checks_disabled");
    if !disabled.is_empty() {
        lines.extend([
            "## Disabled checks".to_owned(),
            String::new(),
            "These checks were deliberately turned off and did not run -- do not read their silence as a clean result:".to_owned(),
            String::new(),
        ]);
        for d in disabled {
            lines.push(format!(
                "- **{}** — {}",
                check_title(py::get(d, "id").unwrap_or(&Value::Null)),
                py::str_value(py::get(d, "reason").unwrap_or(&Value::Null))
            ));
        }
        lines.push("");
    }

    let failed = py::get_list(&summary, "tools_failed");
    if !failed.is_empty() {
        lines.extend([
            "## Unavailable checks".to_owned(),
            String::new(),
            "These checks did not complete. Their silence does not mean no issues were found:"
                .to_owned(),
            String::new(),
        ]);
        for f in failed {
            lines.push(format!(
                "- **{}** — {}",
                check_title(py::get(f, "tool").unwrap_or(&Value::Null)),
                py::str_value(py::get(f, "error").unwrap_or(&Value::Null))
            ));
        }
        lines.push("");
    }

    let suppressed = document.suppressed_issues();
    let exclusions = finding_exclusion_report(&summary, Some(&suppressed));
    if let Some(exclusions) = &exclusions {
        let total = py::get(exclusions, "suppressed_total")
            .and_then(py::to_i64)
            .unwrap_or(0);
        let finding_label = if total == 1 { "finding" } else { "findings" };
        let rule_count = py::get(exclusions, "rules_configured")
            .and_then(py::to_i64)
            .unwrap_or(0);
        let rule_label = if rule_count == 1 { "rule" } else { "rules" };
        lines.extend([
            "## Finding exclusions".to_owned(),
            String::new(),
            format!(
                "The source audit records {total} {finding_label} excluded by {rule_count} \
                 configured URL {rule_label} ({} occurrences). These records are excluded from \
                 the active findings and task tables below.",
                py::str_value(
                    exclusions
                        .get("suppressed_occurrences")
                        .unwrap_or(&Value::Null)
                )
            ),
            String::new(),
            "| Rule | Pattern | Checks | Suppressed findings | Suppressed occurrences | Reason |"
                .to_owned(),
            "|---|---|---|---:|---:|---|".to_owned(),
        ]);
        for rule in py::get_list(exclusions, "rules") {
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
            lines.push(format!(
                "| {} | {} | {} | {} | {} | {} |",
                coverage_field(py::get(rule, "id")),
                coverage_field(py::get(rule, "pattern")),
                coverage_field(Some(&json!(checks))),
                py::str_value(rule.get("suppressed_findings").unwrap_or(&Value::Null)),
                py::str_value(rule.get("suppressed_occurrences").unwrap_or(&Value::Null)),
                coverage_field(py::get(rule, "reason")),
            ));
        }
        lines.push("");
        let excluded_issues = py::get_list(exclusions, "issues");
        if !excluded_issues.is_empty() {
            lines.extend([
                "### Suppressed findings".to_owned(),
                String::new(),
                "| Check | Severity | URL | Rule | Reason |".to_owned(),
                "|---|---|---|---|---|".to_owned(),
            ]);
            for issue in excluded_issues.iter().take(MAX_SUPPRESSED_ROWS) {
                let marker = py::get_map(issue, "suppression");
                lines.push(format!(
                    "| {} | {} | {} | {} | {} |",
                    coverage_field(Some(&json!(check_title(
                        py::get(issue, "check").unwrap_or(&Value::Null)
                    )))),
                    coverage_field(py::get(issue, "severity")),
                    coverage_field(py::get(issue, "target_url")),
                    coverage_field(marker.get("rule_id")),
                    coverage_field(marker.get("reason")),
                ));
            }
            if excluded_issues.len() > MAX_SUPPRESSED_ROWS {
                lines.push(format!(
                    "| … {} more | | | | |",
                    excluded_issues.len() - MAX_SUPPRESSED_ROWS
                ));
            }
            lines.push("");
        }
    }

    let view_columns = finding_view_columns(&summary);
    if let Some(columns) = view_columns {
        lines.extend([
            "## Saved finding view".to_owned(),
            String::new(),
            format!(
                "| {} |",
                columns
                    .iter()
                    .map(|c| finding_view_label(c))
                    .collect::<Vec<_>>()
                    .join(" | ")
            ),
            format!(
                "|{}|",
                columns.iter().map(|_| "---").collect::<Vec<_>>().join("|")
            ),
        ]);
        for finding in document.findings(None) {
            let values = py::get_map(&finding, "view_fields");
            lines.push(format!(
                "| {} |",
                columns
                    .iter()
                    .map(|column| { field(values.get(column), None).replace(['\r', '\n'], " ") })
                    .collect::<Vec<_>>()
                    .join(" | ")
            ));
        }
        lines.push("");
    } else {
        for level in ["critical", "warning", "notice"] {
            let chunk = document.findings(Some(level));
            let count = if document.is_streaming() {
                py::to_i64(by_sev.get(level).unwrap_or(&json!(0))).unwrap_or(0)
            } else {
                chunk.len() as i64
            };
            if count == 0 {
                continue;
            }
            lines.extend([
                format!("## {} — {}", crate::severity_title(level), count),
                String::new(),
            ]);
            for finding in &chunk {
                lines.push(format!(
                    "- **{}**",
                    py::get(finding, "client_title")
                        .map(py::str_value)
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "Audit finding".into())
                ));
                if let Some(observation) =
                    py::get(finding, "client_observation").filter(|v| py::truthy(Some(*v)))
                {
                    lines.push(format!("  - Observation: {}", py::str_value(observation)));
                }
                lines.push(format!(
                    "  - Reproduction: {}",
                    py::str_value(py::get(finding, "client_reproduction").unwrap_or(&Value::Null))
                ));
                for detail in py::get_list(finding, "client_details") {
                    lines.push(format!("  - Evidence: {}", py::str_value(detail)));
                }
                for location in py::get_list(finding, "client_locations") {
                    lines.push(format!("  - Location: {}", py::str_value(location)));
                }
            }
            lines.push("");
        }
    }

    let pages = document.pages();
    if if document.is_streaming() {
        document.page_count().is_some_and(|n| n > 0)
    } else {
        !pages.is_empty()
    } {
        lines.extend([
            "## Pages".to_owned(),
            String::new(),
            "| URL | Status | Title | Words | Canonical |".to_owned(),
            "|---|---|---|---|---|".to_owned(),
        ]);
        for page in &pages {
            lines.push(format!(
                "| {} | {} | {} | {} | {} |",
                field(py::get(page, "url"), None),
                field(py::get(page, "status"), None),
                field(py::get(page, "title"), Some(80)),
                field(py::get(page, "words"), None),
                field(py::get(page, "canonical"), Some(60)),
            ));
        }
        lines.push("");
    }

    if let Some(note) = summary
        .get("severity_note")
        .filter(|v| py::truthy(Some(*v)))
    {
        lines.extend([
            "---".to_owned(),
            String::new(),
            format!("_{}_", py::str_value(note)),
            String::new(),
        ]);
    }
    Ok(())
}

/// Write `document` to `path`.
pub fn write(document: &dyn ReportDoc, path: &Path) -> Result<(), MdError> {
    let mut file = std::fs::File::create(path)?;
    render(document, &mut file)
}

/// A list-shaped sink that writes separators without buffering report rows.
struct Lines<'a> {
    stream: &'a mut dyn Write,
    first: bool,
}

impl<'a> Lines<'a> {
    fn push(&mut self, value: impl AsRef<str>) {
        if !self.first {
            let _ = self.stream.write_all(b"\n");
        }
        let _ = self.stream.write_all(value.as_ref().as_bytes());
        self.first = false;
    }

    fn extend<const N: usize>(&mut self, values: [String; N]) {
        for value in values {
            self.push(value);
        }
    }
}

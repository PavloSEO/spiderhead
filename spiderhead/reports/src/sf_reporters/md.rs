//! The `audit.md` human-readable projection of a retained audit.
//!
//! The renderer only formats what the analyzer measured: every absence (score, coverage,
//! internal linking) is stated with its reason instead of being printed as a clean zero.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use spiderhead_formats::check_registry::{builtin as check_registry, CheckMeta, Severity};
use spiderhead_formats::models::{AuditResult, Issue};
use thiserror::Error;

use super::provider;
use crate::client_findings::check_title;
use crate::py;

/// Labels shown in the severity table and section headings.
const SEVERITY_LABEL: [(&str, &str); 3] = [
    ("critical", "🔴 Critical"),
    ("warning", "🟡 Warning"),
    ("notice", "⚪ Notice"),
];
/// Checks whose findings render as a broken-link location table.
const LINK_CHECKS: [&str; 3] = [
    "BROKEN_INTERNAL_LINK",
    "LINK_TO_5XX",
    "INTERNAL_LINK_TO_REDIRECT",
];
/// Checks whose findings render as duplicate groups.
const DUPLICATE_CHECKS: [&str; 4] = [
    "TITLE_DUPLICATE",
    "DESC_DUPLICATE",
    "H1_DUPLICATE",
    "DUPLICATE_BY_HASH",
];

// Readability caps — keep tables/sections from exploding on big crawls.
const TOP_CHECKS: usize = 12;
const MAX_CELL_CHARS: usize = 200;
const MAX_LINK_ROWS: usize = 200;
const MAX_GROUP_URLS: usize = 50;
const MAX_GENERIC_ROWS: usize = 500;

/// Failure writing the Markdown artifact.
#[derive(Debug, Error)]
pub enum MdError {
    /// Creating the output directory or writing the file failed.
    #[error("audit.md write failed: {0}")]
    Io(#[from] std::io::Error),
}

/// Text for values interpolated in the report: `None` for missing or null, `True`/`False`,
/// plain numbers and strings, and quoted representations for lists and objects.
pub(super) fn py_str(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "None".to_owned(),
        Some(v @ (Value::Array(_) | Value::Object(_))) => py::repr(v),
        Some(v) => py::str_value(v),
    }
}

/// Like the above, with a caller-chosen default for a *missing* key; a stored `None` still
/// formats as `None`.
pub(super) fn py_str_or(value: Option<&Value>, none: &str) -> String {
    match value {
        None => none.to_owned(),
        Some(Value::Null) => "None".to_owned(),
        Some(v @ (Value::Array(_) | Value::Object(_))) => py::repr(v),
        Some(v) => py::str_value(v),
    }
}

/// `esc`: table-cell escape — backslash first, then the pipe delimiter,
/// newlines flattened; overlong cells truncated with an ellipsis.
pub(super) fn esc(value: Option<&Value>) -> String {
    let text = match value {
        None | Some(Value::Null) => return String::new(),
        Some(v @ (Value::Array(_) | Value::Object(_))) => py::repr(v),
        Some(v) => py::str_value(v),
    };
    let text = text
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace('\n', " ")
        .trim()
        .to_owned();
    if text.chars().count() > MAX_CELL_CHARS {
        let mut out: String = text.chars().take(MAX_CELL_CHARS - 1).collect();
        out.push('…');
        out
    } else {
        text
    }
}

/// `code`: inline-code span made safe by neutralizing backticks.
pub(super) fn code(value: Option<&Value>) -> String {
    esc(value).replace('`', "ʼ")
}

/// Integer value for `kb`: truncates floats, parses signed digit strings with
/// underscores, and reads booleans as 0 or 1; anything else is an error.
fn py_int(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(Value::Bool(flag)) => Some(i64::from(*flag)),
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Some(Value::String(text)) => {
            let cleaned: String = text.trim().chars().filter(|c| *c != '_').collect();
            cleaned.parse::<i64>().ok()
        }
        _ => None,
    }
}

/// `kb`: byte count as whole KiB, `—` when the value is not a number.
fn kb(value: Option<&Value>) -> String {
    match py_int(value) {
        Some(n) => format!("{:.0} KB", n as f64 / 1024.0),
        None => "—".to_owned(),
    }
}

/// `check_meta(check)`: registry metadata or the documented fallback.
fn check_meta(check: &str) -> CheckMeta {
    match check_registry() {
        Ok(registry) => registry.meta_or_fallback(check),
        Err(_) => CheckMeta {
            id: check.to_owned(),
            severity: Severity::Notice,
            source: "SF-derived".to_owned(),
            message: check.to_owned(),
            fix: None,
            doc_url: None,
        },
    }
}

/// Map lookup with a default for a missing key.
pub(super) fn get_or<'a>(
    map: &'a serde_json::Map<String, Value>,
    key: &str,
    default: &'a Value,
) -> &'a Value {
    map.get(key).unwrap_or(default)
}

/// Capitalize the first character and lowercase the rest.
fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => {
            let mut out: String = first.to_uppercase().collect();
            out.push_str(&chars.as_str().to_lowercase());
            out
        }
    }
}

/// The URL a finding prints: `details.safe_url` when set, else `target_url`.
fn issue_url(issue: &Issue) -> Value {
    match issue.details.get("safe_url") {
        Some(v) if py::truthy(Some(v)) => v.clone(),
        _ => issue
            .target_url
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null),
    }
}

/// Render the report and write it as UTF-8, creating the destination directory first;
/// returns the path.
pub fn write_markdown(result: &AuditResult, path: &str) -> Result<String, MdError> {
    let text = render_markdown(result);
    let target = PathBuf::from(path);
    let absolute = if target.is_absolute() {
        target.clone()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(&target)
    };
    let parent = absolute
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(parent)?;
    std::fs::write(&target, text)?;
    Ok(path.to_owned())
}

/// Render the whole `audit.md` body (the text `write_markdown` persists).
pub fn render_markdown(result: &AuditResult) -> String {
    let mut lines: Vec<String> = Vec::new();
    let run = &result.run;
    let s = &result.summary;

    // 1. header
    lines.push(format!(
        "# SEO audit — {}",
        py_str_or(run.get("project"), "unknown")
    ));
    lines.push(String::new());
    lines.push(format!(
        "- **Generated:** {}",
        py_str(run.get("generated_at"))
    ));
    if py::truthy(run.get("source")) {
        lines.push(format!("- **Audited site:** {}", esc(run.get("source"))));
    }
    lines.push(String::new());

    // 2. health summary
    lines.push("## Health summary".to_owned());
    lines.push(String::new());
    if run.get("crawl_valid") == Some(&Value::Bool(false)) {
        // An invalid crawl leads with the failure: a score printed next to a
        // critical NO_RESPONSE reads as a verdict on the site, not on the run.
        let reason = [
            s.get("health_score_reason"),
            run.get("crawl_invalid_reason"),
        ]
        .into_iter()
        .flatten()
        .find(|v| py::truthy(Some(v)))
        .map(|v| esc(Some(v)))
        .unwrap_or_else(|| "unknown".to_owned());
        lines.push(format!("> **Crawl failed — no health score.** {reason}."));
        lines.push(">".to_owned());
        lines.push(
            "> The findings below describe the failed run, not the state of the site.".to_owned(),
        );
    } else {
        if s.get("health_score").is_none_or(Value::is_null) {
            // The aggregator withholds the score deliberately and records why;
            // the reason is stated, never disguised as a number.
            let reason = s
                .get("health_score_reason")
                .filter(|v| py::truthy(Some(v)))
                .map(|v| esc(Some(v)))
                .unwrap_or_else(|| "the run recorded no reason".to_owned());
            lines.push(format!("> **No health score.** {reason}."));
        } else {
            lines.push(format!(
                "**Health score: {} / 100**",
                py_str(s.get("health_score"))
            ));
        }
        if py::truthy(s.get("health_score_basis")) {
            lines.push(String::new());
            lines.push(format!("_{}_", esc(s.get("health_score_basis"))));
        }
        if py::truthy(s.get("health_score_scope")) {
            lines.push(String::new());
            lines.push(format!("_{}_", esc(s.get("health_score_scope"))));
        }
        if let Some(coverage) = s.get("check_coverage").filter(|c| py::truthy(Some(c))) {
            lines.push(String::new());
            lines.push(format!(
                "- Checks: **{} fired**, {} skipped, {} silent, {} disabled (of {} total)",
                py_str_or(coverage.get("checks_fired"), "0"),
                py_str_or(coverage.get("checks_skipped"), "0"),
                py_str_or(coverage.get("checks_silent"), "0"),
                py_str_or(coverage.get("checks_disabled"), "0"),
                py_str_or(coverage.get("checks_total"), "0"),
            ));
        }
    }
    lines.push(String::new());
    let empty = Map::new();
    let totals = s.get("totals").and_then(Value::as_object).unwrap_or(&empty);
    lines.push(format!(
        "- URLs crawled: **{}** (HTML: {}, indexable: {})",
        py_str(totals.get("urls_crawled")),
        py_str(totals.get("html_pages")),
        py_str(totals.get("html_indexable")),
    ));
    lines.push(format!(
        "- Total issues: **{}**",
        py_str(totals.get("issues_total"))
    ));
    let finding_policy: &[Value] = run
        .get("finding_exclusion_policy")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if !finding_policy.is_empty() {
        let total = result.suppressed_issues.len();
        let label = if total == 1 { "finding" } else { "findings" };
        lines.push(format!(
            "- **Finding exclusions:** {total} {label} suppressed by {} configured rule(s).",
            finding_policy.len()
        ));
    }
    lines.push(String::new());
    let by_sev = s.get("by_severity").and_then(Value::as_object);
    lines.push("| Severity | Count |".to_owned());
    lines.push("|---|---:|".to_owned());
    for (sev, label) in SEVERITY_LABEL {
        let count = by_sev
            .and_then(|m| m.get(sev))
            .map(py::str_value)
            .unwrap_or_else(|| "0".to_owned());
        lines.push(format!("| {label} | {count} |"));
    }
    lines.push(String::new());
    let top: Vec<(&String, &Value)> = s
        .get("by_check")
        .and_then(Value::as_object)
        .map(|m| m.iter().take(TOP_CHECKS).collect())
        .unwrap_or_default();
    if !top.is_empty() {
        // Use the actual severity carried by this run's issues (post
        // severity_overrides), not the registry default.
        let mut check_severity: HashMap<&str, &str> = HashMap::new();
        for issue in &result.issues {
            check_severity
                .entry(issue.check.as_str())
                .or_insert(issue.severity.as_str());
        }
        lines.push("**Most frequent issues:**".to_owned());
        lines.push(String::new());
        lines.push("| Issue | Count | Severity |".to_owned());
        lines.push("|---|---:|---|".to_owned());
        for (check, count) in top {
            let severity = check_severity
                .get(check.as_str())
                .map(|s| (*s).to_owned())
                .unwrap_or_else(|| check_meta(check).severity.as_str().to_owned());
            let title = check_title(&Value::String(check.clone()));
            lines.push(format!(
                "| {title} | {} | {severity} |",
                py::str_value(count)
            ));
        }
        lines.push(String::new());
    }
    provider::write_internal_linking(&mut lines, s.get("internal_linking"));
    if let Some(ss) = s.get("size_stats_bytes") {
        lines.push(format!(
            "**HTML size:** median {}, p90 {}, p95 {}, max {}.",
            kb(ss.get("median")),
            kb(ss.get("p90")),
            kb(ss.get("p95")),
            kb(ss.get("max")),
        ));
        lines.push(String::new());
    }

    // Printed above the findings: a reader who has gone through 400 findings
    // has already spent the effort this warning was meant to save.
    let implausible = s
        .get("implausible_checks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !implausible.is_empty() {
        lines.push("## Look at these before trusting the rest".to_owned());
        lines.push(String::new());
        lines.push(
            concat!(
                "Each check below describes more than half the crawled pages. That can be true ",
                "-- a site really may have no meta description anywhere -- but it is also what ",
                "a broken check looks like, and it is worth one minute of checking against the ",
                "live site before the rest of this report is acted on."
            )
            .to_owned(),
        );
        lines.push(String::new());
        lines.push("| Issue | Pages | Share of crawl |".to_owned());
        lines.push("|---|---:|---:|".to_owned());
        for row in &implausible {
            let share = row.get("share").and_then(Value::as_f64).unwrap_or_default();
            lines.push(format!(
                "| {} | {} | {:.0}% |",
                check_title(row.get("check").unwrap_or(&Value::Null)),
                py::str_value(row.get("pages").unwrap_or(&Value::Null)),
                share * 100.0,
            ));
        }
        lines.push(String::new());
    }

    // group issues by severity
    let mut by_sev_issues: HashMap<&str, Vec<&Issue>> = HashMap::new();
    for issue in &result.issues {
        by_sev_issues
            .entry(issue.severity.as_str())
            .or_default()
            .push(issue);
    }

    // 3/4/5. sections by severity
    for (sev, label) in SEVERITY_LABEL {
        let issues = by_sev_issues.get(sev).cloned().unwrap_or_default();
        if issues.is_empty() {
            continue;
        }
        lines.push(format!("## {label} ({})", issues.len()));
        lines.push(String::new());
        render_severity_section(&mut lines, &issues);
    }

    if !finding_policy.is_empty() {
        lines.push(format!(
            "## Finding exclusions ({} rules)",
            finding_policy.len()
        ));
        lines.push(String::new());
        lines.push(
            "Rules are evaluated in order against finding target URLs; the first matching rule is used."
                .to_owned(),
        );
        lines.push(String::new());
        lines.push("| Rule | Pattern | Checks | Reason | Suppressed findings |".to_owned());
        lines.push("|---|---|---|---|---:|".to_owned());
        let by_rule: HashMap<String, &Value> = s
            .get("finding_exclusions")
            .and_then(|fe| fe.get("by_rule"))
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .map(|item| (py_str(item.get("id")), item))
                    .collect()
            })
            .unwrap_or_default();
        for rule in finding_policy {
            let rule_summary = by_rule.get(&py_str(rule.get("id")));
            let checks = rule
                .get("checks")
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .map(py::str_value)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .filter(|joined| !joined.is_empty())
                .unwrap_or_else(|| "all checks".to_owned());
            let suppressed = rule_summary
                .and_then(|row| row.get("suppressed_findings"))
                .map(py::str_value)
                .unwrap_or_else(|| "0".to_owned());
            lines.push(format!(
                "| {} | `{}` | {} | {} | {suppressed} |",
                esc(rule.get("id")),
                code(rule.get("pattern")),
                esc(Some(&Value::String(checks))),
                esc(rule.get("reason")),
            ));
        }
        lines.push(String::new());
    }

    if !result.suppressed_issues.is_empty() {
        lines.push(format!(
            "## Suppressed findings ({})",
            result.suppressed_issues.len()
        ));
        lines.push(String::new());
        lines.push(
            "These measured findings are excluded from active totals, score and tasks by the explicit URL policy."
                .to_owned(),
        );
        lines.push(String::new());
        lines.push("| Check | Severity | URL | Rule | Reason |".to_owned());
        lines.push("|---|---|---|---|---|".to_owned());
        for issue in result.suppressed_issues.iter().take(MAX_GENERIC_ROWS) {
            let empty = Map::new();
            let suppression = issue
                .get("suppression")
                .and_then(Value::as_object)
                .unwrap_or(&empty);
            let details = issue
                .get("details")
                .and_then(Value::as_object)
                .unwrap_or(&empty);
            let url = match details.get("safe_url") {
                Some(v) if py::truthy(Some(v)) => v.clone(),
                _ => issue.get("target_url").cloned().unwrap_or(Value::Null),
            };
            lines.push(format!(
                "| {} | {} | {} | {} | {} |",
                py::str_value(issue.get("check").unwrap_or(&Value::Null)),
                py::str_value(issue.get("severity").unwrap_or(&Value::Null)),
                esc(Some(&url)),
                esc(suppression.get("rule_id")),
                esc(suppression.get("reason")),
            ));
        }
        if result.suppressed_issues.len() > MAX_GENERIC_ROWS {
            lines.push(format!(
                "| … {} more | | | | |",
                result.suppressed_issues.len() - MAX_GENERIC_ROWS
            ));
        }
        lines.push(String::new());
    }

    // 6. sitemap & robots
    if s.contains_key("sitemap") {
        render_sitemap(&mut lines, &s["sitemap"]);
    }

    provider::write_provider_findings(&mut lines, s.get("provider_findings"));

    // 7. appendix
    let skipped = &result.skipped;
    if !skipped.is_empty() {
        lines.push("## Appendix: skipped checks".to_owned());
        lines.push(String::new());
        lines.push("| Issue | Reason |".to_owned());
        lines.push("|---|---|".to_owned());
        for sk in skipped {
            lines.push(format!(
                "| {} | {} |",
                check_title(&Value::String(sk.id.clone())),
                esc(Some(&Value::String(sk.reason.clone()))),
            ));
        }
        lines.push(String::new());
    }
    let disabled = &result.disabled;
    if !disabled.is_empty() {
        lines.push("## Appendix: disabled checks".to_owned());
        lines.push(String::new());
        lines.push("| Issue |".to_owned());
        lines.push("|---|".to_owned());
        for d in disabled {
            lines.push(format!("| {} |", check_title(&Value::String(d.id.clone())),));
        }
        lines.push(String::new());
    }

    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// `render_severity_section`: one `## <severity>` block, checks sorted by id.
fn render_severity_section(lines: &mut Vec<String>, issues: &[&Issue]) {
    let mut by_check: HashMap<&str, Vec<&Issue>> = HashMap::new();
    for issue in issues {
        by_check
            .entry(issue.check.as_str())
            .or_default()
            .push(*issue);
    }
    let mut checks: Vec<&str> = by_check.keys().copied().collect();
    checks.sort_unstable();

    for check in checks {
        let group = &by_check[check];
        let meta = check_meta(check);
        lines.push(format!(
            "### {} ({})",
            check_title(&Value::String(check.to_owned())),
            group.len()
        ));
        lines.push(String::new());
        if LINK_CHECKS.contains(&check) {
            render_link_table(lines, group);
        } else if check == "LARGE_HTML" {
            render_large_html(lines, group);
        } else if DUPLICATE_CHECKS.contains(&check) {
            render_duplicates(lines, group);
        } else if check == "H1_MULTIPLE" {
            render_h1_multiple(lines, group);
        } else {
            render_generic(lines, group);
        }
        if let Some(fix) = meta.fix.as_deref().filter(|f| !f.is_empty()) {
            lines.push(format!("> _How to fix:_ {fix}"));
            lines.push(String::new());
        }
    }
}

/// `render_link_table`: destination table capped at `MAX_LINK_ROWS` rows.
fn render_link_table(lines: &mut Vec<String>, group: &[&Issue]) {
    lines.push("| Destination | Status | Source page | Anchor | Position | XPath |".to_owned());
    lines.push("|---|---:|---|---|---|---|".to_owned());
    let mut rows = 0usize;
    for issue in group {
        for loc in &issue.locations {
            if rows >= MAX_LINK_ROWS {
                lines.push("| … more rows omitted | | | | | |".to_owned());
                lines.push(String::new());
                return;
            }
            rows += 1;
            let status = match issue.status_code {
                Some(code) if code != 0 => code.to_string(),
                _ => String::new(),
            };
            lines.push(format!(
                "| {} | {} | {} | {} | {} | `{}` |",
                esc(issue
                    .target_url
                    .as_ref()
                    .map(|u| Value::String(u.clone()))
                    .as_ref()),
                status,
                esc(loc.get("source_url")),
                esc(loc.get("anchor")),
                esc(loc.get("link_position")),
                code(loc.get("link_path")),
            ));
        }
    }
    lines.push(String::new());
}

/// `render_large_html`: per-URL size table sorted by descending ratio.
fn render_large_html(lines: &mut Vec<String>, group: &[&Issue]) {
    lines.push("| URL | Size | × median | Rank |".to_owned());
    lines.push("|---|---:|---:|---:|".to_owned());
    let mut sorted: Vec<&Issue> = group.to_vec();
    sorted.sort_by(|a, b| {
        let ra = a
            .details
            .get("ratio")
            .and_then(Value::as_f64)
            .unwrap_or_default();
        let rb = b
            .details
            .get("ratio")
            .and_then(Value::as_f64)
            .unwrap_or_default();
        rb.partial_cmp(&ra).unwrap_or(std::cmp::Ordering::Equal)
    });
    for issue in sorted {
        let d = &issue.details;
        let rank = d
            .get("rank")
            .filter(|r| py::truthy(Some(r)))
            .map(py::str_value)
            .unwrap_or_else(|| "—".to_owned());
        lines.push(format!(
            "| {} | {} | ×{} | {} |",
            esc(issue
                .target_url
                .as_ref()
                .map(|u| Value::String(u.clone()))
                .as_ref()),
            kb(d.get("size_bytes")),
            py_str(d.get("ratio")),
            rank,
        ));
    }
    lines.push(String::new());
}

/// `render_duplicates`: one block per distinct `group_id` (None included).
fn render_duplicates(lines: &mut Vec<String>, group: &[&Issue]) {
    let mut seen_groups: std::collections::HashSet<Option<String>> =
        std::collections::HashSet::new();
    for issue in group {
        let gid = issue.group_id.clone();
        if !seen_groups.insert(gid.clone()) {
            continue;
        }
        let value = issue.details.get("value");
        let urls: Vec<&Issue> = group
            .iter()
            .filter(|i| i.group_id == gid)
            .copied()
            .collect();
        match value {
            Some(v) if py::truthy(Some(v)) => {
                lines.push(format!("- **\"{}\"** — {} URLs:", esc(Some(v)), urls.len()));
            }
            _ => {
                lines.push(format!("- Duplicate group — {} URLs:", urls.len()));
            }
        }
        for url_issue in urls.iter().take(MAX_GROUP_URLS) {
            lines.push(format!(
                "    - {}",
                esc(url_issue
                    .target_url
                    .as_ref()
                    .map(|u| Value::String(u.clone()))
                    .as_ref())
            ));
        }
        if urls.len() > MAX_GROUP_URLS {
            lines.push(format!("    - … {} more", urls.len() - MAX_GROUP_URLS));
        }
    }
    lines.push(String::new());
}

/// `render_h1_multiple`: URL plus every recorded H1 text.
fn render_h1_multiple(lines: &mut Vec<String>, group: &[&Issue]) {
    lines.push("| URL | H1 text |".to_owned());
    lines.push("|---|---|".to_owned());
    for issue in group {
        let texts = issue
            .details
            .get("h1_texts")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .map(|t| esc(Some(t)))
                    .collect::<Vec<_>>()
                    .join(" ⏐ ")
            })
            .unwrap_or_default();
        lines.push(format!(
            "| {} | {} |",
            esc(issue
                .target_url
                .as_ref()
                .map(|u| Value::String(u.clone()))
                .as_ref()),
            texts,
        ));
    }
    lines.push(String::new());
}

/// `render_generic`: details table when any issue carries scalar details,
/// otherwise a plain URL list; both capped at `MAX_GENERIC_ROWS`.
fn render_generic(lines: &mut Vec<String>, group: &[&Issue]) {
    const HIDDEN: [&str; 4] = ["safe_url", "url_id", "user_canonical", "google_canonical"];
    let has_details = group.iter().any(|i| !i.details.is_empty());
    if has_details {
        lines.push("| URL | Details |".to_owned());
        lines.push("|---|---|".to_owned());
        for issue in group.iter().take(MAX_GENERIC_ROWS) {
            let detail = issue
                .details
                .iter()
                .filter(|(k, v)| !HIDDEN.contains(&k.as_str()) && !v.is_array() && !v.is_object())
                .map(|(k, v)| format!("{}: {}", capitalize(&k.replace('_', " ")), esc(Some(v))))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("| {} | {} |", esc(Some(&issue_url(issue))), detail));
        }
    } else {
        for issue in group.iter().take(MAX_GENERIC_ROWS) {
            lines.push(format!("- {}", esc(Some(&issue_url(issue)))));
        }
    }
    if group.len() > MAX_GENERIC_ROWS {
        lines.push(format!("- … {} more", group.len() - MAX_GENERIC_ROWS));
    }
    lines.push(String::new());
}

/// `render_sitemap`: sitemap/robots coverage block.
fn render_sitemap(lines: &mut Vec<String>, sm: &Value) {
    lines.push("## Sitemap & robots".to_owned());
    lines.push(String::new());
    lines.push(format!(
        "- Declared in robots.txt: **{}**",
        py_str(sm.get("declared_in_robots"))
    ));
    if let Some(sitemaps) = sm.get("sitemaps").and_then(Value::as_array) {
        if py::truthy(sm.get("sitemaps")) {
            let joined = sitemaps
                .iter()
                .map(|x| esc(Some(x)))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("- Sitemaps: {joined}"));
        }
    }
    lines.push(format!(
        "- URLs in sitemap: **{}**  ·  indexable URLs in crawl: **{}**",
        py_str(sm.get("urls_in_sitemap")),
        py_str(sm.get("urls_in_crawl_indexable")),
    ));
    lines.push(format!(
        "- In sitemap but not in crawl: **{}**  ·  in crawl but not in sitemap: **{}**",
        py_str(sm.get("in_sitemap_not_in_crawl")),
        py_str(sm.get("in_crawl_not_in_sitemap")),
    ));
    lines.push(format!(
        "- Non-200 URLs in sitemap: **{}**  ·  non-indexable URLs in sitemap: **{}**",
        py_str(sm.get("non_200_in_sitemap")),
        py_str(sm.get("non_indexable_in_sitemap")),
    ));
    if py::truthy(sm.get("image_entries_in_sitemap"))
        || py::truthy(sm.get("news_entries_in_sitemap"))
        || py::truthy(sm.get("video_entries_in_sitemap"))
    {
        // Attached image/news/video entries are their own population: the
        // report keeps both numbers, never a single mixed figure.
        lines.push(format!(
            "- Extension entries in sitemap: images **{}**  ·  news **{}**  ·  video **{}** (counted separately from pages)",
            py_str_or(sm.get("image_entries_in_sitemap"), "0"),
            py_str_or(sm.get("news_entries_in_sitemap"), "0"),
            py_str_or(sm.get("video_entries_in_sitemap"), "0"),
        ));
    }
    if let Some(lm) = sm.get("lastmod").filter(|v| py::truthy(Some(v))) {
        let share = lm
            .get("share_older_than_threshold")
            .and_then(Value::as_f64)
            .unwrap_or_default();
        let mut line = format!(
            "- lastmod: {} to {} (median {}); older than threshold: {}%",
            py_str(lm.get("oldest")),
            py_str(lm.get("newest")),
            py_str(lm.get("median")),
            (share * 100.0).round_ties_even() as i64,
        );
        if py::truthy(lm.get("all_identical")) {
            line.push_str(" · all values are identical!");
        }
        if py::truthy(lm.get("invalid_count")) {
            line.push_str(&format!(" · invalid: {}", py_str(lm.get("invalid_count"))));
        }
        lines.push(line);
    }
    lines.push(String::new());
}

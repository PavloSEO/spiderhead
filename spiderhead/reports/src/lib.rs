#![recursion_limit = "512"]

//! Report formatters: XLSX, DOCX, CSV, Markdown, JSON, PDF, and the task and reporter surfaces.
//!
//! Every writer consumes a loaded audit document: the flat `spiderhead.site-audit/1`
//! contract, or an audit.v2 document normalized once at the boundary in [`document`].
//! Writers format recorded evidence only; they never compute findings or touch the network.

pub mod audit_pdf;
pub mod bi;
pub mod bi_destinations;
pub mod bi_index;
pub mod build_report;
pub mod check_meta;
pub mod chromium_pdf;
pub mod client_findings;
pub mod cohorts;
pub mod crawl_settings;
pub mod csvfile;
pub mod document;
pub mod docx;
pub mod evidence_import;
pub mod evidence_join;
pub mod evidence_summary;
pub mod facts;
pub mod join;
pub mod json_out;
pub mod looker_link;
pub mod md;
pub mod pdf_model;
pub mod pdf_output;
pub mod pdf_stream;
pub mod pdf_validation;
pub mod project_coverage;
pub mod py;
pub mod reporting_pack;
pub mod segments;
pub mod sf_reporters;
pub mod sf_tasks;
pub mod svg_charts;
pub mod traffic_dashboard;
pub mod xlsx;

/// Report formats `build_report` accepts.
pub const FORMATS: [&str; 6] = ["xlsx", "docx", "csv", "md", "json", "pdf"];

/// Severity name to display title.
pub static SEVERITY_TITLES: std::sync::LazyLock<
    std::collections::HashMap<&'static str, &'static str>,
> = std::sync::LazyLock::new(|| {
    [
        ("critical", "Critical"),
        ("warning", "Warning"),
        ("notice", "Notice"),
        ("opportunity", "Opportunity"),
    ]
    .into_iter()
    .collect()
});

/// Display title for a severity, falling back to the raw level.
pub fn severity_title(level: &str) -> &str {
    document::severity_title(level)
}

/// The "checks completed" value for a normalized report summary.
///
/// `tools_run` names the checks that actually executed. A source that cannot supply
/// that inventory must say "not reported" rather than show a count of zero.
pub fn checks_completed_display(summary: &serde_json::Value) -> serde_json::Value {
    match summary.get("tools_run") {
        None | Some(serde_json::Value::Null) => {
            serde_json::Value::String("not reported".to_owned())
        }
        Some(serde_json::Value::Array(items)) => serde_json::json!(items.len()),
        Some(_) => serde_json::Value::String("not reported".to_owned()),
    }
}

/// Bound every report-shaped output respects.
pub const DEFAULT_MAX_OUTPUT_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Minimum free disk required before writing report outputs.
pub const MIN_FREE_DISK_BYTES: u64 = 16 * 1024 * 1024;

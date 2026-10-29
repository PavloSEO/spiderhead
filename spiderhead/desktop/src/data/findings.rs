//! The findings dashboard: saved-audit counters, per-check states and the
//! facet rows that open the URL table pre-filtered. Numbers come from the
//! audit.v2 companion via `AuditV2Reader` and the store's own
//! `issue_counters::check_states`, so they equal the reference summary.

use std::path::Path;

use serde_json::{json, Map, Value};
use spiderhead_store::audit_v2::AuditV2Reader;

use super::filters::QueryError;
use super::session::ScanSession;

/// Per-check row for the dashboard table.
#[derive(Debug, Clone)]
pub struct CheckRow {
    /// Check id in registry case.
    pub id: String,
    /// Default registry severity.
    pub severity: String,
    /// `fired` | `silent` | `skipped` | `disabled` | `unavailable` | `fired_suppressed`.
    pub state: String,
    /// Saved findings count (0 for silent/suppressed, null otherwise).
    pub findings: Option<i64>,
    /// Skipped/disabled reason text.
    pub reason: String,
    /// Findings suppressed by a saved policy.
    pub suppressed: i64,
}

/// Everything the dashboard needs, loaded once per scan.
pub struct Findings {
    /// `issues_total` from the saved summary.
    pub issues_total: i64,
    /// `findings_total` (rolls up group members differently).
    pub findings_total: i64,
    /// Per-severity totals; keys absent in the audit stay absent.
    pub by_severity: Map<String, Value>,
    /// Per-check rows in registry order.
    pub checks: Vec<CheckRow>,
    /// Number of rollup groups (`/groups` collection size).
    pub groups: i64,
    /// `summary.check_coverage` raw counters (checks_run etc.).
    pub coverage: Map<String, Value>,
}

impl Findings {
    /// Load from the audit.v2 companion; `None` when the companion is missing.
    pub fn load(scan_path: &Path) -> Option<Self> {
        // verify_binding=false — see session.rs; the strict scan validator is a
        // later store slice, the companion's own checks still run.
        let reader = AuditV2Reader::new(scan_path, false, true, Some(5.0)).ok()?;
        let header = &reader.header;
        let summary = header.get("summary")?.as_object()?;
        let totals = summary.get("totals")?.as_object()?;
        let issues_total = totals.get("issues_total")?.as_i64()?;
        let findings_total = totals
            .get("findings_total")
            .and_then(Value::as_i64)
            .unwrap_or(issues_total);
        let by_severity = summary
            .get("by_severity")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let by_check = summary
            .get("by_check")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let coverage = summary
            .get("check_coverage")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let run = header.get("run").and_then(Value::as_object).cloned()?;
        let suppressed = summary
            .get("finding_exclusions")
            .and_then(|e| e.get("by_check"))
            .cloned()
            .unwrap_or_else(|| json!({}));
        let registry = registry_value();
        let states = spiderhead_store::issue_counters::check_states(
            &registry,
            &Value::Object(by_check),
            &Value::Object(coverage.clone()),
            &Value::Object(run),
            &suppressed,
        );
        let checks = states
            .into_iter()
            .map(|row| CheckRow {
                id: row.get("id").and_then(Value::as_str).unwrap_or("").into(),
                severity: row
                    .get("severity")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .into(),
                state: row
                    .get("state")
                    .and_then(Value::as_str)
                    .unwrap_or("unavailable")
                    .into(),
                findings: row.get("findings").and_then(Value::as_i64),
                reason: row
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .into(),
                suppressed: row
                    .get("suppressed_findings")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
            })
            .collect();
        let groups = reader.count("/groups").unwrap_or(0);
        Some(Self {
            issues_total,
            findings_total,
            by_severity,
            checks,
            groups,
            coverage,
        })
    }

    /// `dashboard.inspect` payload.
    pub fn inspect(&self) -> Value {
        json!({
            "state": "available",
            "issues_total": self.issues_total,
            "findings_total": self.findings_total,
            "by_severity": self.by_severity,
            "groups": self.groups,
            "checks": self
                .checks
                .iter()
                .filter(|c| c.state != "unavailable")
                .map(|c| json!({
                    "id": c.id,
                    "severity": c.severity,
                    "state": c.state,
                    "findings": c.findings,
                    "suppressed_findings": c.suppressed,
                    "reason": c.reason,
                }))
                .collect::<Vec<_>>(),
            "coverage": self.coverage,
        })
    }
}

/// The check registry as the `{ID: {severity: …}}` map `check_states` expects.
fn registry_value() -> Value {
    let mut out = Map::new();
    if let Ok(reg) = spiderhead_formats::check_registry::builtin() {
        for meta in reg.checks() {
            out.insert(
                meta.id.to_uppercase(),
                json!({"severity": meta.severity.as_str()}),
            );
        }
    }
    Value::Object(out)
}

/// Clickable facet row: the issue filter it applies plus the count.
pub fn issue_facet(
    session: &mut ScanSession,
    checks: &[String],
    severity: Option<&str>,
) -> Result<i64, QueryError> {
    session
        .materialize_issues(checks, severity)
        .map_err(|e| QueryError::unavailable(e.reason_code, e.message))?;
    session
        .con
        .query_row("SELECT COUNT(*) FROM issue_urls", [], |r| r.get(0))
        .map_err(|e| QueryError::unavailable("cannot_read", e.to_string()))
}

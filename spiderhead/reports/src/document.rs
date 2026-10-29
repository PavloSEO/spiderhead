//! Audit document loading, contract detection and external-audit normalization.
//!
//! Every writer in this crate reads the flat `findings`/`pages` site-audit contract.
//! An external crawler's `audit.json` (`issues` + `pages[].metrics`) is reshaped into
//! it once, here, by [`normalize_sf_audit`]. Neither contract is rendered unless its
//! declared version marker matches exactly.
//!

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use thiserror::Error;

use spiderhead_core::formats::matches;
use spiderhead_store::audit_v2::AuditV2Reader;
use spiderhead_store::inputs::{resolve_audit_source, AuditSource};

use crate::client_findings::{client_reason, project_finding};
use crate::facts::crawl_domain;
use crate::py;

/// Version marker of the `seohead.site-audit/1` contract.
pub const SITE_AUDIT_SCHEMA: &str = "spiderhead.site-audit/1";

/// Version marker of the external audit.json contract.
pub const SF_AUDIT_SCHEMA_VERSION: &str = "2.0";

/// Report formats `build_report` accepts.
pub const FORMATS: [&str; 6] = ["xlsx", "docx", "csv", "md", "json", "pdf"];

/// Severity name to display title.
pub fn severity_title(severity: &str) -> &str {
    match severity {
        "critical" => "Critical",
        "warning" => "Warning",
        "notice" => "Notice",
        "opportunity" => "Opportunity",
        other => other,
    }
}

/// Error returned at the report boundary: a structured message, never a panic.
///
/// `build_report` turns it into `{"ok": false, "error": ...}`.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct ReportInputError(pub String);

/// The "checks completed" value for a normalized report summary.
///
/// `tools_run` is the one place either accepted contract names the checks that
/// actually executed; a source that cannot supply it says "not reported" rather
/// than zero.
pub fn checks_completed_display(summary: &Value) -> Value {
    match py::get(summary, "tools_run") {
        Some(Value::Array(items)) => json!(items.len()),
        _ => json!("not reported"),
    }
}

/// Load a mapping, JSON file, or validated scan; diagnostics stay outside the audit.
///
/// Returns a document, or a streaming source ([`AuditSource::Streaming`]) for audit.v2
/// input. `diagnostics` collects load warnings.
pub fn load(data: &Value, diagnostics: &mut Vec<Value>) -> Result<AuditSource, ReportInputError> {
    if data.is_object() {
        return Ok(AuditSource::Document(data.clone()));
    }
    let text = match data {
        Value::String(s) => s.clone(),
        other => py::repr(other),
    };
    let path = Path::new(&text);
    if !path.exists() {
        return Err(ReportInputError(format!("audit file not found: {text}")));
    }
    let (source, notices) =
        resolve_audit_source(data).map_err(|e| ReportInputError(e.to_string()))?;
    diagnostics.extend(notices.into_iter().map(Value::Object));
    Ok(source)
}

/// Identify which of the two audit contracts `document` matches.
///
/// Returns `(kind, error)`: `kind` is `None` when the document must be refused;
/// `error` then names the offending or missing marker, or is `None` when neither
/// shape matched at all.
pub fn detect_kind(document: &Value) -> (Option<&'static str>, Option<String>) {
    let present = |key: &str| document.as_object().is_some_and(|m| m.contains_key(key));
    if present("findings") || present("schema") {
        let marker = py::get(document, "schema").and_then(Value::as_str);
        if !matches(marker, SITE_AUDIT_SCHEMA) {
            return (
                None,
                Some(format!(
                    "unsupported or missing 'schema' marker {}; \
                     site-audit reports require exactly '{}'",
                    py::get(document, "schema")
                        .map(py::repr)
                        .unwrap_or_else(|| "None".into()),
                    SITE_AUDIT_SCHEMA
                )),
            );
        }
        let invalid: Vec<&str> = [("findings", "list"), ("pages", "list"), ("summary", "dict")]
            .iter()
            .filter(|(name, expected)| !container_is(document, name, expected))
            .map(|(name, _)| *name)
            .collect();
        if !invalid.is_empty() {
            return (
                None,
                Some(format!(
                    "site-audit has invalid or missing container(s): {}",
                    invalid.join(", ")
                )),
            );
        }
        return (Some("site-audit"), None);
    }
    if present("issues") || present("schema_version") {
        let marker = py::get(document, "schema_version").and_then(Value::as_str);
        if !matches(marker, SF_AUDIT_SCHEMA_VERSION) {
            return (
                None,
                Some(format!(
                    "unsupported or missing 'schema_version' marker {}; \
                     Export Analyzer audits require exactly '{}'",
                    py::get(document, "schema_version")
                        .map(py::repr)
                        .unwrap_or_else(|| "None".into()),
                    SF_AUDIT_SCHEMA_VERSION
                )),
            );
        }
        let invalid: Vec<&str> = [
            ("run", "dict"),
            ("summary", "dict"),
            ("issues", "list"),
            ("pages", "list"),
            ("groups", "list"),
        ]
        .iter()
        .filter(|(name, expected)| !container_is(document, name, expected))
        .map(|(name, _)| *name)
        .collect();
        if !invalid.is_empty() {
            return (
                None,
                Some(format!(
                    "Export Analyzer audit has invalid or missing container(s): {}",
                    invalid.join(", ")
                )),
            );
        }
        return (Some("sf-audit"), None);
    }
    (None, None)
}

fn container_is(document: &Value, name: &str, expected: &str) -> bool {
    match py::get(document, name) {
        Some(value) => match expected {
            "list" => value.is_array(),
            "dict" => value.is_object(),
            _ => false,
        },
        None => false,
    }
}

/// Reshape one external issue into the flat finding contract.
pub fn normalize_sf_issue(issue: &Value) -> Value {
    // Falsy values (None, "", 0, [], {}) fall back to the default.
    let or = |value: Option<&Value>, default: Value| -> Value {
        match value {
            Some(v) if py::truthy(Some(v)) => v.clone(),
            _ => default,
        }
    };
    let get = |key: &str| py::get(issue, key).cloned();
    let mut out = Map::new();
    out.insert("severity".into(), get("severity").unwrap_or(Value::Null));
    out.insert("source".into(), get("source").unwrap_or(json!("")));
    out.insert("url".into(), or(py::get(issue, "target_url"), json!("")));
    out.insert("text".into(), get("message").unwrap_or(json!("")));
    out.insert("check".into(), get("check").unwrap_or(json!("")));
    out.insert(
        "status_code".into(),
        get("status_code").unwrap_or(Value::Null),
    );
    out.insert(
        "occurrences_count".into(),
        get("occurrences_count").unwrap_or(Value::Null),
    );
    out.insert("fix_hint".into(), or(py::get(issue, "fix_hint"), json!("")));
    out.insert(
        "locations".into(),
        or(py::get(issue, "locations"), json!([])),
    );
    out.insert("details".into(), or(py::get(issue, "details"), json!({})));
    out.insert("evidence".into(), or(py::get(issue, "evidence"), json!({})));
    Value::Object(out)
}

/// Reshape one external page into the flat page contract.
pub fn normalize_sf_page(page: &Value) -> Value {
    let metrics = py::get_map(page, "metrics");
    let or = |value: Option<&Value>, default: Value| -> Value {
        match value {
            Some(v) if py::truthy(Some(v)) => v.clone(),
            _ => default,
        }
    };
    let h1_raw = metrics.get("h1");
    let h1_value = match h1_raw {
        Some(Value::Array(items)) if !items.is_empty() => items[0].clone(),
        _ => h1_raw.cloned().unwrap_or(Value::Null),
    };
    let mut out = Map::new();
    out.insert(
        "url".into(),
        py::get(page, "url").cloned().unwrap_or(json!("")),
    );
    out.insert(
        "status".into(),
        py::get(page, "status_code").cloned().unwrap_or(Value::Null),
    );
    out.insert("title".into(), or(metrics.get("title"), json!("")));
    out.insert(
        "title_length".into(),
        or(metrics.get("title_length"), json!("")),
    );
    out.insert(
        "description_length".into(),
        or(metrics.get("desc_length"), json!("")),
    );
    out.insert("h1".into(), or(Some(&h1_value), json!("")));
    out.insert("canonical".into(), or(metrics.get("canonical"), json!("")));
    out.insert("words".into(), or(metrics.get("word_count"), json!(0)));
    out.insert("schema_types".into(), json!(""));
    out.insert("schema_errors".into(), json!(""));
    out.insert("social_missing".into(), json!(""));
    Value::Object(out)
}

/// Reshape an external `audit.json` into the flat site-audit contract.
///
/// Every copied field is evidence already in `document`; none is computed or guessed.
pub fn normalize_sf_audit(document: &Value) -> Value {
    let run = py::get_map(document, "run");
    let summary = py::get_map(document, "summary");
    let totals = summary
        .get("totals")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let by_severity = summary
        .get("by_severity")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let findings: Vec<Value> = py::get_list(document, "issues")
        .iter()
        .map(|issue| {
            let mut normalized = normalize_sf_issue(issue);
            if let Some(segment) = py::get(issue, "__view_segment") {
                normalized
                    .as_object_mut()
                    .map(|m| m.insert("__view_segment".into(), segment.clone()));
            }
            normalized
        })
        .collect();
    let pages: Vec<Value> = py::get_list(document, "pages")
        .iter()
        .map(normalize_sf_page)
        .collect();

    let tools_failed: Vec<Value> =
        run.get("checks_skipped")
            .and_then(Value::as_array)
            .map_or(Vec::new(), |items| {
                items
                    .iter()
                    .map(|item| {
                        json!({"tool": py::get(item, "id").cloned().unwrap_or(Value::Null),
                           "error": py::get(item, "reason").cloned().unwrap_or(Value::Null)})
                    })
                    .collect()
            });
    let checks_disabled: Vec<Value> =
        run.get("checks_disabled")
            .and_then(Value::as_array)
            .map_or(Vec::new(), |items| {
                items
                    .iter()
                    .map(|item| {
                        json!({"id": py::get(item, "id").cloned().unwrap_or(Value::Null),
                           "reason": py::get(item, "reason").cloned().unwrap_or(Value::Null)})
                    })
                    .collect()
            });

    // `by_check` names only checks that found something; `check_coverage` also names
    // checks that ran silently. Together they form `tools_run`.
    let check_coverage = summary
        .get("check_coverage")
        .filter(|v| py::truthy(Some(*v)));
    let tools_run = check_coverage.map(|coverage| {
        let mut names: BTreeSet<String> = py::get_map(summary_value(document), "by_check")
            .keys()
            .cloned()
            .collect();
        for item in py::get_list(coverage, "checks_silent_ids") {
            if let Some(name) = item.as_str() {
                names.insert(name.to_owned());
            }
        }
        json!(names.into_iter().collect::<Vec<_>>())
    });

    let suppressed_issues = match py::get(document, "suppressed_issues") {
        Some(Value::Array(items)) => items.clone(),
        _ => Vec::new(),
    };

    let crawl_valid = match run.get("crawl_valid") {
        None | Some(Value::Null) => true,
        Some(Value::Bool(flag)) => *flag,
        Some(other) => py::truthy(Some(other)),
    };

    let domain = match run.get("project") {
        Some(v) if py::truthy(Some(v)) => v.clone(),
        _ => json!(crawl_domain(&Value::Object(run.clone()))),
    };

    let url = run
        .get("source")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| {
            pages
                .first()
                .and_then(|p| py::filled_str(p, "url"))
                .map(String::from)
        })
        .unwrap_or_default();

    let mut summary_out = Map::new();
    summary_out.insert(
        "evidence_contract".into(),
        summary
            .get("evidence_contract")
            .cloned()
            .unwrap_or(Value::Null),
    );
    summary_out.insert(
        "pages_checked".into(),
        totals
            .get("urls_crawled")
            .cloned()
            .unwrap_or(json!(pages.len())),
    );
    summary_out.insert(
        "findings_total".into(),
        totals
            .get("issues_total")
            .cloned()
            .unwrap_or(json!(findings.len())),
    );
    summary_out.insert(
        "findings_by_severity".into(),
        json!({
            "critical": by_severity.get("critical").cloned().unwrap_or(json!(0)),
            "warning": by_severity.get("warning").cloned().unwrap_or(json!(0)),
            "notice": by_severity.get("notice").cloned().unwrap_or(json!(0)),
        }),
    );
    summary_out.insert("tools_run".into(), tools_run.unwrap_or(Value::Null));
    summary_out.insert("tools_failed".into(), json!(tools_failed));
    summary_out.insert("checks_disabled".into(), json!(checks_disabled));
    summary_out.insert("crawl_valid".into(), json!(crawl_valid));
    summary_out.insert(
        "crawl_invalid_reason".into(),
        if crawl_valid {
            Value::Null
        } else {
            run.get("crawl_invalid_reason")
                .cloned()
                .or_else(|| summary.get("health_score_reason").cloned())
                .unwrap_or(Value::Null)
        },
    );
    summary_out.insert(
        "crawl_partial".into(),
        json!(py::truthy(run.get("crawl_partial"))),
    );
    summary_out.insert(
        "crawl_finish_reason".into(),
        run.get("crawl_finish_reason")
            .cloned()
            .or_else(|| run.get("crawl_stopped_reason").cloned())
            .unwrap_or(Value::Null),
    );
    summary_out.insert(
        "crawl_scope_note".into(),
        summary
            .get("health_score_scope")
            .cloned()
            .unwrap_or(Value::Null),
    );
    summary_out.insert(
        "severity_note".into(),
        summary
            .get("health_score_basis")
            .cloned()
            .unwrap_or(Value::Null),
    );
    summary_out.insert(
        "finding_exclusions".into(),
        summary
            .get("finding_exclusions")
            .cloned()
            .unwrap_or(Value::Null),
    );
    summary_out.insert(
        "finding_exclusion_policy".into(),
        run.get("finding_exclusion_policy")
            .cloned()
            .unwrap_or(Value::Null),
    );

    let mut out = Map::new();
    out.insert("domain".into(), domain);
    out.insert("url".into(), json!(url));
    out.insert(
        "generated_at".into(),
        json!(run
            .get("generated_at")
            .map(py::str_value)
            .unwrap_or_default()),
    );
    out.insert("findings".into(), json!(findings));
    out.insert("pages".into(), json!(pages));
    out.insert("suppressed_issues".into(), json!(suppressed_issues));
    out.insert("summary".into(), Value::Object(summary_out));
    Value::Object(out)
}

fn summary_value(document: &Value) -> &Value {
    document.get("summary").unwrap_or(&Value::Null)
}

/// Forward-only rows over a retained audit.v2 collection.
/// Reads go through [`AuditV2Reader::get_item`]/[`AuditV2Reader::count`] so a
/// bounded report never materializes the collection tail.
pub struct AuditV2Rows<'a> {
    reader: &'a AuditV2Reader,
    pointer: String,
}

impl<'a> AuditV2Rows<'a> {
    /// Wrap `reader` at `pointer` (a JSON-pointer collection name).
    pub fn new(reader: &'a AuditV2Reader, pointer: &str) -> Self {
        Self {
            reader,
            pointer: pointer.to_owned(),
        }
    }

    /// Row count of the collection.
    pub fn len(&self) -> i64 {
        self.reader.count(&self.pointer).unwrap_or(0)
    }

    /// True when the collection has no rows.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The row at `index` in forward order, 0-based.
    pub fn get(&self, index: i64) -> Result<Value, spiderhead_store::error::StoreError> {
        self.reader.get_item(&self.pointer, index)
    }

    /// Forward slice of rows from `start` up to `stop`.
    pub fn slice(
        &self,
        start: i64,
        stop: i64,
    ) -> Result<Vec<Value>, spiderhead_store::error::StoreError> {
        let len = self.len();
        let start = start.clamp(0, len);
        let stop = stop.clamp(start, len);
        (start..stop).map(|i| self.get(i)).collect()
    }

    /// Materialize the collection (bounded callers only).
    pub fn to_vec(&self) -> Result<Vec<Value>, spiderhead_store::error::StoreError> {
        self.reader.iter_collection(&self.pointer)
    }
}

/// Re-iterable display projection over an audit.v2 reader.
///
/// `document` carries the normalized header fields; findings/pages stay lazy and
/// are projected per row through [`Self::get`], [`Self::iter_findings`].
pub struct AuditV2ReportView<'a> {
    /// The normalized header document (`issues`/`pages` intentionally empty).
    pub document: Value,
    /// `reader.count("/pages")` captured at build.
    pub page_count: i64,
    /// `reader.count("/issues")` captured at build.
    pub finding_count: i64,
    /// Underlying validated reader.
    pub reader: &'a AuditV2Reader,
}

impl<'a> AuditV2ReportView<'a> {
    /// Build the projection and cross-check the saved summary against counts.
    pub fn new(reader: &'a AuditV2Reader) -> Result<Self, ReportInputError> {
        let metadata = metadata_value(&reader.header, reader);
        let mut metadata = metadata;
        if let Some(m) = metadata.as_object_mut() {
            m.insert("issues".into(), json!([]));
            m.insert("pages".into(), json!([]));
        }
        let mut document = normalize_sf_audit(&metadata);
        if reader.collections.contains_key("/suppressed_issues") {
            document.as_object_mut().map(|m| {
                m.insert(
                    "suppressed_issues".into(),
                    json!({"$audit_v2_collection": "/suppressed_issues"}),
                )
            });
        }
        let source_summary = py::get_map(&metadata, "summary");
        let totals = source_summary
            .get("totals")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let page_count = reader.count("/pages").unwrap_or(0);
        let issue_count = reader.count("/issues").unwrap_or(0);
        if totals
            .get("urls_crawled")
            .is_some_and(|v| py::is_int(v) && py::to_i64(v) != Some(page_count))
        {
            return Err(ReportInputError(
                "audit.v2 page count disagrees with its saved summary".into(),
            ));
        }
        if totals
            .get("issues_total")
            .is_some_and(|v| py::is_int(v) && py::to_i64(v) != Some(issue_count))
        {
            return Err(ReportInputError(
                "audit.v2 issue count disagrees with its saved summary".into(),
            ));
        }
        let mut severity_counts: std::collections::HashMap<String, i64> =
            std::collections::HashMap::new();
        for pointer in ["/issues", "/pages"] {
            let rows = reader
                .iter_collection(pointer)
                .map_err(|e| ReportInputError(e.to_string()))?;
            for (ordinal, record) in rows.iter().enumerate() {
                if !record.is_object() {
                    return Err(ReportInputError(format!(
                        "audit.v2 {pointer} record {} must be an object",
                        ordinal + 1
                    )));
                }
                if pointer == "/issues" {
                    *severity_counts
                        .entry(py::str_value(
                            record.get("severity").unwrap_or(&Value::Null),
                        ))
                        .or_insert(0) += 1;
                } else if ordinal == 0 && !py::truthy(document.get("url")) {
                    document.as_object_mut().map(|m| {
                        m.insert(
                            "url".into(),
                            record.get("url").cloned().unwrap_or(json!("")),
                        )
                    });
                }
            }
        }
        let declared = source_summary
            .get("by_severity")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut by_severity = Map::new();
        for name in ["critical", "warning", "notice"] {
            let counted = severity_counts.get(name).copied().unwrap_or(0);
            if let Some(decl) = declared.get(name) {
                if py::is_int(decl) && py::to_i64(decl) != Some(counted) {
                    return Err(ReportInputError(format!(
                        "audit.v2 {name} issue count disagrees with its saved summary"
                    )));
                }
            }
            by_severity.insert(
                name.to_owned(),
                declared.get(name).cloned().unwrap_or(json!(counted)),
            );
        }
        if let Some(summary) = document.get_mut("summary").and_then(Value::as_object_mut) {
            summary.insert(
                "pages_checked".into(),
                totals
                    .get("urls_crawled")
                    .cloned()
                    .unwrap_or(json!(page_count)),
            );
            summary.insert(
                "findings_total".into(),
                totals
                    .get("issues_total")
                    .cloned()
                    .unwrap_or(json!(issue_count)),
            );
            summary.insert("findings_by_severity".into(), Value::Object(by_severity));
            let checks_disabled: Vec<Value> = summary
                .get("checks_disabled")
                .and_then(Value::as_array)
                .map_or(Vec::new(), |items| {
                    items
                        .iter()
                        .filter(|item| item.is_object())
                        .map(|item| {
                            let mut row = item.clone();
                            if let Some(m) = row.as_object_mut() {
                                m.insert(
                                    "reason".into(),
                                    json!(client_reason(
                                        py::get(item, "reason").unwrap_or(&Value::Null)
                                    )),
                                );
                            }
                            row
                        })
                        .collect()
                });
            summary.insert("checks_disabled".into(), json!(checks_disabled));
            let tools_failed: Vec<Value> = summary
                .get("tools_failed")
                .and_then(Value::as_array)
                .map_or(Vec::new(), |items| {
                    items
                        .iter()
                        .filter(|item| item.is_object())
                        .map(|item| {
                            let mut row = item.clone();
                            if let Some(m) = row.as_object_mut() {
                                m.insert(
                                    "error".into(),
                                    json!(client_reason(
                                        py::get(item, "error").unwrap_or(&Value::Null)
                                    )),
                                );
                            }
                            row
                        })
                        .collect()
                });
            summary.insert("tools_failed".into(), json!(tools_failed));
        }
        Ok(Self {
            document,
            page_count,
            finding_count: issue_count,
            reader,
        })
    }

    /// `view.get(name)`: projected findings/pages streams, document fields otherwise.
    pub fn get(&self, name: &str) -> Option<Value> {
        match name {
            "findings" => self.reader.iter_collection("/issues").ok().map(|rows| {
                json!(rows
                    .iter()
                    .map(|issue| project_finding(&normalize_sf_issue(issue)))
                    .collect::<Vec<_>>())
            }),
            "pages" => self
                .reader
                .iter_collection("/pages")
                .ok()
                .map(|rows| json!(rows.iter().map(normalize_sf_page).collect::<Vec<_>>())),
            "summary" => self.document.get("summary").cloned(),
            _ => self.document.get(name).cloned(),
        }
    }

    /// Iterate projected findings, optionally of one severity.
    pub fn iter_findings(&self, severity: Option<&str>) -> Vec<Value> {
        self.reader
            .iter_collection("/issues")
            .unwrap_or_default()
            .iter()
            .map(|issue| project_finding(&normalize_sf_issue(issue)))
            .filter(|finding| {
                severity.is_none()
                    || py::get(finding, "severity").and_then(Value::as_str) == severity
            })
            .collect()
    }

    /// `suppressed_issues` — a retained rows view when the collection exists.
    pub fn suppressed_rows(&self) -> Option<AuditV2Rows<'a>> {
        self.reader
            .collections
            .contains_key("/suppressed_issues")
            .then(|| AuditV2Rows::new(self.reader, "/suppressed_issues"))
    }
}

/// Header fields the report never materializes (the unbounded row collections).
/// Every other marker resolves eagerly; writers iterate both forms the same way,
/// so content is identical.
const LAZY_POINTERS: [&str; 4] = ["/issues", "/pages", "/groups", "/suppressed_issues"];

fn metadata_value(value: &Value, reader: &AuditV2Reader) -> Value {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(pointer)) =
                map.get(spiderhead_store::audit_v2::COLLECTION_MARKER)
            {
                if LAZY_POINTERS.contains(&pointer.as_str()) {
                    return value.clone();
                }
                return reader
                    .iter_collection(pointer)
                    .map(Value::Array)
                    .unwrap_or(Value::Null);
            }
            Value::Object(
                map.iter()
                    .map(|(k, item)| (k.clone(), metadata_value(item, reader)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| metadata_value(item, reader))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Make a path segment safe: unsafe characters become `_`, whitespace runs `-`,
/// underscores collapse, capped at 160 characters, with `"file"` as fallback.
pub fn safe_segment(value: &Value) -> String {
    let text = match value {
        Value::Null => String::new(),
        other => py::str_value(other),
    };
    let mut out = String::with_capacity(text.len().min(160));
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => {
                // collapse a run of unsafe chars/underscores into one '_'
                while matches!(chars.peek(), Some(c) if *c == '_'
                    || matches!(c, '<'|'>'|':'|'"'|'/'|'\\'|'|'|'?'|'*'|'\u{0}'..='\u{1f}'))
                {
                    chars.next();
                }
                out.push('_');
            }
            c if c <= '\u{1f}' => {
                while matches!(chars.peek(), Some(c) if *c == '_'
                    || matches!(c, '<'|'>'|':'|'"'|'/'|'\\'|'|'|'?'|'*'|'\u{0}'..='\u{1f}'))
                {
                    chars.next();
                }
                out.push('_');
            }
            c if c.is_whitespace() => {
                while matches!(chars.peek(), Some(c) if c.is_whitespace()) {
                    chars.next();
                }
                out.push('-');
            }
            '_' => {
                while matches!(chars.peek(), Some('_')) {
                    chars.next();
                }
                out.push('_');
            }
            c => out.push(c),
        }
    }
    let truncated: String = out.chars().take(160).collect();
    if truncated.is_empty() {
        "file".to_owned()
    } else {
        truncated
    }
}

/// Default output path: `audit-<domain>.<fmt>` when no path is given.
pub fn default_target(domain: &str, fmt: &str, path: Option<&str>) -> PathBuf {
    match path {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from(format!("audit-{domain}.{fmt}")),
    }
}

/// The surface every writer calls on a report document.
///
/// Writers receive either the normalized document or an [`AuditV2ReportView`];
/// both expose the same accessors.
pub trait ReportDoc {
    /// `document.get(key)` — cloned field value.
    fn get(&self, key: &str) -> Option<Value>;
    /// Projected findings, optionally filtered to one severity.
    fn findings(&self, severity: Option<&str>) -> Vec<Value>;
    /// Normalized page rows.
    fn pages(&self) -> Vec<Value>;
    /// Suppressed issues as plain rows (`[]` when absent).
    fn suppressed_issues(&self) -> Vec<Value>;
    /// Saved `/pages` count for streaming views; `None` for dict documents.
    fn page_count(&self) -> Option<i64>;
    /// `True` for the audit.v2 streaming projection.
    fn is_streaming(&self) -> bool;
}

impl ReportDoc for Value {
    fn get(&self, key: &str) -> Option<Value> {
        self.as_object().and_then(|m| m.get(key)).cloned()
    }

    fn findings(&self, severity: Option<&str>) -> Vec<Value> {
        py::get_list(self, "findings")
            .iter()
            .filter(|f| {
                severity.is_none() || py::get(f, "severity").and_then(Value::as_str) == severity
            })
            .cloned()
            .collect()
    }

    fn pages(&self) -> Vec<Value> {
        py::get_list(self, "pages").to_vec()
    }

    fn suppressed_issues(&self) -> Vec<Value> {
        py::get_list(self, "suppressed_issues").to_vec()
    }

    fn page_count(&self) -> Option<i64> {
        None
    }

    fn is_streaming(&self) -> bool {
        false
    }
}

impl<'a> ReportDoc for AuditV2ReportView<'a> {
    fn get(&self, key: &str) -> Option<Value> {
        self.get(key)
    }

    fn findings(&self, severity: Option<&str>) -> Vec<Value> {
        self.iter_findings(severity)
    }

    fn pages(&self) -> Vec<Value> {
        self.get("pages")
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
    }

    fn suppressed_issues(&self) -> Vec<Value> {
        self.suppressed_rows()
            .and_then(|rows| rows.to_vec().ok())
            .unwrap_or_default()
    }

    fn page_count(&self) -> Option<i64> {
        Some(self.page_count)
    }

    fn is_streaming(&self) -> bool {
        true
    }
}

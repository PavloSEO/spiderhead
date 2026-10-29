//! Multi-site fact export: one comparable table of facts across domains.
//!
//! Facts are leaves in five states (`measured`, `absent`, `partial`, `unavailable`,
//! `not_requested`). The module never computes scores or ratios and never touches the network.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};
use thiserror::Error;

use spiderhead_core::net::{normalize_domain, registrable_domain};

use crate::document::{detect_kind, load, SF_AUDIT_SCHEMA_VERSION, SITE_AUDIT_SCHEMA};
use crate::py;

/// Export contract version.
pub const SCHEMA_VERSION: &str = "facts.v1";

/// All states a fact leaf may hold.
pub const FACT_STATES: [&str; 5] = [
    "measured",
    "absent",
    "partial",
    "unavailable",
    "not_requested",
];

/// Reason recorded on the two index-count facts.
pub const INDEX_COUNT_REASON: &str = "index counts are not implementable from a configured provider: dataforseo.py keeps only rank/url/domain/title, and yandex_cloud.py never reads <found>; no configured provider returns a result count";

const NULL_VALUE_STATES: [&str; 2] = ["unavailable", "not_requested"];

const INDEX_COUNT_KEYS: [(&str, &str); 2] = [
    ("index_count_google", "dataforseo"),
    ("index_count_yandex", "yandex_cloud"),
];

/// Malformed `fact` call: a caller bug, not a property of the site.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct FactError(pub String);

fn is_not(key: &str) -> &'static str {
    match key {
        "crawl_pages_crawled" => "not the number of pages the site actually has",
        "crawl_html_indexable" => "not a measure of ranking eligibility",
        "crawl_issues_total" => "not a health score or a ranking signal",
        "crawl_issues_critical" => "not weighted or normalised against site size",
        "crawl_issues_warning" => "not weighted or normalised against site size",
        "crawl_issues_notice" => "not weighted or normalised against site size",
        "crawl_checks_available" => "not a percentage of checks that ran",
        "crawl_checks_skipped" => "not evidence the skipped checks would have found issues",
        "crawl_checks_disabled" => "not evidence the disabled checks would have found issues",
        "site_pages_checked" => "not the number of pages the site actually has",
        "site_findings_total" => "not a health score or a ranking signal",
        "site_findings_critical" => "not weighted or normalised against site size",
        "site_findings_warning" => "not weighted or normalised against site size",
        "site_findings_notice" => "not weighted or normalised against site size",
        "site_tools_run_count" => "not evidence the tools that ran found nothing wrong",
        "site_tools_failed_count" => "not detected is not proof of absence for the failed tools",
        "index_count_google" | "index_count_yandex" => {
            "not detected is not proof of absence; this tool never asked"
        }
        _ => "",
    }
}

/// Build one fact leaf.
///
/// `reason`, `bound` and `note` are optional; `paid` marks a paid-provider fact.
#[allow(clippy::too_many_arguments)]
pub fn fact(
    value: Value,
    state: &str,
    source: &str,
    method: &str,
    reason: Option<&str>,
    bound: Option<&str>,
    paid: bool,
    note: Option<&str>,
) -> Result<Value, FactError> {
    if !FACT_STATES.contains(&state) {
        return Err(FactError(format!(
            "unknown fact state {state:?}; expected one of {FACT_STATES:?}"
        )));
    }
    if NULL_VALUE_STATES.contains(&state) && !value.is_null() {
        return Err(FactError(format!(
            "state {state:?} must not carry a value, got {value}"
        )));
    }
    if state != "measured" && reason.is_none_or(|r| r.is_empty()) {
        return Err(FactError(format!("state {state:?} requires a reason")));
    }
    if state == "partial" && bound.is_none_or(|b| b.is_empty()) {
        return Err(FactError("state 'partial' requires a bound".into()));
    }
    Ok(json!({
        "value": value,
        "state": state,
        "source": source,
        "method": method,
        "reason": reason,
        "bound": bound,
        "paid": paid,
        "note": note,
    }))
}

fn unavailable(key: &str, source: &str, method: &str, reason: &str) -> Value {
    let mut built = fact(
        Value::Null,
        "unavailable",
        source,
        method,
        Some(reason),
        None,
        false,
        None,
    )
    .unwrap_or(Value::Null);
    built
        .as_object_mut()
        .map(|m| m.insert("is_not".into(), json!(is_not(key))));
    built
}

fn stamped(key: &str, built: Value) -> Value {
    let mut built = built;
    built
        .as_object_mut()
        .map(|m| m.insert("is_not".into(), json!(is_not(key))));
    built
}

/// The domain a crawl audit's `run` block was produced for.
///
/// Tries `start_url`, then `source` (both must contain `://`), then `project`.
pub fn crawl_domain(run: &Value) -> String {
    for key in ["start_url", "source"] {
        if let Some(value) = py::get(run, key) {
            let text = py::str_value(value);
            if !text.is_empty() && text.contains("://") {
                if let Ok(parsed) = url::Url::parse(&text) {
                    let netloc = match parsed.port() {
                        Some(port) => format!("{}:{port}", parsed.host_str().unwrap_or_default()),
                        None => parsed.host_str().unwrap_or_default().to_owned(),
                    };
                    return normalize_domain(&netloc);
                }
            }
        }
    }
    if let Some(project) = py::get(run, "project") {
        let text = py::str_value(project);
        if !text.is_empty() {
            return normalize_domain(&text);
        }
    }
    String::new()
}

fn load_document(
    path_or_doc: &Value,
    label: &str,
    field: &str,
) -> Result<(Value, String, String), FactError> {
    let mut sink = Vec::new();
    let document = load(path_or_doc, &mut sink).map_err(|e| FactError(e.to_string()))?;
    let document = match document {
        spiderhead_store::inputs::AuditSource::Document(value) => value,
        spiderhead_store::inputs::AuditSource::Streaming(_) => {
            return Err(FactError(format!(
                "{field} for site {label:?} must be a JSON object, got AuditV2Reader"
            )))
        }
    };
    if !document.is_object() {
        return Err(FactError(format!(
            "{field} for site {label:?} must be a JSON object, got {}",
            type_name(&document)
        )));
    }
    let (kind, marker_error) = detect_kind(&document);
    let Some(kind) = kind else {
        return Err(FactError(marker_error.unwrap_or_else(|| {
            format!(
                "{field} for site {label:?} does not match a recognized audit schema \
                 ({SITE_AUDIT_SCHEMA:?} or Export Analyzer {SF_AUDIT_SCHEMA_VERSION:?})"
            )
        })));
    };
    let doc_domain = if kind == "site-audit" {
        normalize_domain(&py::str_value(
            py::get(&document, "domain").unwrap_or(&Value::Null),
        ))
    } else {
        crawl_domain(py::get(&document, "run").unwrap_or(&Value::Null))
    };
    let label_domain = normalize_domain(label);
    let file_name = match path_or_doc {
        Value::String(path) => path.clone(),
        _ => format!("<inline {field}>"),
    };
    if doc_domain.is_empty() {
        return Err(FactError(format!(
            "{field} for site {label:?} (file {file_name:?}) carries no discoverable domain"
        )));
    }
    if doc_domain != label_domain {
        return Err(FactError(format!(
            "site-identity mismatch: file {file_name:?} filed under label {label:?} \
             actually belongs to domain {doc_domain:?}"
        )));
    }
    Ok((document, kind.to_owned(), doc_domain))
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn measured_or_absent(value: &Value, source: &str, method: &str, absent_reason: &str) -> Value {
    let (state, reason) = if py::truthy(Some(value)) {
        ("measured", None)
    } else {
        ("absent", Some(absent_reason))
    };
    fact(
        value.clone(),
        state,
        source,
        method,
        reason,
        None,
        false,
        None,
    )
    .unwrap_or(Value::Null)
}

fn crawl_facts(document: &Value) -> Map<String, Value> {
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
    let coverage = summary.get("check_coverage");

    let mut facts = Map::new();

    if let Some(value) = totals.get("urls_crawled") {
        if py::truthy(run.get("crawl_partial")) {
            let reason = run
                .get("crawl_finish_reason")
                .or_else(|| run.get("crawl_stopped_reason"))
                .map(py::str_value)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "crawl stopped before completion".to_owned());
            let built = fact(
                value.clone(),
                "partial",
                "sf_audit_run",
                "summary.totals.urls_crawled",
                Some(&reason),
                Some(&format!(
                    ">= {} (crawl did not finish)",
                    py::str_value(value)
                )),
                false,
                None,
            )
            .unwrap_or(Value::Null);
            facts.insert("crawl_pages_crawled".into(), built);
        } else {
            let built = fact(
                value.clone(),
                "measured",
                "sf_audit_run",
                "summary.totals.urls_crawled",
                None,
                None,
                false,
                None,
            )
            .unwrap_or(Value::Null);
            facts.insert("crawl_pages_crawled".into(), built);
        }
    } else {
        facts.insert(
            "crawl_pages_crawled".into(),
            unavailable(
                "crawl_pages_crawled",
                "sf_audit_run",
                "summary.totals.urls_crawled",
                "totals.urls_crawled not present in this document",
            ),
        );
    }

    if let Some(value) = totals.get("html_indexable") {
        facts.insert(
            "crawl_html_indexable".into(),
            measured_or_absent(
                value,
                "sf_audit_run",
                "summary.totals.html_indexable",
                "crawl found zero indexable HTML pages",
            ),
        );
    } else {
        facts.insert(
            "crawl_html_indexable".into(),
            unavailable(
                "crawl_html_indexable",
                "sf_audit_run",
                "summary.totals.html_indexable",
                "totals.html_indexable not present in this document",
            ),
        );
    }

    if let Some(value) = totals.get("issues_total") {
        facts.insert(
            "crawl_issues_total".into(),
            measured_or_absent(
                value,
                "sf_audit_run",
                "summary.totals.issues_total",
                "crawl recorded zero issues",
            ),
        );
    } else {
        facts.insert(
            "crawl_issues_total".into(),
            unavailable(
                "crawl_issues_total",
                "sf_audit_run",
                "summary.totals.issues_total",
                "totals.issues_total not present in this document",
            ),
        );
    }

    for level in ["critical", "warning", "notice"] {
        let key = format!("crawl_issues_{level}");
        if let Some(value) = by_severity.get(level) {
            facts.insert(
                key.clone(),
                measured_or_absent(
                    value,
                    "sf_audit_run",
                    &format!("summary.by_severity.{level}"),
                    &format!("crawl recorded zero {level} issues"),
                ),
            );
        } else {
            facts.insert(
                key.clone(),
                unavailable(
                    &key,
                    "sf_audit_run",
                    &format!("summary.by_severity.{level}"),
                    "by_severity not present in this document",
                ),
            );
        }
    }

    for (name, coverage_key) in [
        ("crawl_checks_available", "checks_available"),
        ("crawl_checks_skipped", "checks_skipped"),
        ("crawl_checks_disabled", "checks_disabled"),
    ] {
        match coverage.and_then(Value::as_object) {
            Some(map) => match map.get(coverage_key) {
                Some(value) => {
                    facts.insert(
                        name.into(),
                        measured_or_absent(
                            value,
                            "sf_audit_run",
                            &format!("summary.check_coverage.{coverage_key}"),
                            &format!("{coverage_key} is zero"),
                        ),
                    );
                }
                None => {
                    facts.insert(
                        name.into(),
                        unavailable(
                            name,
                            "sf_audit_run",
                            &format!("summary.check_coverage.{coverage_key}"),
                            "check_coverage present but missing this field",
                        ),
                    );
                }
            },
            None => {
                facts.insert(
                    name.into(),
                    unavailable(
                        name,
                        "sf_audit_run",
                        &format!("summary.check_coverage.{coverage_key}"),
                        "check_coverage not present in this document",
                    ),
                );
            }
        }
    }

    facts
        .into_iter()
        .map(|(key, value)| (key.clone(), stamped(&key, value)))
        .collect()
}

fn site_audit_facts(document: &Value) -> Map<String, Value> {
    let summary = py::get_map(document, "summary");
    let by_severity = summary
        .get("findings_by_severity")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let tools_run = summary.get("tools_run");
    let tools_failed = summary.get("tools_failed");

    let mut facts = Map::new();

    if let Some(value) = summary.get("pages_checked") {
        facts.insert(
            "site_pages_checked".into(),
            measured_or_absent(
                value,
                "seo_site_audit",
                "summary.pages_checked",
                "site audit checked zero pages",
            ),
        );
    } else {
        facts.insert(
            "site_pages_checked".into(),
            unavailable(
                "site_pages_checked",
                "seo_site_audit",
                "summary.pages_checked",
                "pages_checked not present in this document",
            ),
        );
    }

    if let Some(value) = summary.get("findings_total") {
        facts.insert(
            "site_findings_total".into(),
            measured_or_absent(
                value,
                "seo_site_audit",
                "summary.findings_total",
                "site audit recorded zero findings",
            ),
        );
    } else {
        facts.insert(
            "site_findings_total".into(),
            unavailable(
                "site_findings_total",
                "seo_site_audit",
                "summary.findings_total",
                "findings_total not present in this document",
            ),
        );
    }

    for level in ["critical", "warning", "notice"] {
        let key = format!("site_findings_{level}");
        if let Some(value) = by_severity.get(level) {
            facts.insert(
                key.clone(),
                measured_or_absent(
                    value,
                    "seo_site_audit",
                    &format!("summary.findings_by_severity.{level}"),
                    &format!("site audit recorded zero {level} findings"),
                ),
            );
        } else {
            facts.insert(
                key.clone(),
                unavailable(
                    &key,
                    "seo_site_audit",
                    &format!("summary.findings_by_severity.{level}"),
                    "findings_by_severity not present in this document",
                ),
            );
        }
    }

    if let Some(items) = tools_run.and_then(Value::as_array) {
        facts.insert(
            "site_tools_run_count".into(),
            measured_or_absent(
                &json!(items.len()),
                "seo_site_audit",
                "len(summary.tools_run)",
                "no site-level tool ran",
            ),
        );
    } else {
        facts.insert(
            "site_tools_run_count".into(),
            unavailable(
                "site_tools_run_count",
                "seo_site_audit",
                "len(summary.tools_run)",
                "tools_run not present in this document",
            ),
        );
    }

    if let Some(items) = tools_failed.and_then(Value::as_array) {
        facts.insert(
            "site_tools_failed_count".into(),
            measured_or_absent(
                &json!(items.len()),
                "seo_site_audit",
                "len(summary.tools_failed)",
                "no site-level tool failed",
            ),
        );
    } else {
        facts.insert(
            "site_tools_failed_count".into(),
            unavailable(
                "site_tools_failed_count",
                "seo_site_audit",
                "len(summary.tools_failed)",
                "tools_failed not present in this document",
            ),
        );
    }

    facts
        .into_iter()
        .map(|(key, value)| (key.clone(), stamped(&key, value)))
        .collect()
}

const ALL_DOCUMENT_KEYS: [(&str, &str, &str); 16] = [
    (
        "crawl_pages_crawled",
        "sf_audit_run",
        "summary.totals.urls_crawled",
    ),
    (
        "crawl_html_indexable",
        "sf_audit_run",
        "summary.totals.html_indexable",
    ),
    (
        "crawl_issues_total",
        "sf_audit_run",
        "summary.totals.issues_total",
    ),
    (
        "crawl_issues_critical",
        "sf_audit_run",
        "summary.by_severity.critical",
    ),
    (
        "crawl_issues_warning",
        "sf_audit_run",
        "summary.by_severity.warning",
    ),
    (
        "crawl_issues_notice",
        "sf_audit_run",
        "summary.by_severity.notice",
    ),
    (
        "crawl_checks_available",
        "sf_audit_run",
        "summary.check_coverage.checks_available",
    ),
    (
        "crawl_checks_skipped",
        "sf_audit_run",
        "summary.check_coverage.checks_skipped",
    ),
    (
        "crawl_checks_disabled",
        "sf_audit_run",
        "summary.check_coverage.checks_disabled",
    ),
    (
        "site_pages_checked",
        "seo_site_audit",
        "summary.pages_checked",
    ),
    (
        "site_findings_total",
        "seo_site_audit",
        "summary.findings_total",
    ),
    (
        "site_findings_critical",
        "seo_site_audit",
        "summary.findings_by_severity.critical",
    ),
    (
        "site_findings_warning",
        "seo_site_audit",
        "summary.findings_by_severity.warning",
    ),
    (
        "site_findings_notice",
        "seo_site_audit",
        "summary.findings_by_severity.notice",
    ),
    (
        "site_tools_run_count",
        "seo_site_audit",
        "len(summary.tools_run)",
    ),
    (
        "site_tools_failed_count",
        "seo_site_audit",
        "len(summary.tools_failed)",
    ),
];

fn index_count_facts() -> Map<String, Value> {
    INDEX_COUNT_KEYS
        .iter()
        .map(|(key, source)| {
            (
                key.to_string(),
                stamped(
                    key,
                    fact(
                        Value::Null,
                        "not_requested",
                        source,
                        "none",
                        Some(INDEX_COUNT_REASON),
                        None,
                        false,
                        None,
                    )
                    .unwrap_or(Value::Null),
                ),
            )
        })
        .collect()
}

/// Build a `facts.v1` export from `{label, crawl_audit, site_audit}` descriptors.
///
/// `crawl_audit`/`site_audit` are optional and may be documents or JSON-file
/// paths. Refuses a duplicate registrable domain (checked before any load) and
/// a site-identity mismatch.
pub fn build_facts_export(sites: &[Value]) -> Result<Value, FactError> {
    if sites.is_empty() {
        return Err(FactError(
            "sites required: at least one {label, ...} descriptor".into(),
        ));
    }
    let labels: Vec<String> = sites
        .iter()
        .map(|site| py::str_value(py::get(site, "label").unwrap_or(&Value::Null)))
        .collect();
    if labels.iter().any(|label| label.is_empty()) {
        return Err(FactError(
            "every site descriptor requires a non-empty label".into(),
        ));
    }

    let domain_by_label: BTreeMap<String, String> = labels
        .iter()
        .map(|label| (label.clone(), normalize_domain(label)))
        .collect();
    let mut seen_domain: BTreeMap<String, String> = BTreeMap::new();
    for label in &labels {
        let domain = &domain_by_label[label];
        if let Some(prior) = seen_domain.get(domain) {
            return Err(FactError(format!(
                "labels {prior:?} and {label:?} both resolve to domain {domain:?}; \
                 refuse the same site under two labels rather than comparing it with itself"
            )));
        }
        seen_domain.insert(domain.clone(), label.clone());
    }

    let registrable_by_label: BTreeMap<String, String> = domain_by_label
        .iter()
        .map(|(label, domain)| (label.clone(), registrable_domain(domain)))
        .collect();
    let mut shared_registrable: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for label in &labels {
        shared_registrable
            .entry(registrable_by_label[label].clone())
            .or_default()
            .push(label.clone());
    }
    shared_registrable.retain(|_, group| group.len() > 1);

    let mut rows = Vec::new();
    for site in sites {
        let label = py::str_value(py::get(site, "label").unwrap_or(&Value::Null));
        let mut facts = Map::new();
        let mut note_parts: Vec<String> = Vec::new();
        let mut documents_read = 0;

        let crawl_input = py::get(site, "crawl_audit");
        let site_input = py::get(site, "site_audit");

        if let Some(input) = crawl_input.filter(|v| !v.is_null()) {
            let (document, kind, _domain) = load_document(input, &label, "crawl_audit")?;
            if kind != "sf-audit" {
                return Err(FactError(format!(
                    "crawl_audit for site {label:?} is a {kind:?} document, not a crawl audit"
                )));
            }
            facts.extend(crawl_facts(&document));
            documents_read += 1;
        }

        if let Some(input) = site_input.filter(|v| !v.is_null()) {
            let (document, kind, _domain) = load_document(input, &label, "site_audit")?;
            if kind != "site-audit" {
                return Err(FactError(format!(
                    "site_audit for site {label:?} is a {kind:?} document, not a site audit"
                )));
            }
            facts.extend(site_audit_facts(&document));
            documents_read += 1;
        }

        if documents_read == 0 {
            note_parts.push("no readable documents were supplied for this site".into());
            for (key, source, method) in ALL_DOCUMENT_KEYS {
                facts.insert(
                    key.to_string(),
                    unavailable(
                        key,
                        source,
                        method,
                        "no readable documents were supplied for this site",
                    ),
                );
            }
        }

        facts.extend(index_count_facts());

        let reg = &registrable_by_label[&label];
        if let Some(group) = shared_registrable.get(reg) {
            let others: Vec<String> = group
                .iter()
                .filter(|other| *other != &label)
                .cloned()
                .collect();
            note_parts.push(format!("shares registrable domain {reg:?} with {others:?}"));
        }

        rows.push(json!({
            "label": label,
            "domain": domain_by_label[&label],
            "note": if note_parts.is_empty() { Value::Null } else { json!(note_parts.join("; ")) },
            "facts": facts,
        }));
    }

    Ok(json!({"schema_version": SCHEMA_VERSION, "sites": rows}))
}

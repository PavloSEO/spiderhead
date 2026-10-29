//! Project one validated run into BI dataset rows: pages, findings, links,
//! metrics, cohorts, and coverage.
//! Store reads from the evidence-join store are a named gap: the provider store is never
//! populated, so the cursor-backed observation stream is unreachable here.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::{json, Map, Value};

use crate::bi::dates::IsoDate;
use std::path::{Path, PathBuf};

use crate::bi::inputs::{audit_source, provider_file, read_audit_input, scan_source};
use crate::bi::schema::{
    canonical_json, digest_parts, json_value, page_key, page_url, BIExportError, ProviderInput,
    RunInput, DEFAULT_BYTES_PER_PARTITION, DEFAULT_MAX_OUTPUT_BYTES, DEFAULT_MAX_SCAN_BYTES,
    DEFAULT_ROWS_PER_PARTITION, MAX_BYTES_PER_PARTITION, MAX_OUTPUT_BYTES,
    MAX_PROVIDER_JOIN_TOTAL_BYTES, MAX_PROVIDER_SOURCES, MAX_ROWS_PER_PARTITION, MAX_SCAN_BYTES,
};
use crate::py;

fn err(msg: impl Into<String>) -> BIExportError {
    BIExportError::new(msg)
}

/// One body or link metric from a page, aware of the audit shape.
pub fn audit_metric(page: &Value, name: &str, kind: &str) -> Value {
    if kind == "site-audit" {
        if name == "meta_description" {
            if let Some(v) = page.get("description") {
                return v.clone();
            }
        }
        return page.get(name).cloned().unwrap_or(Value::Null);
    }
    let metrics = page.get("metrics");
    let Some(metrics) = metrics.and_then(Value::as_object) else {
        return Value::Null;
    };
    let aliases = [
        ("meta_description", "meta_description"),
        ("crawl_depth", "crawl_depth"),
        ("response_time", "response_time"),
        ("word_count", "word_count"),
        ("text_ratio", "text_ratio"),
        ("title", "title"),
        ("h1", "h1"),
        ("canonical", "canonical"),
        ("inlinks", "inlinks"),
        ("outlinks", "outlinks"),
        ("external_outlinks", "external_outlinks"),
    ];
    let alias = aliases
        .iter()
        .find(|(k, _)| *k == name)
        .map(|(_, v)| *v)
        .unwrap_or(name);
    let value = metrics.get(alias).cloned().unwrap_or(Value::Null);
    if name == "h1" && value.is_array() {
        return value
            .as_array()
            .and_then(|a| a.first().cloned())
            .unwrap_or_else(|| json!(""));
    }
    value
}

/// Whether the page carries the metric at all.
pub fn audit_metric_present(page: &Value, name: &str, kind: &str) -> bool {
    if kind == "site-audit" {
        return page.get(name).is_some();
    }
    let Some(metrics) = page.get("metrics").and_then(Value::as_object) else {
        return false;
    };
    let aliases = [
        ("meta_description", "meta_description"),
        ("crawl_depth", "crawl_depth"),
        ("response_time_seconds", "response_time"),
        ("word_count", "word_count"),
        ("text_ratio", "text_ratio"),
        ("title", "title"),
        ("h1", "h1"),
        ("canonical", "canonical"),
        ("inlinks", "inlinks"),
        ("unique_inlinks", "unique_inlinks"),
        ("internal_outlinks", "outlinks"),
        ("external_outlinks", "external_outlinks"),
        ("size_bytes", "size_bytes"),
    ];
    let alias = aliases
        .iter()
        .find(|(k, _)| *k == name)
        .map(|(_, v)| *v)
        .unwrap_or(name);
    metrics.contains_key(alias)
}

/// Body value of an audit metric.
pub fn audit_body_value(
    value: &Value,
    present: bool,
    skip_reason: Option<&str>,
    field_name: &str,
) -> (Value, &'static str, Option<String>) {
    if skip_reason.is_some() && (!present || value.is_null() || value == &json!("")) {
        return (Value::Null, "unavailable", skip_reason.map(str::to_owned));
    }
    if !present {
        return (
            Value::Null,
            "unknown",
            Some(format!("audit does not include {field_name} for this page")),
        );
    }
    if value.is_null() {
        return (
            Value::Null,
            "unknown",
            Some(format!(
                "audit field {field_name} is null without a measured-state marker"
            )),
        );
    }
    (value.clone(), "measured", None)
}

/// `(value, state, reason)` triple used by the page projection.
type Cell3 = (Value, &'static str, Option<String>);

/// One projected `pages` row with state and reason companions.
pub fn page_row(run: &RunInput, page: &Value, ordinal: usize) -> Result<Value, BIExportError> {
    let url = match page_url(page) {
        Some(url) => url,
        None => {
            return Err(err(format!(
                "page source row {ordinal} has no non-empty URL"
            )))
        }
    };
    let (url_key, key_state, key_reason) = page_key(Some(&url));
    let body_marker = page.get("body_unavailable");
    let (body_state, body_reason): (&'static str, Option<String>) =
        if body_marker.is_none() && run.source_kind == "scan" {
            (
                "unknown",
                Some("scan does not retain body availability for this row".into()),
            )
        } else if py::truthy(body_marker) {
            (
                "unavailable",
                Some(py::str_value(body_marker.unwrap_or(&Value::Null))),
            )
        } else if run.source_kind == "scan" {
            use unicode_casefold::UnicodeCaseFold;
            let raw = page.get("content_type");
            let text = if py::truthy(raw) {
                py::str_value(raw.unwrap_or(&Value::Null))
            } else {
                String::new()
            };
            let content_type: String = text
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .case_fold()
                .collect();
            if content_type != "text/html" && content_type != "application/xhtml+xml" {
                (
                    "not_applicable",
                    Some("response is not a supported HTML media type".into()),
                )
            } else if page.get("document_id").is_none_or(|v| v.is_null()) {
                (
                    "unavailable",
                    Some("scan page has no parsed document evidence".into()),
                )
            } else {
                ("measured", None)
            }
        } else {
            (
                "unknown",
                Some("audit document does not declare per-page body retention".into()),
            )
        };

    let body_value = |value: &Value| -> Cell3 {
        if body_state == "not_applicable" {
            return (Value::Null, "not_applicable", body_reason.clone());
        }
        if body_state != "measured" {
            return (Value::Null, body_state, body_reason.clone());
        }
        if value.is_null() {
            return (
                Value::Null,
                "unavailable",
                Some("source audit does not include this page field".into()),
            );
        }
        (value.clone(), "measured", None)
    };
    let source_value = |name: &str, value: &Value| -> Cell3 {
        if value.is_null() {
            return (
                Value::Null,
                "unavailable",
                Some(format!("source does not include {name} for this page")),
            );
        }
        (value.clone(), "measured", None)
    };

    let mut link_state: String = if run.source_kind != "scan" {
        "measured".into()
    } else {
        run.links_source_state.clone()
    };
    let mut link_reason = run.links_source_reason.clone();

    let mut states: HashMap<String, Cell3> = HashMap::new();
    let representation: Value;
    let source_fields: Value;
    let source_ordinal: Value;
    let mut outlinks_total_override: Option<Cell3> = None;

    if run.source_kind == "scan" {
        let audit_page = page
            .get("_bi_audit_page")
            .filter(|v| v.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        let mut values: HashMap<String, Value> = [
            ("status_code", page.get("status_code")),
            ("content_type", page.get("content_type")),
            ("indexability", audit_page.get("indexability")),
            ("indexability_status", audit_page.get("indexability_status")),
            ("crawl_depth", page.get("crawl_depth")),
            ("response_time_seconds", page.get("response_time")),
            ("size_bytes", page.get("size_bytes")),
            ("title", page.get("title")),
            ("meta_description", page.get("meta_description")),
            ("h1", page.get("h1")),
            ("canonical", page.get("canonical")),
            ("word_count", page.get("word_count")),
            ("text_ratio", page.get("text_ratio")),
            ("inlinks", page.get("inlinks")),
            ("unique_inlinks", page.get("unique_inlinks")),
            ("outlinks_total", page.get("outlinks")),
            ("external_outlinks", page.get("external_outlinks")),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.cloned().unwrap_or(Value::Null)))
        .collect();
        let body_names = [
            "title",
            "meta_description",
            "h1",
            "canonical",
            "word_count",
            "text_ratio",
        ];
        let outlinks_total = page.get("outlinks").cloned().unwrap_or(Value::Null);
        let external_outlinks = page
            .get("external_outlinks")
            .cloned()
            .unwrap_or(Value::Null);
        let internal_outlinks = if py::is_int(&outlinks_total) && py::is_int(&external_outlinks) {
            json!(std::cmp::max(
                py::to_i64(&outlinks_total).unwrap_or(0)
                    - py::to_i64(&external_outlinks).unwrap_or(0),
                0,
            ))
        } else {
            Value::Null
        };
        let _ = &mut values;
        values.insert("internal_outlinks".into(), internal_outlinks.clone());
        let link_cap = run
            .run_metadata
            .get("capabilities")
            .and_then(|c| c.get("links"))
            .cloned()
            .unwrap_or_else(|| json!({}));
        link_state = link_cap
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        if run.links_source_state == "unavailable" {
            link_state = "unavailable".into();
        }
        link_reason = run.links_source_reason.clone();
        let link_names = [
            "inlinks",
            "unique_inlinks",
            "internal_outlinks",
            "outlinks_total",
            "external_outlinks",
        ];
        for (name, value) in &values {
            let state = if body_names.contains(&name.as_str()) {
                body_value(value)
            } else if link_names.contains(&name.as_str()) {
                if !["complete", "partial"].contains(&link_state.as_str()) {
                    (
                        Value::Null,
                        "unavailable",
                        Some(
                            link_reason
                                .clone()
                                .unwrap_or_else(|| "link evidence unavailable".into()),
                        ),
                    )
                } else {
                    source_value(name, value)
                }
            } else {
                source_value(name, value)
            };
            states.insert(name.clone(), state);
        }
        states.insert(
            "internal_outlinks".into(),
            if !["complete", "partial"].contains(&link_state.as_str())
                || internal_outlinks.is_null()
            {
                (
                    Value::Null,
                    "unavailable",
                    Some(
                        link_reason
                            .clone()
                            .unwrap_or_else(|| "outlink counts were not retained".into()),
                    ),
                )
            } else {
                (
                    internal_outlinks,
                    "derived",
                    Some(
                        "total outlinks minus external outlinks, matching the crawl evidence projection"
                            .into(),
                    ),
                )
            },
        );
        let indexability = values.get("indexability").cloned().unwrap_or(Value::Null);
        states.insert(
            "indexability".into(),
            source_value("indexability", &indexability),
        );
        representation = page.get("representation").cloned().unwrap_or(Value::Null);
        source_ordinal = page
            .get("page_ordinal")
            .cloned()
            .unwrap_or_else(|| json!(ordinal));
        source_fields = {
            let mut out = Map::new();
            for (key, value) in py::map(page).cloned().unwrap_or_default() {
                if ![
                    "url",
                    "url_id",
                    "page_ordinal",
                    "document_id",
                    "_bi_audit_page",
                ]
                .contains(&key.as_str())
                {
                    out.insert(key, json_value(&value));
                }
            }
            Value::Object(out)
        };
    } else {
        source_ordinal = json!(ordinal);
        let values: HashMap<String, Value> = [
            (
                "status_code",
                page.get("status_code")
                    .or_else(|| page.get("status"))
                    .cloned()
                    .unwrap_or(Value::Null),
            ),
            (
                "content_type",
                page.get("content_type").cloned().unwrap_or(Value::Null),
            ),
            (
                "indexability",
                page.get("indexability").cloned().unwrap_or(Value::Null),
            ),
            (
                "indexability_status",
                page.get("indexability_status")
                    .cloned()
                    .unwrap_or(Value::Null),
            ),
            (
                "crawl_depth",
                audit_metric(page, "crawl_depth", &run.source_kind),
            ),
            (
                "response_time_seconds",
                audit_metric(page, "response_time", &run.source_kind),
            ),
            (
                "size_bytes",
                audit_metric(page, "size_bytes", &run.source_kind),
            ),
            ("title", audit_metric(page, "title", &run.source_kind)),
            (
                "meta_description",
                audit_metric(page, "meta_description", &run.source_kind),
            ),
            ("h1", audit_metric(page, "h1", &run.source_kind)),
            (
                "canonical",
                audit_metric(page, "canonical", &run.source_kind),
            ),
            (
                "word_count",
                audit_metric(page, "word_count", &run.source_kind),
            ),
            (
                "text_ratio",
                audit_metric(page, "text_ratio", &run.source_kind),
            ),
            ("inlinks", audit_metric(page, "inlinks", &run.source_kind)),
            (
                "unique_inlinks",
                audit_metric(page, "unique_inlinks", &run.source_kind),
            ),
            (
                "internal_outlinks",
                audit_metric(page, "outlinks", &run.source_kind),
            ),
            ("outlinks_total", Value::Null),
            (
                "external_outlinks",
                audit_metric(page, "external_outlinks", &run.source_kind),
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
        let body_checks: [(&str, &str); 6] = [
            ("title", "TITLE_MISSING"),
            ("meta_description", "DESC_MISSING"),
            ("h1", "H1_MISSING"),
            ("canonical", "CANONICAL_MISSING"),
            ("word_count", "THIN_CONTENT"),
            ("text_ratio", "LOW_TEXT_RATIO"),
        ];
        for (name, value) in &values {
            let check = body_checks.iter().find(|(n, _)| n == name).map(|(_, c)| *c);
            let state = if let Some(check) = check {
                let source_name = if run.source_kind == "site-audit" && name == "meta_description" {
                    "description"
                } else {
                    name.as_str()
                };
                audit_body_value(
                    value,
                    audit_metric_present(page, source_name, &run.source_kind),
                    run.run_metadata
                        .get("check_skips")
                        .and_then(Value::as_object)
                        .and_then(|skips| skips.get(check))
                        .and_then(Value::as_str),
                    name,
                )
            } else {
                source_value(name, value)
            };
            states.insert(name.clone(), state);
        }
        if run.source_kind == "sf-audit"
            && values.get("status_code") == Some(&json!(0))
            && !values
                .get("status_code")
                .is_some_and(|v| v.is_boolean() || !v.is_number())
        {
            states.insert(
                "status_code".into(),
                (
                    Value::Null,
                    "unavailable",
                    Some(
                        "SF audit uses status_code 0 as a no-response sentinel, not an HTTP status"
                            .into(),
                    ),
                ),
            );
        }
        let (internal_value, internal_state, internal_reason) = states
            .get("internal_outlinks")
            .cloned()
            .unwrap_or((Value::Null, "unknown", None));
        let (external_value, external_state, external_reason) = states
            .get("external_outlinks")
            .cloned()
            .unwrap_or((Value::Null, "unknown", None));
        if internal_state == "measured" && external_state == "measured" {
            let sum =
                py::to_i64(&internal_value).unwrap_or(0) + py::to_i64(&external_value).unwrap_or(0);
            outlinks_total_override = Some((
                json!(sum),
                "derived",
                Some("source internal plus external outlink counts".into()),
            ));
        } else {
            outlinks_total_override = Some((
                Value::Null,
                "unavailable",
                Some(
                    internal_reason
                        .or(external_reason)
                        .unwrap_or_else(|| "source does not expose both outlink counts".into()),
                ),
            ));
        }
        representation = audit_metric(page, "representation", &run.source_kind);
        source_fields = json_value(page);
    }

    if !py::is_int(&source_ordinal) {
        return Err(err(format!(
            "page source row {ordinal} has a non-integer source ordinal"
        )));
    }
    let int_or_null = |name: &str, label: &str| -> Result<(), BIExportError> {
        let value = &states
            .get(name)
            .map(|(v, _, _)| v.clone())
            .unwrap_or(Value::Null);
        if !(value.is_null() || py::is_int(value)) {
            return Err(err(format!(
                "page {} has a non-integer {label}",
                py::repr_str(&url)
            )));
        }
        Ok(())
    };
    int_or_null("status_code", "status code")?;
    int_or_null("crawl_depth", "crawl depth")?;
    int_or_null("word_count", "word count")?;
    int_or_null("size_bytes", "byte size")?;

    let repr_str_value = representation.as_str().filter(|s| !s.is_empty());
    let mut row = json!({
        "run_id": run.run_id,
        "url_observation_id": &digest_parts(&[&run.run_id, &url])[..32],
        "source_ordinal": source_ordinal,
        "url": url,
        "url_key": url_key,
        "url_key_state": key_state,
        "url_key_reason": key_reason,
        "body_evidence_state": body_state,
        "body_evidence_reason": body_reason,
        "representation": representation,
        "representation_state": if repr_str_value.is_some() { "measured" } else { "unavailable" },
        "representation_reason": Value::Null,
        "source_fields_json": source_fields,
    });
    if repr_str_value.is_none() {
        row["representation_reason"] = json!("source does not include page representation");
    }
    for output_name in [
        "status_code",
        "content_type",
        "indexability",
        "indexability_status",
        "crawl_depth",
        "response_time_seconds",
        "size_bytes",
        "title",
        "meta_description",
        "h1",
        "canonical",
        "word_count",
        "text_ratio",
    ] {
        let (value, state, reason) =
            states
                .get(output_name)
                .cloned()
                .unwrap_or((Value::Null, "unknown", None));
        row[output_name] = value;
        let state_name = match output_name {
            "response_time_seconds" => "response_time",
            "size_bytes" => "size",
            other => other,
        };
        row[format!("{state_name}_state")] = json!(state);
        row[format!("{state_name}_reason")] = reason.map_or(Value::Null, Value::String);
    }
    if let Some(state) = outlinks_total_override {
        states.insert("outlinks_total".into(), state);
    }
    for output_name in [
        "inlinks",
        "unique_inlinks",
        "internal_outlinks",
        "outlinks_total",
        "external_outlinks",
    ] {
        let (value, state, reason) =
            states
                .get(output_name)
                .cloned()
                .unwrap_or((Value::Null, "unknown", None));
        row[output_name] = value;
        row[format!("{output_name}_state")] = json!(state);
        row[format!("{output_name}_reason")] = reason.map_or(Value::Null, Value::String);
    }
    let link_states: Vec<&str> = [
        "inlinks",
        "unique_inlinks",
        "internal_outlinks",
        "outlinks_total",
        "external_outlinks",
    ]
    .iter()
    .map(|name| states.get(*name).map(|(_, s, _)| *s).unwrap_or("unknown"))
    .collect();
    let unavailable_link_counts = link_states.iter().filter(|s| **s == "unavailable").count();
    if unavailable_link_counts == link_states.len() {
        row["link_counts_state"] = json!("unavailable");
        row["link_counts_reason"] = link_reason.clone().map_or(
            json!("one or more link counts are unavailable"),
            Value::String,
        );
    } else if link_state == "partial" || unavailable_link_counts > 0 {
        row["link_counts_state"] = json!("partial");
        row["link_counts_reason"] = if unavailable_link_counts > 0 {
            link_reason.clone().map_or(
                json!("some link counts are unavailable in the retained page projection"),
                Value::String,
            )
        } else {
            link_reason
                .clone()
                .map_or(json!("link counts cover a partial crawl"), Value::String)
        };
    } else {
        row["link_counts_state"] = json!("measured");
        row["link_counts_reason"] = Value::Null;
    }
    Ok(row)
}

/// One projected `findings` row.
pub fn finding_source(
    run: &RunInput,
    finding: &Value,
    ordinal: usize,
    group_map: &crate::bi_index::GroupIndex,
) -> Result<Value, BIExportError> {
    let sf_shape = ["sf-audit", "scan"].contains(&run.source_kind.as_str());
    let check_id = finding.get("check").cloned().unwrap_or(Value::Null);
    let url = if sf_shape {
        finding.get("target_url")
    } else {
        finding.get("url")
    }
    .cloned()
    .unwrap_or(Value::Null);
    let message = if sf_shape {
        finding.get("message")
    } else {
        finding.get("text")
    }
    .cloned()
    .unwrap_or(Value::Null);
    let details = finding.get("details").cloned().unwrap_or(Value::Null);
    let locations = finding.get("locations").cloned().unwrap_or(Value::Null);
    let evidence = finding.get("evidence").cloned().unwrap_or(Value::Null);
    let source = finding.get("source").cloned().unwrap_or(Value::Null);
    let status = finding.get("status_code").cloned().unwrap_or(Value::Null);
    let occurrences = finding
        .get("occurrences_count")
        .cloned()
        .unwrap_or(Value::Null);
    let fix_hint = finding.get("fix_hint").cloned().unwrap_or(Value::Null);
    let group_id = finding.get("group_id").cloned().unwrap_or(Value::Null);
    let severity = finding.get("severity").cloned().unwrap_or(Value::Null);
    let raw_finding_id = finding.get("id").cloned().unwrap_or(Value::Null);
    let kind = if sf_shape {
        "audit_finding"
    } else {
        "site_audit_finding"
    };
    let status_present = finding.get("status_code").is_some();
    let occurrences_present = finding.get("occurrences_count").is_some();
    let group = match group_id.as_str() {
        Some(id) => group_map.get(id)?,
        None => None,
    };
    let source_finding_id = raw_finding_id
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let finding_id = digest_parts(&[
        &run.run_id,
        &ordinal.to_string(),
        source_finding_id.as_deref().unwrap_or(""),
        &canonical_json(finding).map_err(|e| err(e.0))?,
    ])[..32]
        .to_owned();
    let (url_key, key_state, key_reason) = page_key(url.as_str());
    let (check_id, check_state, check_reason): (Value, &'static str, Option<&'static str>) =
        match check_id.as_str() {
            Some(id) if !id.is_empty() => (check_id, "measured", None),
            _ => (
                Value::Null,
                "unavailable",
                Some("source finding has no check identifier"),
            ),
        };
    if !status.is_null() && !py::is_int(&status) {
        return Err(err(format!(
            "finding {} status_code must be an integer or null",
            py::repr_str(&finding_id)
        )));
    }
    if !occurrences.is_null() && !py::is_int(&occurrences) {
        return Err(err(format!(
            "finding {} occurrences_count must be an integer or null",
            py::repr_str(&finding_id)
        )));
    }
    let (url_state, url_reason) = if url.as_str().is_some_and(|u| !u.is_empty()) {
        ("measured", Value::Null)
    } else {
        (
            "not_applicable",
            json!("source finding is not attached to one URL"),
        )
    };
    let status_state = if !status.is_null() {
        "measured"
    } else if status_present {
        "unavailable"
    } else {
        "not_applicable"
    };
    let status_reason = if status_present && status.is_null() {
        json!("source finding status code is null")
    } else if !status.is_null() {
        Value::Null
    } else {
        json!("finding contract does not attach a status code")
    };
    let occurrence_state = if !occurrences.is_null() {
        "measured"
    } else if occurrences_present {
        "unavailable"
    } else {
        "not_applicable"
    };
    let occurrence_reason = if occurrences_present && occurrences.is_null() {
        json!("source finding occurrence count is null")
    } else if !occurrences.is_null() {
        Value::Null
    } else {
        json!("finding contract does not declare an occurrence count")
    };
    let message_state = if message.as_str().is_some_and(|m| !m.is_empty()) {
        "measured"
    } else {
        "unavailable"
    };
    let message_reason = if message_state == "measured" {
        Value::Null
    } else {
        json!("source finding has no message text")
    };
    let severity_state = if severity.as_str().is_some_and(|s| !s.is_empty()) {
        "measured"
    } else {
        "unknown"
    };
    let severity_reason = if severity_state == "measured" {
        Value::Null
    } else {
        json!("source severity is not declared")
    };
    let source_state = if source.as_str().is_some_and(|s| !s.is_empty()) {
        "measured"
    } else {
        "unknown"
    };
    let source_reason = if source_state == "measured" {
        Value::Null
    } else {
        json!("source finding origin is not declared")
    };
    let fix_state = if fix_hint.as_str().is_some_and(|f| !f.is_empty()) {
        "measured"
    } else {
        "not_supplied"
    };
    let group = group.unwrap_or(Value::Null);
    let group_url_count = if group.is_null() {
        Value::Null
    } else {
        group.get("member_count").cloned().unwrap_or_else(|| {
            group
                .get("urls")
                .and_then(Value::as_array)
                .map(|a| json!(a.len()))
                .unwrap_or(Value::Null)
        })
    };
    Ok(json!({
        "run_id": run.run_id,
        "finding_id": finding_id,
        "source_finding_id": source_finding_id,
        "source_ordinal": ordinal,
        "finding_kind": kind,
        "check_id": check_id,
        "check_state": check_state,
        "check_reason": check_reason,
        "severity": severity,
        "severity_state": severity_state,
        "severity_reason": severity_reason,
        "url": url,
        "url_state": url_state,
        "url_reason": url_reason,
        "url_key": url_key,
        "url_key_state": key_state,
        "url_key_reason": key_reason,
        "message": message,
        "message_state": message_state,
        "message_reason": message_reason,
        "source": source,
        "source_state": source_state,
        "source_reason": source_reason,
        "status_code": status,
        "status_code_state": status_state,
        "status_code_reason": status_reason,
        "occurrences_count": occurrences,
        "occurrences_count_state": occurrence_state,
        "occurrences_count_reason": occurrence_reason,
        "fix_hint": fix_hint,
        "fix_hint_state": fix_state,
        "fix_hint_reason": if fix_state == "measured" { Value::Null } else { json!("source finding has no fix hint") },
        "group_id": group_id,
        "group_state": if !group.is_null() { "grouped" } else { "not_grouped" },
        "group_value": group.get("value").cloned().unwrap_or(Value::Null),
        "group_url_count": group_url_count,
        "group_urls_json": group.get("urls").cloned().unwrap_or(Value::Null),
        "locations_json": if locations.is_null() { json!([]) } else { locations },
        "details_json": if details.is_null() { json!({}) } else { details },
        "evidence_json": if evidence.is_null() { json!({}) } else { evidence },
        "source_finding_json": finding,
    }))
}

/// `(row_index, natural_key_sha256)` of one normalized row.
pub fn row_id(row: &Value) -> Result<(i64, String), BIExportError> {
    let index = row.get("row_index");
    let digest = row.get("natural_key_sha256");
    let valid_hash = digest.and_then(Value::as_str).is_some_and(|d| {
        d.len() == 64
            && d.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    });
    let index_ok = index.is_some_and(py::is_int) && py::to_i64(index.unwrap()).unwrap_or(-1) >= 0;
    if !index_ok || !valid_hash {
        return Err(err(
            "normalized provider row lacks a valid row_index/natural_key_sha256 identity",
        ));
    }
    Ok((
        py::to_i64(index.unwrap()).unwrap_or(0),
        digest.and_then(Value::as_str).unwrap_or("").to_owned(),
    ))
}

/// One provider metric observation: the row, the metric spec, and the typed entry under it.
pub struct Observation {
    /// `(row_index, natural_key_sha256)` identity of the normalized row.
    pub identity: (i64, String),
    /// The normalized source row.
    pub row: Value,
    /// The declared metric spec this observation carries.
    pub metric: Value,
    /// The typed `{state, value, ...}` entry for `metric.name` on the row.
    pub entry: Value,
    /// The row's `dimensions` map.
    pub dimensions: Value,
    /// `matched` / `external_only` / `unkeyable` population.
    pub population: String,
    /// Sorted matched page URLs (non-store joins only).
    pub matched_urls: Vec<String>,
    /// `len(matched_urls)` or the store-declared count.
    pub matched_page_count: i64,
    /// Store provenance dict (unreachable without `provider.store`).
    pub matched_url_provenance: Option<Value>,
    /// The row's `url` provenance object.
    pub url: Value,
}

/// Provider observations for the non-store path only; the provider store is never
/// populated in this port, so the cursor-backed stream branch is unreachable.
pub fn provider_observations(
    provider: &ProviderInput,
    join: &Value,
) -> Result<(Vec<Observation>, Value), BIExportError> {
    let _ = provider;
    let evidence = join.get("evidence").cloned().unwrap_or(Value::Null);
    let metrics = evidence
        .get("metrics")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let declared_dimensions = evidence
        .get("dimensions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut row_map: BTreeMap<(i64, String), Value> = BTreeMap::new();
    let mut matches: HashMap<(i64, String), Vec<String>> = HashMap::new();
    let mut populations: HashMap<(i64, String), &'static str> = HashMap::new();
    for item in join
        .get("matched")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        if !item.is_object() || !item.get("rows").is_some_and(Value::is_array) {
            return Err(err("provider join matched entry has malformed rows"));
        }
        let page_url = item
            .get("url")
            .filter(|u| py::truthy(Some(u)))
            .cloned()
            .or_else(|| item.get("page").and_then(|p| p.get("url")).cloned());
        for row in item
            .get("rows")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            if !row.is_object() {
                return Err(err("provider join contains a non-object normalized row"));
            }
            let identity = row_id(&row)?;
            if let Some(previous) = row_map.get(&identity) {
                if canonical_json(previous).map_err(|e| err(e.0))?
                    != canonical_json(&row).map_err(|e| err(e.0))?
                {
                    return Err(err(
                        "provider join repeats a source row identity with different content",
                    ));
                }
            }
            row_map.insert(identity.clone(), row);
            if let Some(url) = page_url
                .as_ref()
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                matches
                    .entry(identity.clone())
                    .or_default()
                    .push(url.to_owned());
            }
            populations.insert(identity, "matched");
        }
    }
    for item in join
        .get("external_only")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let row = item.get("row").cloned().unwrap_or(Value::Null);
        if !row.is_object() {
            return Err(err("provider join external_only entry lacks a row"));
        }
        let identity = row_id(&row)?;
        if let Some(previous) = row_map.get(&identity) {
            if canonical_json(previous).map_err(|e| err(e.0))?
                != canonical_json(&row).map_err(|e| err(e.0))?
            {
                return Err(err(
                    "provider join repeats a source row identity with different content",
                ));
            }
        }
        row_map.insert(identity.clone(), row);
        populations.entry(identity).or_insert("external_only");
    }
    for row in join
        .get("unkeyable_rows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        if !row.is_object() {
            return Err(err("provider join unkeyable_rows contains a non-object"));
        }
        let identity = row_id(&row)?;
        if let Some(previous) = row_map.get(&identity) {
            if canonical_json(previous).map_err(|e| err(e.0))?
                != canonical_json(&row).map_err(|e| err(e.0))?
            {
                return Err(err(
                    "provider join repeats a source row identity with different content",
                ));
            }
        }
        row_map.insert(identity.clone(), row);
        populations.entry(identity).or_insert("unkeyable");
    }
    let summary_rows = join
        .get("summary")
        .and_then(|s| s.get("rows"))
        .and_then(Value::as_i64);
    if summary_rows != Some(row_map.len() as i64) {
        return Err(err(
            "provider join populations do not conserve the normalized source row count",
        ));
    }
    let matched_rows = populations.values().filter(|p| **p == "matched").count() as i64;
    let summary_matched = join
        .get("summary")
        .and_then(|s| s.get("matched_rows"))
        .and_then(Value::as_i64);
    if summary_matched != Some(matched_rows) {
        return Err(err(
            "provider join matched evidence rows disagree with its summary",
        ));
    }
    let mut observations = Vec::new();
    let mut dimensions_seen: std::collections::BTreeSet<String> = declared_dimensions
        .iter()
        .filter_map(|d| d.as_str().map(str::to_owned))
        .collect();
    let metric_names: std::collections::BTreeSet<String> = metrics
        .iter()
        .filter_map(|m| m.get("name").and_then(Value::as_str).map(str::to_owned))
        .collect();
    for (identity, row) in &row_map {
        let dimensions_row = row
            .get("dimensions")
            .filter(|d| py::truthy(Some(d)))
            .cloned()
            .unwrap_or_else(|| json!({}));
        if !dimensions_row.is_object() {
            return Err(err("normalized provider dimensions must be an object"));
        }
        for name in dimensions_row
            .as_object()
            .map(|m| m.keys())
            .into_iter()
            .flatten()
        {
            dimensions_seen.insert(name.clone());
        }
        let row_metrics = row.get("metrics").cloned().unwrap_or(Value::Null);
        if !row_metrics.is_object()
            || row_metrics
                .as_object()
                .map(|m| m.keys().cloned().collect::<std::collections::BTreeSet<_>>())
                != Some(metric_names.clone())
        {
            return Err(err(
                "provider row metric names differ from the declared source metrics",
            ));
        }
        for metric in &metrics {
            let name = metric.get("name").and_then(Value::as_str).unwrap_or("");
            let entry = row_metrics.get(name).cloned().unwrap_or(Value::Null);
            let state = entry.get("state").and_then(Value::as_str);
            if !entry.is_object() || !["measured", "unavailable"].contains(&state.unwrap_or("")) {
                return Err(err(format!(
                    "provider metric {} has an invalid availability state",
                    py::repr_str(name)
                )));
            }
            let value = entry.get("value").cloned().unwrap_or(Value::Null);
            if state == Some("measured") {
                let finite = match &value {
                    Value::Bool(_) => false,
                    Value::Number(n) => n.as_f64().is_some_and(|f| f.is_finite()),
                    _ => false,
                };
                if !finite {
                    return Err(err(format!(
                        "measured provider metric {} must be a finite number",
                        py::repr_str(name)
                    )));
                }
            } else if !value.is_null() {
                return Err(err(format!(
                    "unavailable provider metric {} must have a null value",
                    py::repr_str(name)
                )));
            }
            let url = row
                .get("url")
                .filter(|u| py::truthy(Some(u)))
                .cloned()
                .unwrap_or_else(|| json!({}));
            if !url.is_object() {
                return Err(err("normalized provider URL provenance must be an object"));
            }
            let mut matched_urls = matches.get(identity).cloned().unwrap_or_default();
            matched_urls.sort();
            observations.push(Observation {
                identity: identity.clone(),
                row: row.clone(),
                metric: metric.clone(),
                entry,
                dimensions: dimensions_row.clone(),
                population: populations
                    .get(identity)
                    .unwrap_or(&"unkeyable")
                    .to_string(),
                matched_page_count: matched_urls.len() as i64,
                matched_urls,
                matched_url_provenance: None,
                url,
            });
        }
    }
    let info = json!({
        "dimensions": dimensions_seen.into_iter().collect::<Vec<_>>(),
        "metric_specs": metrics,
        "summary": join.get("summary").cloned().unwrap_or(Value::Null),
        "crawl": join.get("crawl").cloned().unwrap_or_else(|| json!({})),
        "evidence": evidence,
        "compatibility": provider.compatibility,
    });
    Ok((observations, info))
}

/// Interpret collection provenance without upgrading an import default to evidence.
pub fn provider_collection(header: &Value) -> Result<Value, BIExportError> {
    let collection = header
        .get("collection")
        .filter(|c| py::truthy(Some(c)))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let origins = header
        .get("field_origins")
        .filter(|o| py::truthy(Some(o)))
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !collection.is_object() || !origins.is_object() {
        return Err(err(
            "provider source has malformed collection metadata or field origins",
        ));
    }
    let mut collection = collection.as_object().cloned().unwrap_or_default();
    let origins = origins.as_object().cloned().unwrap_or_default();
    let mut state = collection
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let mut reason: Option<String> = None;
    if state.is_empty()
        || !["declared", "envelope"].contains(
            &origins
                .get("collection_state")
                .and_then(Value::as_str)
                .unwrap_or(""),
        )
    {
        state = "unknown".into();
        reason = Some("source did not establish collection_state".into());
    } else if state == "complete" {
        let flags = ["sampled", "thresholded", "truncated"];
        let unknown: Vec<&str> = flags
            .iter()
            .copied()
            .filter(|flag| {
                !(collection.get(*flag).is_some_and(Value::is_boolean)
                    && ["declared", "envelope"]
                        .contains(&origins.get(*flag).and_then(Value::as_str).unwrap_or("")))
            })
            .collect();
        let degraded: Vec<&str> = flags
            .iter()
            .copied()
            .filter(|flag| collection.get(*flag) == Some(&json!(true)) && !unknown.contains(flag))
            .collect();
        if !degraded.is_empty() {
            state = "partial".into();
            reason = Some(format!(
                "provider collection is degraded: {}",
                degraded.join(", ")
            ));
        }
        if !unknown.is_empty() {
            state = if degraded.is_empty() {
                "unknown".into()
            } else {
                "partial".into()
            };
            let unknown_msg = format!(
                "source did not explicitly establish collection flags: {}",
                unknown.join(", ")
            );
            reason = Some(match reason {
                Some(r) => format!("{r}; {unknown_msg}"),
                None => unknown_msg,
            });
        }
    }
    if let Some(reason) = reason {
        collection.insert("state".into(), json!(state));
        let existing = collection.get("reason").and_then(Value::as_str);
        collection.insert(
            "reason".into(),
            json!(match existing {
                Some(e) if !e.is_empty() => format!("{reason}; {e}"),
                _ => reason,
            }),
        );
        return Ok(Value::Object(collection));
    }
    Ok(Value::Object(collection))
}

/// One projected `metrics` row per provider observation and declared metric.
pub fn metric_rows<'a>(
    run: &'a RunInput,
    sources: &'a [(&'a ProviderInput, &'a Vec<Observation>, &'a Value)],
    dimensions: &'a Map<String, Value>,
) -> impl Iterator<Item = Result<Value, BIExportError>> + 'a {
    sources
        .iter()
        .flat_map(move |(provider, observations, info)| {
            let mut rows: Vec<Result<Value, BIExportError>> = Vec::new();
            if provider.error_state.is_some() {
                return rows.into_iter();
            }
            let header = info.get("evidence").cloned().unwrap_or(Value::Null);
            let period = header
                .get("period")
                .filter(|p| py::truthy(Some(p)))
                .cloned()
                .unwrap_or_else(|| json!({}));
            if py::truthy(Some(&period)) && !period.is_object() {
                rows.push(Err(err(format!(
                    "provider source {} has malformed period metadata",
                    provider.source_id
                ))));
                return rows.into_iter();
            }
            let start_date = period.get("start_date").cloned().unwrap_or(Value::Null);
            let end_date = period.get("end_date").cloned().unwrap_or(Value::Null);
            if !start_date.is_null() {
                let valid = IsoDate::parse(start_date.as_str().unwrap_or("")).is_some()
                    && IsoDate::parse(end_date.as_str().unwrap_or("")).is_some();
                if !valid {
                    rows.push(Err(err(format!(
                        "provider source {} has invalid period dates",
                        provider.source_id
                    ))));
                    return rows.into_iter();
                }
            }
            let collection = match provider_collection(&header) {
                Ok(c) => c,
                Err(e) => {
                    rows.push(Err(e));
                    return rows.into_iter();
                }
            };
            let field_origins = header
                .get("field_origins")
                .filter(|o| py::truthy(Some(o)))
                .cloned()
                .unwrap_or_else(|| json!({}));
            if !field_origins.is_object() {
                rows.push(Err(err(format!(
                    "provider source {} has malformed field origins",
                    provider.source_id
                ))));
                return rows.into_iter();
            }
            let fact = |name: &str, value: &Value| -> (&'static str, Option<String>) {
                if value.is_null() {
                    return ("unknown", Some(format!("source did not establish {name}")));
                }
                let origin = field_origins.get(name).and_then(Value::as_str);
                if origin == Some("unknown") {
                    return (
                        "unknown",
                        Some(format!("{name} has an unknown provenance origin")),
                    );
                }
                ("measured", None)
            };
            let period_state = if !start_date.is_null() && !end_date.is_null() {
                "measured"
            } else {
                "unknown"
            };
            let period_reason = if period_state == "measured" {
                Value::Null
            } else {
                json!("inclusive period boundaries are unknown")
            };
            for obs in observations.iter() {
                let row = &obs.row;
                let entry = &obs.entry;
                let url = &obs.url;
                let (index, natural_hash) = &obs.identity;
                let metric_name = obs.metric.get("name").and_then(Value::as_str).unwrap_or("");
                let dims_row = &obs.dimensions;
                let key = if url.get("state").and_then(Value::as_str) == Some("keyed") {
                    url.get("normalized").cloned().unwrap_or(Value::Null)
                } else {
                    Value::Null
                };
                let observation_id = digest_parts(&[
                    &provider.source_id,
                    &index.to_string(),
                    natural_hash,
                    metric_name,
                ])[..32]
                    .to_owned();
                let unit = obs
                    .metric
                    .get("unit")
                    .filter(|u| py::truthy(Some(u)))
                    .or_else(|| entry.get("unit"))
                    .cloned()
                    .unwrap_or(Value::Null);
                let unit_known = py::truthy(
                    obs.metric
                        .get("unit")
                        .filter(|u| py::truthy(Some(u)))
                        .or_else(|| entry.get("unit")),
                );
                let (tz_state, tz_reason) =
                    fact("timezone", header.get("timezone").unwrap_or(&Value::Null));
                let (attr_state, attr_reason) = fact(
                    "attribution",
                    header.get("attribution").unwrap_or(&Value::Null),
                );
                let (engine_state, engine_reason) = fact(
                    "search_engine",
                    header.get("search_engine").unwrap_or(&Value::Null),
                );
                let (type_state, type_reason) = fact(
                    "search_type",
                    header.get("search_type").unwrap_or(&Value::Null),
                );
                let dims_state: Map<String, Value> = dims_row
                .as_object()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|(name, value)| {
                    (
                        name,
                        json!({
                            "state": if value.is_null() { "unavailable" } else { "measured" },
                            "reason": if value.is_null() { "source dimension is null" } else { "" },
                        }),
                    )
                })
                .map(|(k, v)| {
                    let mut v = v;
                    if v.get("reason") == Some(&json!("")) {
                        v["reason"] = Value::Null;
                    }
                    (k, v)
                })
                .collect();
                let mut result_map = Map::new();
                {
                    let mut put = |name: &str, value: Value| {
                        result_map.insert(name.to_owned(), value);
                    };
                    put("run_id", json!(run.run_id));
                    put("metric_observation_id", json!(observation_id));
                    put("provider_source_id", json!(provider.source_id));
                    put(
                        "provider",
                        header.get("provider").cloned().unwrap_or(Value::Null),
                    );
                    put(
                        "operation",
                        header.get("operation").cloned().unwrap_or(Value::Null),
                    );
                    put(
                        "reporting_identity",
                        header
                            .get("reporting_identity")
                            .cloned()
                            .unwrap_or(Value::Null),
                    );
                    put(
                        "privacy",
                        header.get("privacy").cloned().unwrap_or(Value::Null),
                    );
                    put("metric_name", json!(metric_name));
                    put("metric_unit", unit.clone());
                    put(
                        "metric_unit_state",
                        json!(if unit_known { "measured" } else { "unknown" }),
                    );
                    put(
                        "metric_unit_reason",
                        if unit_known {
                            Value::Null
                        } else {
                            json!("unit is not declared")
                        },
                    );
                    put(
                        "value_number",
                        if entry.get("state").and_then(Value::as_str) == Some("measured") {
                            entry.get("value").cloned().unwrap_or(Value::Null)
                        } else {
                            Value::Null
                        },
                    );
                    put(
                        "value_state",
                        entry.get("state").cloned().unwrap_or(Value::Null),
                    );
                    put(
                        "value_reason",
                        entry.get("reason").cloned().unwrap_or(Value::Null),
                    );
                    put(
                        "raw_value_json",
                        entry.get("raw").cloned().unwrap_or(Value::Null),
                    );
                    put("url_raw", url.get("raw").cloned().unwrap_or(Value::Null));
                    put(
                        "url_resolved",
                        url.get("resolved").cloned().unwrap_or(Value::Null),
                    );
                    put("url_key", key.clone());
                    put(
                        "url_state",
                        url.get("state")
                            .cloned()
                            .unwrap_or_else(|| json!("unknown")),
                    );
                    put(
                        "url_reason",
                        url.get("reason").cloned().unwrap_or(Value::Null),
                    );
                    put("period_start", start_date.clone());
                    put("period_end", end_date.clone());
                    put("period_state", json!(period_state));
                    put("period_reason", period_reason.clone());
                    put(
                        "timezone",
                        header.get("timezone").cloned().unwrap_or(Value::Null),
                    );
                    put("timezone_state", json!(tz_state));
                    put(
                        "timezone_reason",
                        tz_reason.clone().map_or(Value::Null, Value::String),
                    );
                    put(
                        "attribution",
                        header.get("attribution").cloned().unwrap_or(Value::Null),
                    );
                    put("attribution_state", json!(attr_state));
                    put(
                        "attribution_reason",
                        attr_reason.clone().map_or(Value::Null, Value::String),
                    );
                    put(
                        "search_engine",
                        header.get("search_engine").cloned().unwrap_or(Value::Null),
                    );
                    put("search_engine_state", json!(engine_state));
                    put(
                        "search_engine_reason",
                        engine_reason.clone().map_or(Value::Null, Value::String),
                    );
                    put(
                        "search_type",
                        header.get("search_type").cloned().unwrap_or(Value::Null),
                    );
                    put("search_type_state", json!(type_state));
                    put(
                        "search_type_reason",
                        type_reason.clone().map_or(Value::Null, Value::String),
                    );
                    put(
                        "collection_state",
                        collection.get("state").cloned().unwrap_or(Value::Null),
                    );
                    put(
                        "collection_reason",
                        collection.get("reason").cloned().unwrap_or(Value::Null),
                    );
                    put(
                        "collection_sampled",
                        collection.get("sampled").cloned().unwrap_or(Value::Null),
                    );
                    put(
                        "collection_thresholded",
                        collection
                            .get("thresholded")
                            .cloned()
                            .unwrap_or(Value::Null),
                    );
                    put(
                        "collection_truncated",
                        collection.get("truncated").cloned().unwrap_or(Value::Null),
                    );
                    put("source_row_index", json!(index));
                    put("natural_key_sha256", json!(natural_hash));
                    put(
                        "ambiguous_source_row",
                        json!(py::truthy(row.get("ambiguous"))),
                    );
                    put("population_state", json!(obs.population));
                    put("matched_page_count", json!(obs.matched_page_count));
                    put(
                        "matched_page_urls_json",
                        obs.matched_url_provenance
                            .clone()
                            .unwrap_or_else(|| json!(obs.matched_urls)),
                    );
                    put("dimensions_state_json", Value::Object(dims_state.clone()));
                    put("source_row_json", row.clone());
                    put("source_metadata_json", header.clone());
                }
                let mut result = Value::Object(result_map);
                if let Some(result_map) = result.as_object_mut() {
                    for (dimension, field_name) in dimensions {
                        let value = dims_row.get(dimension).cloned().unwrap_or(Value::Null);
                        result_map.insert(
                            field_name.as_str().unwrap_or_default().to_owned(),
                            if value.is_null() {
                                Value::Null
                            } else {
                                json!(py::str_value(&value))
                            },
                        );
                    }
                }
                rows.push(Ok(result));
            }
            rows.into_iter()
        })
}

/// One projected `link_occurrences` row from a retained scan link record.
pub fn link_row(run: &RunInput, record: &Value) -> Result<Value, BIExportError> {
    let source = record.get("source_url").and_then(Value::as_str);
    let destination_url = record.get("destination_url").and_then(Value::as_str);
    if !source.is_some_and(|s| !s.is_empty()) || !destination_url.is_some_and(|s| !s.is_empty()) {
        return Err(err(
            "retained link occurrence lacks source or destination URL",
        ));
    }
    let (source, destination_url) = (
        source.unwrap_or_default(),
        destination_url.unwrap_or_default(),
    );
    let rel_value = record.get("rel_json").cloned().unwrap_or(Value::Null);
    let rel = if let Value::String(text) = &rel_value {
        serde_json::from_str::<Value>(text)
            .map_err(|_| err("retained link occurrence has invalid rel_json"))?
    } else {
        rel_value
    };
    if !rel.is_null() && !rel.is_array() {
        return Err(err("retained link rel evidence must be a JSON list"));
    }
    let link_id = record.get("link_id");
    let ordinal = record.get("ordinal");
    if !link_id.is_some_and(py::is_int) || !ordinal.is_some_and(py::is_int) {
        return Err(err("retained link occurrence lacks its numeric identity"));
    }
    let (source_key, source_key_state, source_key_reason) = page_key(Some(source));
    let (destination_key, destination_key_state, destination_key_reason) =
        page_key(Some(destination_url));
    let evidence_representation = record
        .get("evidence_representation")
        .and_then(Value::as_str);
    let Some(evidence_representation) = evidence_representation else {
        return Err(err("retained link occurrence has no representation"));
    };
    let link_id_i = py::to_i64(link_id.unwrap()).unwrap_or(0);
    let ordinal_i = py::to_i64(ordinal.unwrap()).unwrap_or(0);
    let occurrence_id = digest_parts(&[&run.run_id, &link_id_i.to_string()])[..32].to_owned();
    let payload: Map<String, Value> = py::map(record)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|(k, v)| (k, json_value(&v)))
        .collect();
    let anchor = record.get("anchor").cloned().unwrap_or(Value::Null);
    let nofollow = record.get("nofollow").cloned().unwrap_or(Value::Null);
    let target = record.get("target").cloned().unwrap_or(Value::Null);
    let position = record.get("position").cloned().unwrap_or(Value::Null);
    let source_document_id = record
        .get("source_document_id")
        .cloned()
        .unwrap_or(Value::Null);
    let legacy = evidence_representation == "legacy_unknown";
    Ok(json!({
        "run_id": run.run_id,
        "link_occurrence_id": occurrence_id,
        "source_url": source,
        "source_url_key": source_key,
        "source_url_key_state": source_key_state,
        "source_url_key_reason": source_key_reason,
        "destination_url": destination_url,
        "destination_url_key": destination_key,
        "destination_url_key_state": destination_key_state,
        "destination_url_key_reason": destination_key_reason,
        "link_ordinal": ordinal_i,
        "evidence_representation": evidence_representation,
        "link_type": if legacy { Value::Null } else { json!("hyperlink") },
        "link_type_state": if legacy { "unknown" } else { "measured" },
        "link_type_reason": if legacy { json!("legacy source does not retain link element type") } else { Value::Null },
        "anchor": anchor,
        "anchor_state": if anchor.is_null() { "unknown" } else { "measured" },
        "anchor_reason": if anchor.is_null() { json!("anchor value was not retained") } else { Value::Null },
        "nofollow": if nofollow.is_null() { Value::Null } else { json!(py::truthy(Some(&nofollow))) },
        "nofollow_state": if nofollow.is_null() { "unknown" } else { "measured" },
        "nofollow_reason": if nofollow.is_null() { json!("nofollow state was not retained") } else { Value::Null },
        "rel_json": rel,
        "rel_state": if rel.is_null() { "unknown" } else { "measured" },
        "rel_reason": if rel.is_null() { json!("link relation values were not retained") } else { Value::Null },
        "target": target,
        "target_state": if target.is_null() { "unknown" } else { "measured" },
        "target_reason": if target.is_null() { json!("target attribute was not retained") } else { Value::Null },
        "placement": if py::truthy(Some(&position)) { position.clone() } else { Value::Null },
        "placement_state": if py::truthy(Some(&position)) { "measured" } else { "unknown" },
        "placement_reason": if py::truthy(Some(&position)) { Value::Null } else { json!("link placement was not captured") },
        "dom_context_json": Value::Null,
        "dom_context_state": "not_captured",
        "dom_context_reason": "scan.v1 does not retain DOM ancestor context for link occurrences",
        "source_document_id": source_document_id,
        "source_document_state": if source_document_id.is_null() { "not_captured" } else { "measured" },
        "source_document_reason": if source_document_id.is_null() { json!("source document identity was not retained") } else { Value::Null },
        "raw_href": record.get("raw_href").cloned().unwrap_or(Value::Null),
        "source_link_id": link_id_i,
        "source_link_json": Value::Object(payload),
    }))
}

/// Scan-run input.
fn scan_run(
    path: &Path,
    out_directory: &Path,
    providers: &mut [ProviderInput],
    limits: &ExportLimits,
) -> Result<Value, BIExportError> {
    let con = spiderhead_store::scan_artifact::open_scan(path, false, None)
        .map_err(|e| err(format!("scan input failed validation: {e}")))?;
    let mut run = scan_source(
        &path.to_string_lossy(),
        &con,
        limits.max_scan_bytes,
        out_directory.parent().unwrap_or_else(|| Path::new(".")),
    )?;
    crate::bi::writer::write_package(
        &mut run,
        true,
        out_directory,
        providers,
        limits.max_rows_per_file,
        limits.max_bytes_per_file,
        limits.max_output_bytes,
        limits.search_metric.as_deref(),
    )
}

/// Audit-run input.
fn audit_run(
    value: &Value,
    out_directory: &Path,
    providers: &mut [ProviderInput],
    limits: &ExportLimits,
) -> Result<Value, BIExportError> {
    let (document, raw, name) = read_audit_input(value)?;
    let mut run = audit_source(&document, &raw, name)?;
    crate::bi::writer::write_package(
        &mut run,
        false,
        out_directory,
        providers,
        limits.max_rows_per_file,
        limits.max_bytes_per_file,
        limits.max_output_bytes,
        limits.search_metric.as_deref(),
    )
}

/// Limits for the BI export, with the default values below.
#[derive(Debug, Clone)]
pub struct ExportLimits {
    /// `max_rows_per_file` (default 25_000).
    pub max_rows_per_file: usize,
    /// `max_bytes_per_file` (default 8 MiB).
    pub max_bytes_per_file: u64,
    /// `max_output_bytes` (default 4 GiB).
    pub max_output_bytes: u64,
    /// `max_scan_bytes` (default 8 GiB).
    pub max_scan_bytes: u64,
    /// Search metric: `clicks`, `impressions`, or none.
    pub search_metric: Option<String>,
}

impl Default for ExportLimits {
    fn default() -> Self {
        ExportLimits {
            max_rows_per_file: DEFAULT_ROWS_PER_PARTITION,
            max_bytes_per_file: DEFAULT_BYTES_PER_PARTITION,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            max_scan_bytes: DEFAULT_MAX_SCAN_BYTES,
            search_metric: None,
        }
    }
}

/// Write a complete local BI package from one saved run and
/// optional joins.
pub fn export_bi(
    scan: Option<&Path>,
    audit: Option<&Value>,
    provider_joins: &[String],
    out_dir: Option<&Path>,
    limits: ExportLimits,
) -> Result<Value, BIExportError> {
    if scan.is_none() == audit.is_none() {
        return Err(err("supply exactly one of scan or audit"));
    }
    let out_dir_str = out_dir
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    if out_dir.is_none() || out_dir_str.trim().is_empty() {
        return Err(err(
            "out_dir is required; BI packages are written only to an explicit path",
        ));
    }
    if limits.max_rows_per_file == 0 || limits.max_rows_per_file > MAX_ROWS_PER_PARTITION {
        return Err(err(format!(
            "max_rows_per_file must be 1..{MAX_ROWS_PER_PARTITION}"
        )));
    }
    if !(1024..=MAX_BYTES_PER_PARTITION).contains(&limits.max_bytes_per_file) {
        return Err(err(format!(
            "max_bytes_per_file must be 1024..{MAX_BYTES_PER_PARTITION}"
        )));
    }
    if limits.max_output_bytes < limits.max_bytes_per_file
        || limits.max_output_bytes > MAX_OUTPUT_BYTES
    {
        return Err(err(format!(
            "max_output_bytes must be at least max_bytes_per_file and at most {MAX_OUTPUT_BYTES}"
        )));
    }
    if limits.max_scan_bytes == 0 || limits.max_scan_bytes > MAX_SCAN_BYTES {
        return Err(err(format!("max_scan_bytes must be 1..{MAX_SCAN_BYTES}")));
    }
    if let Some(metric) = &limits.search_metric {
        if metric != "clicks" && metric != "impressions" {
            return Err(err(
                "search_metric must be 'clicks', 'impressions', or null",
            ));
        }
    }
    if provider_joins.len() > MAX_PROVIDER_SOURCES {
        return Err(err(format!(
            "provider_joins is limited to {MAX_PROVIDER_SOURCES} sources"
        )));
    }
    let mut providers = Vec::with_capacity(provider_joins.len());
    let mut provider_total_bytes = 0u64;
    let mut provider_hashes = BTreeSet::new();
    for path in provider_joins {
        let provider = provider_file(path)?;
        if !provider_hashes.insert(provider.sha256.clone()) {
            return Err(err(
                "the same provider join artifact was supplied more than once",
            ));
        }
        provider_total_bytes += provider.byte_count;
        if provider_total_bytes > MAX_PROVIDER_JOIN_TOTAL_BYTES {
            return Err(err(format!(
                "provider join inputs exceed the {MAX_PROVIDER_JOIN_TOTAL_BYTES}-byte total bound"
            )));
        }
        providers.push(provider);
    }
    let destination = if out_dir_str.starts_with('/') {
        PathBuf::from(&out_dir_str)
    } else {
        std::env::current_dir()
            .map_err(|e| err(format!("cannot resolve out_dir: {e}")))?
            .join(&out_dir_str)
    };
    if destination.is_symlink() || destination.exists() || destination.is_symlink() {
        return Err(err(format!(
            "output directory already exists or is a symlink: {}",
            destination
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default()
        )));
    }
    let parent = destination
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    if !parent.is_dir() {
        return Err(err("out_dir parent directory must exist"));
    }
    if parent.is_symlink() {
        return Err(err("out_dir parent directory must not be a symlink"));
    }
    if let Some(scan) = scan {
        scan_run(scan, &destination, &mut providers, &limits)
    } else {
        audit_run(
            audit.unwrap_or(&Value::Null),
            &destination,
            &mut providers,
            &limits,
        )
    }
}

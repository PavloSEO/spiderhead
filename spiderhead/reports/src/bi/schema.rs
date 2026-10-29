//! Typed BI contracts: dataset field schemas, hard resource bounds,
//! and the shared serialization helpers every layer reads.

use std::borrow::Cow;

use serde_json::{json, Map, Value};
use thiserror::Error;

use spiderhead_core::tabular::neutralize_formula;

use crate::py;

/// Manifest format marker.
pub const MANIFEST_FORMAT: &str = "spiderhead.bi-manifest.v1";
/// BI schema version marker.
pub const BI_SCHEMA_VERSION: &str = "spiderhead.bi.v1";
/// Evidence join contract marker.
pub const JOIN_FORMAT: &str = "spiderhead.evidence-join.v1";
/// Normalized provider evidence marker.
pub const NORMALIZED_FORMAT: &str = "spiderhead.normalized-evidence.v1";

pub const MAX_AUDIT_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_SCAN_BYTES: u64 = 32 * 1024 * 1024 * 1024;
pub const DEFAULT_MAX_SCAN_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub const MAX_PROVIDER_JOIN_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_PROVIDER_JOIN_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_PROVIDER_SOURCES: usize = 8;
pub const MAX_PROVIDER_ROWS_PER_SOURCE: usize = 100_000;
pub const MAX_PAGES: usize = 1_000_000;
pub const MAX_FINDINGS: usize = 1_000_000;
pub const MAX_LINK_OCCURRENCES: usize = 5_000_000;
pub const MAX_ROWS_PER_PARTITION: usize = 250_000;
pub const MAX_BYTES_PER_PARTITION: u64 = 64 * 1024 * 1024;
pub const MAX_OUTPUT_BYTES: u64 = 16 * 1024 * 1024 * 1024;
pub const MAX_OUTPUT_ROWS: usize = 10_000_000;
pub const MAX_OUTPUT_PARTITIONS: usize = 10_000;
pub const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_CELL_BYTES: u64 = 8 * 1024 * 1024;
pub const DEFAULT_ROWS_PER_PARTITION: usize = 25_000;
pub const DEFAULT_BYTES_PER_PARTITION: u64 = 8 * 1024 * 1024;
pub const DEFAULT_MAX_OUTPUT_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const MIN_FREE_DISK_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_PROJECTION_INDEX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Partition bound read at call time, so hosts that patch the facade constant
/// keep one source of truth.
pub fn max_output_partitions() -> usize {
    MAX_OUTPUT_PARTITIONS
}

/// A source artifact or bounded BI export violates its declared contract.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct BIExportError(pub String);

impl BIExportError {
    /// Wrap a message.
    pub fn new(message: impl Into<String>) -> Self {
        BIExportError(message.into())
    }
}

/// One typed dataset field.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    /// Field name in the emitted CSV header.
    pub name: Cow<'static, str>,
    /// BI field type (string/integer/number/boolean/date/json).
    pub type_: &'static str,
    /// Whether a null cell is legal.
    pub nullable: bool,
    /// Human description for the manifest.
    pub description: Cow<'static, str>,
}

/// Static field constructor: name, type, nullability and an empty description.
pub const fn f(
    name: &'static str,
    type_: &'static str,
    nullable: bool,
    description: &'static str,
) -> Field {
    Field {
        name: Cow::Borrowed(name),
        type_,
        nullable,
        description: Cow::Borrowed(description),
    }
}

/// Dynamic string-typed field (provider dimension columns and companions).
pub fn dyn_field(
    name: impl Into<String>,
    type_: &'static str,
    nullable: bool,
    description: impl Into<String>,
) -> Field {
    Field {
        name: Cow::Owned(name.into()),
        type_,
        nullable,
        description: Cow::Owned(description.into()),
    }
}

pub const PAGE_FIELDS: &[Field] = &[
    f(
        "run_id",
        "string",
        false,
        "Stable identity of the source scan or audit.",
    ),
    f(
        "url_observation_id",
        "string",
        false,
        "Stable key for this run and raw URL.",
    ),
    f(
        "source_ordinal",
        "integer",
        false,
        "Source page order, retained for traceability.",
    ),
    f(
        "url",
        "string",
        false,
        "URL exactly as retained by the source artifact.",
    ),
    f(
        "url_key",
        "string",
        true,
        "Strict external_join.v1 key, when derivable.",
    ),
    f(
        "url_key_state",
        "string",
        false,
        "keyed, unkeyable, or not supplied.",
    ),
    f(
        "url_key_reason",
        "string",
        true,
        "Reason a URL key is unavailable.",
    ),
    f("status_code", "integer", true, ""),
    f("status_code_state", "string", false, ""),
    f("status_code_reason", "string", true, ""),
    f("content_type", "string", true, ""),
    f("content_type_state", "string", false, ""),
    f("content_type_reason", "string", true, ""),
    f("indexability", "string", true, ""),
    f("indexability_state", "string", false, ""),
    f("indexability_reason", "string", true, ""),
    f("indexability_status", "string", true, ""),
    f("indexability_status_state", "string", false, ""),
    f("indexability_status_reason", "string", true, ""),
    f("crawl_depth", "integer", true, ""),
    f("crawl_depth_state", "string", false, ""),
    f("crawl_depth_reason", "string", true, ""),
    f("response_time_seconds", "number", true, ""),
    f("response_time_state", "string", false, ""),
    f("response_time_reason", "string", true, ""),
    f("size_bytes", "integer", true, ""),
    f("size_state", "string", false, ""),
    f("size_reason", "string", true, ""),
    f("title", "string", true, ""),
    f("title_state", "string", false, ""),
    f("title_reason", "string", true, ""),
    f("meta_description", "string", true, ""),
    f("meta_description_state", "string", false, ""),
    f("meta_description_reason", "string", true, ""),
    f("h1", "string", true, ""),
    f("h1_state", "string", false, ""),
    f("h1_reason", "string", true, ""),
    f("canonical", "string", true, ""),
    f("canonical_state", "string", false, ""),
    f("canonical_reason", "string", true, ""),
    f("word_count", "integer", true, ""),
    f("word_count_state", "string", false, ""),
    f("word_count_reason", "string", true, ""),
    f("text_ratio", "number", true, ""),
    f("text_ratio_state", "string", false, ""),
    f("text_ratio_reason", "string", true, ""),
    f("inlinks", "integer", true, ""),
    f("inlinks_state", "string", false, ""),
    f("inlinks_reason", "string", true, ""),
    f("unique_inlinks", "integer", true, ""),
    f("unique_inlinks_state", "string", false, ""),
    f("unique_inlinks_reason", "string", true, ""),
    f("internal_outlinks", "integer", true, ""),
    f("internal_outlinks_state", "string", false, ""),
    f("internal_outlinks_reason", "string", true, ""),
    f("outlinks_total", "integer", true, ""),
    f("outlinks_total_state", "string", false, ""),
    f("outlinks_total_reason", "string", true, ""),
    f("external_outlinks", "integer", true, ""),
    f("external_outlinks_state", "string", false, ""),
    f("external_outlinks_reason", "string", true, ""),
    f("link_counts_state", "string", false, ""),
    f("link_counts_reason", "string", true, ""),
    f("body_evidence_state", "string", false, ""),
    f("body_evidence_reason", "string", true, ""),
    f("representation", "string", true, ""),
    f("representation_state", "string", false, ""),
    f("representation_reason", "string", true, ""),
    f(
        "source_fields_json",
        "json",
        false,
        "Unprojected source fields, preserved as JSON.",
    ),
];

pub const FINDING_FIELDS: &[Field] = &[
    f("run_id", "string", false, ""),
    f("finding_id", "string", false, ""),
    f("source_finding_id", "string", true, ""),
    f("source_ordinal", "integer", false, ""),
    f("finding_kind", "string", false, ""),
    f("check_id", "string", true, ""),
    f("check_state", "string", false, ""),
    f("check_reason", "string", true, ""),
    f("severity", "string", true, ""),
    f("severity_state", "string", false, ""),
    f("severity_reason", "string", true, ""),
    f("url", "string", true, ""),
    f("url_state", "string", false, ""),
    f("url_reason", "string", true, ""),
    f("url_key", "string", true, ""),
    f("url_key_state", "string", false, ""),
    f("url_key_reason", "string", true, ""),
    f("message", "string", true, ""),
    f("message_state", "string", false, ""),
    f("message_reason", "string", true, ""),
    f("source", "string", true, ""),
    f("source_state", "string", false, ""),
    f("source_reason", "string", true, ""),
    f("status_code", "integer", true, ""),
    f("status_code_state", "string", false, ""),
    f("status_code_reason", "string", true, ""),
    f("occurrences_count", "integer", true, ""),
    f("occurrences_count_state", "string", false, ""),
    f("occurrences_count_reason", "string", true, ""),
    f("group_state", "string", false, ""),
    f("fix_hint", "string", true, ""),
    f("fix_hint_state", "string", false, ""),
    f("fix_hint_reason", "string", true, ""),
    f("group_id", "string", true, ""),
    f("group_value", "string", true, ""),
    f("group_url_count", "integer", true, ""),
    f("group_urls_json", "json", true, ""),
    f("locations_json", "json", false, ""),
    f("details_json", "json", false, ""),
    f("evidence_json", "json", false, ""),
    f("source_finding_json", "json", false, ""),
];

pub const METRIC_BASE_FIELDS: &[Field] = &[
    f("run_id", "string", false, ""),
    f("metric_observation_id", "string", false, ""),
    f("provider_source_id", "string", false, ""),
    f("provider", "string", true, ""),
    f("operation", "string", true, ""),
    f("reporting_identity", "string", true, ""),
    f("privacy", "string", true, ""),
    f("metric_name", "string", false, ""),
    f("metric_unit", "string", true, ""),
    f("metric_unit_state", "string", false, ""),
    f("metric_unit_reason", "string", true, ""),
    f("value_number", "number", true, ""),
    f("value_state", "string", false, ""),
    f("value_reason", "string", true, ""),
    f("raw_value_json", "json", true, ""),
    f("url_raw", "string", true, ""),
    f("url_resolved", "string", true, ""),
    f("url_key", "string", true, ""),
    f("url_state", "string", false, ""),
    f("url_reason", "string", true, ""),
    f("period_start", "date", true, ""),
    f("period_end", "date", true, ""),
    f("period_state", "string", false, ""),
    f("period_reason", "string", true, ""),
    f("timezone", "string", true, ""),
    f("timezone_state", "string", false, ""),
    f("timezone_reason", "string", true, ""),
    f("attribution", "string", true, ""),
    f("attribution_state", "string", false, ""),
    f("attribution_reason", "string", true, ""),
    f("search_engine", "string", true, ""),
    f("search_engine_state", "string", false, ""),
    f("search_engine_reason", "string", true, ""),
    f("search_type", "string", true, ""),
    f("search_type_state", "string", false, ""),
    f("search_type_reason", "string", true, ""),
    f("collection_state", "string", false, ""),
    f("collection_reason", "string", true, ""),
    f("collection_sampled", "boolean", true, ""),
    f("collection_thresholded", "boolean", true, ""),
    f("collection_truncated", "boolean", true, ""),
    f("source_row_index", "integer", false, ""),
    f("natural_key_sha256", "string", false, ""),
    f("ambiguous_source_row", "boolean", false, ""),
    f("population_state", "string", false, ""),
    f("matched_page_count", "integer", false, ""),
    f("matched_page_urls_json", "json", false, ""),
    f("dimensions_state_json", "json", false, ""),
    f("source_row_json", "json", false, ""),
    f("source_metadata_json", "json", false, ""),
];

pub const LINK_FIELDS: &[Field] = &[
    f("run_id", "string", false, ""),
    f("link_occurrence_id", "string", false, ""),
    f("source_url", "string", false, ""),
    f("source_url_key", "string", true, ""),
    f("source_url_key_state", "string", false, ""),
    f("source_url_key_reason", "string", true, ""),
    f("destination_url", "string", false, ""),
    f("destination_url_key", "string", true, ""),
    f("destination_url_key_state", "string", false, ""),
    f("destination_url_key_reason", "string", true, ""),
    f("link_ordinal", "integer", false, ""),
    f("evidence_representation", "string", false, ""),
    f("link_type", "string", true, ""),
    f("link_type_state", "string", false, ""),
    f("link_type_reason", "string", true, ""),
    f("anchor", "string", true, ""),
    f("anchor_state", "string", false, ""),
    f("anchor_reason", "string", true, ""),
    f("nofollow", "boolean", true, ""),
    f("nofollow_state", "string", false, ""),
    f("nofollow_reason", "string", true, ""),
    f("rel_json", "json", true, ""),
    f("rel_state", "string", false, ""),
    f("rel_reason", "string", true, ""),
    f("target", "string", true, ""),
    f("target_state", "string", false, ""),
    f("target_reason", "string", true, ""),
    f("placement", "string", true, ""),
    f("placement_state", "string", false, ""),
    f("placement_reason", "string", true, ""),
    f("dom_context_json", "json", true, ""),
    f("dom_context_state", "string", false, ""),
    f("dom_context_reason", "string", true, ""),
    f("source_document_id", "integer", true, ""),
    f("source_document_state", "string", false, ""),
    f("source_document_reason", "string", true, ""),
    f("raw_href", "string", true, ""),
    f("source_link_id", "integer", false, ""),
    f("source_link_json", "json", false, ""),
];

pub const COVERAGE_FIELDS: &[Field] = &[
    f("run_id", "string", false, ""),
    f("dataset", "string", false, ""),
    f("evidence_source_id", "string", false, ""),
    f("population", "string", false, ""),
    f("state", "string", false, ""),
    f("reason", "string", true, ""),
    f("source_rows", "integer", true, ""),
    f("exported_rows", "integer", true, ""),
    f("unavailable_rows", "integer", true, ""),
    f("details_json", "json", false, ""),
];

pub const COHORT_FIELDS: &[Field] = &[
    f("run_id", "string", false, ""),
    f("cohort_observation_id", "string", false, ""),
    f("cohort_id", "string", false, ""),
    f("definition_version", "string", false, ""),
    f("definition", "string", false, ""),
    f("url", "string", true, ""),
    f("url_key", "string", true, ""),
    f("url_key_state", "string", false, ""),
    f("url_key_reason", "string", true, ""),
    f("membership", "string", false, ""),
    f("state", "string", false, ""),
    f("reason", "string", true, ""),
    f("value_label", "string", true, ""),
    f("value_number", "number", true, ""),
    f("threshold", "number", true, ""),
    f("numerator", "integer", true, ""),
    f("denominator", "integer", true, ""),
    f("extraction_coverage_state", "string", false, ""),
    f("extraction_coverage_reason", "string", true, ""),
    f("search_metric", "string", true, ""),
    f("search_value", "number", true, ""),
    f("search_value_state", "string", false, ""),
    f("sessions_value", "number", true, ""),
    f("sessions_value_state", "string", false, ""),
    f("period_start", "date", true, ""),
    f("period_end", "date", true, ""),
    f("timezone", "string", true, ""),
    f("source_metric_observations_json", "json", false, ""),
];

/// `(fields, grain, primary_key)` per dataset.
pub fn dataset_specs() -> &'static [(
    &'static str,
    &'static [Field],
    &'static str,
    &'static [&'static str],
)] {
    &[
        ("pages", PAGE_FIELDS, "one URL observation per run and raw URL", &["run_id", "url_observation_id"]),
        ("findings", FINDING_FIELDS, "one finding/check observation per run and source finding", &["run_id", "finding_id"]),
        ("metrics", METRIC_BASE_FIELDS, "one source metric observation per provider row and metric at its declared dimension/period grain", &["run_id", "provider_source_id", "metric_observation_id"]),
        ("link_occurrences", LINK_FIELDS, "one row per retained source-to-target link occurrence", &["run_id", "link_occurrence_id"]),
        ("coverage", COVERAGE_FIELDS, "one row per dataset, provider population, or check coverage state", &["run_id", "dataset", "evidence_source_id", "population"]),
        ("cohorts", COHORT_FIELDS, "one evidence-qualified URL cohort observation per run, URL, and cohort definition", &["run_id", "cohort_observation_id"]),
    ]
}

/// Dataset spec lookup by name: `(fields, grain, key)`.
pub fn dataset_spec(
    name: &str,
) -> Option<(&'static [Field], &'static str, &'static [&'static str])> {
    dataset_specs()
        .iter()
        .find(|(n, ..)| *n == name)
        .map(|(_, fields, grain, key)| (*fields, *grain, *key))
}

/// JSON text with sorted keys; non-finite numbers are rejected.
pub fn canonical_json(value: &Value) -> Result<String, BIExportError> {
    if has_non_finite(value) {
        return Err(BIExportError::new(
            "value is not safe JSON evidence: Out of range float values are not JSON compliant",
        ));
    }
    spiderhead_store::audit_v2::canonical(value)
        .map_err(|e| BIExportError::new(format!("value is not safe JSON evidence: {e}")))
}

fn has_non_finite(value: &Value) -> bool {
    match value {
        Value::Number(n) => n.as_f64().is_some_and(|f| !f.is_finite()),
        Value::Array(items) => items.iter().any(has_non_finite),
        Value::Object(map) => map.values().any(has_non_finite),
        _ => false,
    }
}

/// Digest of JSON data.
pub fn digest(data: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(data))
}

/// Digest of several values, joined as one input.
pub fn digest_parts(values: &[&str]) -> String {
    digest(values.join("\0").as_bytes())
}

/// JSON-safe value: pass JSON types through, dates as ISO strings, and reject NaN and infinity.
pub fn json_value(value: &Value) -> Value {
    match value {
        Value::Number(n) => match n.as_f64() {
            Some(f) if !f.is_finite() => Value::Null,
            _ => value.clone(),
        },
        _ => value.clone(),
    }
}

const SENSITIVE_SETTING_TOKENS: [&str; 7] = [
    "authorization",
    "cookie",
    "password",
    "secret",
    "token",
    "api_key",
    "private_key",
];

/// Replace values of sensitive-looking keys with a fixed marker.
pub fn redact_setting(value: &Value, key: &str) -> Value {
    let lowered = key.to_lowercase();
    if SENSITIVE_SETTING_TOKENS.iter().any(|t| lowered.contains(t)) {
        return Value::String("REDACTED".to_owned());
    }
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(name, child)| (name.clone(), redact_setting(child, name)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|child| redact_setting(child, key))
                .collect(),
        ),
        _ => json_value(value),
    }
}

/// Cell text: null becomes empty, booleans become `true` or `false`, other strings are neutralized.
pub fn cell_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => {
            if *b {
                "true".to_owned()
            } else {
                "false".to_owned()
            }
        }
        Value::String(s) => neutralize_formula(s),
        other => neutralize_formula(&py::str_value(other)),
    }
}

/// Format one cell as `(text, prefixed)`.
pub fn format_cell(
    value: &Value,
    field_type: &str,
    field_name: &str,
) -> Result<(String, bool), BIExportError> {
    if value.is_null() {
        return Ok((String::new(), false));
    }
    match field_type {
        "boolean" => match value {
            Value::Bool(b) => Ok((
                if *b {
                    "true".to_owned()
                } else {
                    "false".to_owned()
                },
                false,
            )),
            _ => Err(BIExportError::new(format!(
                "{field_name} must be boolean or null"
            ))),
        },
        "integer" => match value {
            Value::Number(n) if n.is_i64() || n.is_u64() => Ok((py::str_value(value), false)),
            _ => Err(BIExportError::new(format!(
                "{field_name} must be integer or null"
            ))),
        },
        "number" => match value {
            Value::Number(n) => {
                let f = n.as_f64().unwrap_or(f64::NAN);
                if !f.is_finite() {
                    return Err(BIExportError::new(format!("{field_name} must be finite")));
                }
                Ok((py::repr(value), false))
            }
            _ => Err(BIExportError::new(format!(
                "{field_name} must be numeric or null"
            ))),
        },
        "date" => match value {
            Value::String(s) if crate::join::IsoDate::parse(s).is_some() => Ok((s.clone(), false)),
            _ => Err(BIExportError::new(format!(
                "{field_name} must be an ISO date or null"
            ))),
        },
        "json" => Ok((canonical_json(value)?, false)),
        "string" => match value {
            Value::String(s) => {
                let safe = cell_text(value);
                let prefixed = safe != *s;
                Ok((safe, prefixed))
            }
            _ => Err(BIExportError::new(format!(
                "{field_name} must be a string or null"
            ))),
        },
        other => Err(BIExportError::new(format!(
            "unsupported BI field type {other:?}"
        ))),
    }
}

/// Safe dimension column name for a provider dimension.
pub fn safe_dimension_fields<'a>(names: impl IntoIterator<Item = &'a str>) -> Map<String, Value> {
    use unicode_casefold::UnicodeCaseFold;
    let unique: std::collections::BTreeSet<&str> = names.into_iter().collect();
    let slug_re = regex::Regex::new(r"[^a-z0-9_]+").unwrap();
    let mut used: std::collections::HashMap<String, &str> = std::collections::HashMap::new();
    let mut result = Map::new();
    for name in unique {
        let folded: String = name.case_fold().collect();
        let mut slug = slug_re.replace_all(&folded, "_").to_string();
        slug = slug.trim_matches('_').to_owned();
        if slug.is_empty() {
            slug = "value".to_owned();
        }
        let mut field = format!("dimension_{slug}");
        if used.get(&field).is_some_and(|existing| *existing != name) {
            field = format!("{field}_{}", &digest(name.as_bytes())[..8]);
        }
        used.insert(field.clone(), name);
        result.insert(name.to_owned(), Value::String(field));
    }
    result
}

/// Base metric fields plus one nullable string column per declared provider dimension,
/// in the mapping order.
pub fn metric_fields(dimension_fields: &Map<String, Value>) -> Vec<Field> {
    let mut fields: Vec<Field> = METRIC_BASE_FIELDS.to_vec();
    for (dimension_name, field_name) in dimension_fields {
        fields.push(dyn_field(
            field_name.as_str().unwrap_or_default().to_owned(),
            "string",
            true,
            format!("Provider dimension {dimension_name}."),
        ));
    }
    fields
}

/// Coverage row.
#[allow(clippy::too_many_arguments)]
pub fn coverage_row(
    run_id: &str,
    dataset: &str,
    source_id: &str,
    population: &str,
    state: &str,
    reason: Option<&Value>,
    source_rows: Option<&Value>,
    exported_rows: Option<&Value>,
    unavailable_rows: Option<&Value>,
    details: Option<&Value>,
) -> Value {
    json!({
        "run_id": run_id,
        "dataset": dataset,
        "evidence_source_id": source_id,
        "population": population,
        "state": state,
        "reason": reason.cloned().unwrap_or(Value::Null),
        "source_rows": source_rows.cloned().unwrap_or(Value::Null),
        "exported_rows": exported_rows.cloned().unwrap_or(Value::Null),
        "unavailable_rows": unavailable_rows.cloned().unwrap_or(Value::Null),
        "details_json": details.cloned().unwrap_or_else(|| json!({})),
    })
}

/// Schema fields for a dataset.
pub fn schema_fields(fields: &[Field]) -> Vec<Value> {
    fields
        .iter()
        .map(|field| {
            json!({
                "name": field.name,
                "type": field.type_,
                "nullable": field.nullable,
                "description": field.description,
            })
        })
        .collect()
}

/// Manifest entry for one dataset.
#[allow(clippy::too_many_arguments)]
pub fn manifest_dataset(
    name: &str,
    fields: &[Field],
    result: &Value,
    state: &str,
    reason: Option<&Value>,
    source_population: &str,
    row_count: usize,
    coverage: Option<&Value>,
) -> Value {
    let (_, grain, key) = dataset_spec(name).unwrap_or((&[], "", &[]));
    json!({
        "schema_version": format!("spiderhead.bi.{name}.v1"),
        "grain": grain,
        "primary_key": key,
        "fields": schema_fields(fields),
        "state": state,
        "reason": reason.cloned().unwrap_or(Value::Null),
        "source_population": source_population,
        "row_count": row_count,
        "partition_count": result.get("partition_count").cloned().unwrap_or(Value::Null),
        "bytes": result.get("bytes").cloned().unwrap_or(Value::Null),
        "formula_safe_cells_prefixed": result.get("formula_safe_cells_prefixed").cloned().unwrap_or(Value::Null),
        "partitions": result.get("partitions").cloned().unwrap_or(Value::Null),
        "coverage": coverage.cloned().unwrap_or_else(|| json!({})),
    })
}

/// One source run (saved audit or scan) behind the projection.
pub struct RunInput {
    /// Stable run identifier.
    pub run_id: String,
    /// `sf-audit`, `site-audit`, or `scan`.
    pub source_kind: String,
    /// Source schema marker retained for the manifest.
    pub source_schema: String,
    /// Source file name when the run came from a path.
    pub source_name: Option<String>,
    /// Source content hash.
    pub source_sha256: String,
    /// Source byte length.
    pub source_bytes: u64,
    /// Run-level metadata for the `runs` dataset.
    pub run_metadata: Map<String, Value>,
    /// Re-iterable page rows.
    pub pages_factory: Box<dyn FnMut() -> Box<dyn Iterator<Item = Value> + Send> + Send>,
    /// Source page count.
    pub page_count: usize,
    /// Re-iterable finding rows.
    pub findings_factory: Box<dyn FnMut() -> Box<dyn Iterator<Item = Value> + Send> + Send>,
    /// Source finding count.
    pub finding_count: usize,
    /// Finding groups, list or stream.
    pub groups: Groups,
    /// Re-iterable link rows.
    pub links_factory: Box<dyn FnMut() -> Box<dyn Iterator<Item = Value> + Send> + Send>,
    /// `complete`/`partial`/`unavailable` link evidence state.
    pub links_source_state: String,
    /// Why link evidence is not `complete`.
    pub links_source_reason: Option<String>,
    /// Check/tool coverage rows collected during ingestion.
    pub coverage_rows: Vec<Value>,
    /// Link row count when the inventory is usable.
    pub link_count: Option<usize>,
    /// Saved audit content hash.
    pub audit_sha256: Option<String>,
}

/// Observation groups: an already-materialized list, or a re-iterable counted stream.
pub enum Groups {
    /// Already materialized rows.
    List(Vec<Value>),
    /// Re-iterable, counted rows (audit.v2 backed).
    Stream {
        /// Factory producing the ordered row iterator.
        factory: Box<dyn FnMut() -> Box<dyn Iterator<Item = Value> + Send> + Send>,
        /// Row count.
        count: usize,
    },
}

impl Groups {
    /// Stream count or list length, used for emptiness.
    pub fn len(&self) -> usize {
        match self {
            Groups::List(items) => items.len(),
            Groups::Stream { count, .. } => *count,
        }
    }

    /// Whether the stream/list holds no rows.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Iterate rows; a list clones its items so the source stays re-iterable.
    pub fn iter(&mut self) -> Box<dyn Iterator<Item = Value> + Send + '_> {
        match self {
            Groups::List(items) => Box::new(items.iter().cloned()),
            Groups::Stream { factory, .. } => factory(),
        }
    }
}

/// One typed provider evidence source.
pub struct ProviderInput {
    /// Content-hash identity of the source bytes.
    pub source_id: String,
    /// File name when supplied by path.
    pub name: Option<String>,
    /// File content hash.
    pub sha256: String,
    /// File byte length.
    pub byte_count: u64,
    /// Saved `evidence-join.v1` document when present.
    pub join: Option<Value>,
    /// Compatibility block beside a nested join document.
    pub compatibility: Option<Value>,
    /// `normalized-evidence.v1` document when present.
    pub normalized: Option<Value>,
    /// Unsupported-source marker (legacy provider-join).
    pub error_state: Option<String>,
    /// Why the source is unsupported.
    pub error_reason: Option<String>,
    /// Rows the legacy artifact claims to hold.
    pub reported_rows: Option<i64>,
}

/// Page URL key.
pub fn page_url(page: &Value) -> Option<String> {
    page.get("url")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Page key as `(key, state, reason)`.
pub fn page_key(url: Option<&str>) -> (Option<String>, &'static str, Option<&'static str>) {
    match crate::join::normalize_join_key(url) {
        Some(key) => (Some(key), "keyed", None),
        None => (
            None,
            "unkeyable",
            Some("URL is not a valid absolute HTTP(S) join key"),
        ),
    }
}

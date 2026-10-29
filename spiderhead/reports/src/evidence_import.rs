//! Offline evidence import: normalize parsed rows against a declared mapping manifest.
//!
//! Only the inline path is supported: callers supply parsed row objects and a manifest.
//! Envelope and result facts are absent on this path.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use serde_json::{json, Map, Value};
use thiserror::Error;

use crate::join::normalize_join_key;
use crate::py;

/// Import or manifest failure.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct EvidenceImportError(pub String);

fn err(msg: impl Into<String>) -> EvidenceImportError {
    EvidenceImportError(msg.into())
}

pub const MAPPING_FORMAT: &str = "spiderhead.evidence-mapping.v1";
pub const NORMALIZED_FORMAT: &str = "spiderhead.normalized-evidence.v1";
pub const MAX_ROWS: usize = 100_000;

const COLLECTION_STATES: [&str; 5] = ["complete", "partial", "failed", "skipped", "not_configured"];
const ROW_SHAPES: [&str; 3] = ["flat", "gsc", "ga4"];
const FILE_KINDS: [&str; 4] = ["csv", "xlsx", "json", "provider_evidence"];
const URL_KINDS: [&str; 2] = ["absolute", "relative_path"];
const DUPLICATE_POLICIES: [&str; 2] = ["mark", "reject"];

const SUPPRESSED_STRINGS: [&str; 2] = ["(other)", "(not set)"];
const NON_URL_STRINGS: [&str; 2] = ["(not set)", "(other)"];

fn provider_contract(provider: &str, key: &str) -> Option<&'static str> {
    match (provider, key) {
        ("gsc", "search_engine") => Some("google"),
        ("gsc", "search_type") => Some("web"),
        ("gsc", "timezone") => Some("America/Los_Angeles"),
        ("bing_webmaster", "search_engine") => Some("bing"),
        ("bing_webmaster", "search_type") => Some("web"),
        ("yandex_webmaster", "search_engine") => Some("yandex"),
        ("yandex_webmaster", "search_type") => Some("web"),
        _ => None,
    }
}

const SOURCE_KEYS: [&str; 10] = [
    "kind",
    "provider",
    "operation",
    "reporting_identity",
    "privacy",
    "search_engine",
    "search_type",
    "attribution",
    "timezone",
    "site_origin",
];
const COLLECTION_KEYS: [&str; 6] = [
    "collected_at",
    "state",
    "sampled",
    "thresholded",
    "truncated",
    "reason",
];
const URL_KEYS: [&str; 3] = ["field", "dimension", "kind"];
const METRIC_KEYS: [&str; 3] = ["name", "type", "unit"];
const MANIFEST_KEYS_FULL: [&str; 9] = [
    "format",
    "source",
    "period",
    "url",
    "metrics",
    "dimensions",
    "collection",
    "duplicate_policy",
    "row_shape",
];

fn present(value: Option<&Value>) -> bool {
    matches!(value, Some(v) if !v.is_null())
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(data))
}

/// JSON text for a JSON-safe value with `, ` and `: ` separators; non-ASCII
/// characters are not escaped.
pub fn dumps_spaced(value: &Value) -> String {
    let mut out = String::new();
    write_spaced(value, &mut out);
    out
}

fn write_spaced(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(text) => {
            out.push_str(&serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into()))
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                write_spaced(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (index, (key, item)) in map.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                out.push_str(&serde_json::to_string(key).unwrap_or_else(|_| "\"\"".into()));
                out.push_str(": ");
                write_spaced(item, out);
            }
            out.push('}');
        }
    }
}

/// Whether a string is a `YYYY-MM-DD` date with a real month and day.
fn is_iso_date(value: &Value) -> bool {
    let Some(text) = value.as_str() else {
        return false;
    };
    let parts: Vec<&str> = text.split('-').collect();
    if parts.len() != 3 || parts[0].len() != 4 || parts[1].len() != 2 || parts[2].len() != 2 {
        return false;
    }
    let (Ok(y), Ok(m), Ok(d)) = (
        parts[0].parse::<i64>(),
        parts[1].parse::<u32>(),
        parts[2].parse::<u32>(),
    ) else {
        return false;
    };
    if !(1..=12).contains(&m) {
        return false;
    }
    // Days per month; leap years follow the proleptic Gregorian rule.
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    (1..=days[(m - 1) as usize]).contains(&d)
}

/// An IANA zone name is valid when the system timezone database holds it.
fn is_known_timezone(name: &str) -> bool {
    if name.is_empty()
        || name.starts_with('/')
        || name
            .split('/')
            .any(|part| part.is_empty() || part == ".." || part == ".")
    {
        return false;
    }
    [
        "/usr/share/zoneinfo",
        "/var/db/timezone/zoneinfo",
        "/etc/zoneinfo",
    ]
    .iter()
    .any(|root| Path::new(root).join(name).is_file())
}

/// Validate an absolute http(s) origin with no path.
fn validated_origin(value: &Value) -> Result<Option<String>, EvidenceImportError> {
    if value.is_null() {
        return Ok(None);
    }
    let Some(text) = value.as_str() else {
        return Err(err("site_origin must be an absolute http(s) origin string"));
    };
    let (scheme, netloc, path, _query, _fragment) = crate::join::urlsplit(text.trim());
    if !["http", "https"].contains(&scheme.as_str()) || netloc.is_empty() || !path.is_empty() {
        return Err(err(format!(
            "site_origin must be an absolute http(s) origin without a path, got {}",
            py::repr(value)
        )));
    }
    Ok(Some(format!("{scheme}://{netloc}").to_lowercase()))
}

fn unknown_keys(map: &Map<String, Value>, allowed: &[&str]) -> Vec<String> {
    map.keys()
        .filter(|k| !allowed.contains(&k.as_str()))
        .cloned()
        .collect()
}

/// Check one declared mapping manifest.
pub fn validate_manifest(manifest: &Value) -> Result<Value, EvidenceImportError> {
    if manifest.is_null() {
        return Ok(json!({}));
    }
    if !manifest.is_object() || manifest.get("format") != Some(&json!(MAPPING_FORMAT)) {
        return Err(err(format!("mapping requires format {MAPPING_FORMAT}")));
    }
    let object = manifest.as_object().unwrap();
    let unknown = unknown_keys(object, &MANIFEST_KEYS_FULL);
    if !unknown.is_empty() {
        let mut sorted = unknown;
        sorted.sort();
        return Err(err(format!("mapping has unsupported keys: {sorted:?}")));
    }
    if let Some(source) = manifest.get("source") {
        if !source.is_object() {
            return Err(err("mapping source must be an object"));
        }
        let unknown = unknown_keys(source.as_object().unwrap(), &SOURCE_KEYS);
        if !unknown.is_empty() {
            let mut sorted = unknown;
            sorted.sort();
            return Err(err(format!(
                "mapping source has unsupported keys: {sorted:?}"
            )));
        }
        for name in [
            "kind",
            "provider",
            "operation",
            "reporting_identity",
            "privacy",
            "search_engine",
            "search_type",
            "attribution",
        ] {
            if present(source.get(name)) && !source[name].is_string() {
                return Err(err(format!("mapping source {name} must be a string")));
            }
        }
        if present(source.get("kind"))
            && !FILE_KINDS.contains(&source["kind"].as_str().unwrap_or_default())
        {
            return Err(err(format!(
                "mapping source kind must be one of {FILE_KINDS:?}"
            )));
        }
        if present(source.get("privacy"))
            && !["restricted", "supplied"].contains(&source["privacy"].as_str().unwrap_or_default())
        {
            return Err(err("mapping source privacy must be restricted or supplied"));
        }
        if present(source.get("timezone")) {
            let tz = &source["timezone"];
            if !tz.is_string() {
                return Err(err("mapping source timezone must be an IANA name string"));
            }
            if !is_known_timezone(tz.as_str().unwrap()) {
                return Err(err(format!(
                    "mapping source timezone is not a known IANA name: {}",
                    tz.as_str().unwrap()
                )));
            }
        }
        if present(source.get("site_origin")) {
            validated_origin(&source["site_origin"])?;
        }
    }
    if let Some(period) = manifest.get("period") {
        if !period.is_object()
            || !unknown_keys(period.as_object().unwrap(), &["start_date", "end_date"]).is_empty()
        {
            return Err(err("mapping period must hold only start_date and end_date"));
        }
        let start = &period["start_date"];
        let end = &period["end_date"];
        if start.is_null() != end.is_null() {
            return Err(err("mapping period requires both start_date and end_date"));
        }
        if !start.is_null()
            && (!is_iso_date(start) || !is_iso_date(end) || start.as_str() > end.as_str())
        {
            return Err(err(
                "mapping period must be an inclusive ordered pair of YYYY-MM-DD dates",
            ));
        }
    }
    if let Some(url_spec) = manifest.get("url") {
        if !url_spec.is_object()
            || !unknown_keys(url_spec.as_object().unwrap(), &URL_KEYS).is_empty()
        {
            return Err(err("mapping url may name only field, dimension and kind"));
        }
        if present(url_spec.get("kind"))
            && !URL_KINDS.contains(&url_spec["kind"].as_str().unwrap_or_default())
        {
            return Err(err(format!(
                "mapping url kind must be one of {URL_KINDS:?}"
            )));
        }
        for name in ["field", "dimension"] {
            if present(url_spec.get(name)) && !url_spec[name].is_string() {
                return Err(err(format!("mapping url {name} must be a string")));
            }
        }
    }
    if let Some(metrics) = manifest.get("metrics") {
        if !metrics.is_array() {
            return Err(err("mapping metrics must be a list"));
        }
        for metric in metrics.as_array().unwrap() {
            if !metric.is_object()
                || !unknown_keys(metric.as_object().unwrap(), &METRIC_KEYS).is_empty()
            {
                return Err(err("each metric may name only name, type and unit"));
            }
            if !metric["name"].is_string() || metric["name"].as_str().unwrap().is_empty() {
                return Err(err("each metric requires a non-empty name"));
            }
            if metric.get("type").cloned().unwrap_or(json!("number")) != json!("number") {
                return Err(err(format!(
                    "metric {:?} has unsupported type {:?}; only numeric metrics join evidence",
                    metric["name"].as_str().unwrap(),
                    metric.get("type")
                )));
            }
            if present(metric.get("unit")) && !metric["unit"].is_string() {
                return Err(err("metric unit must be a string"));
            }
        }
    }
    if let Some(dimensions) = manifest.get("dimensions") {
        let valid = dimensions.is_array()
            && dimensions
                .as_array()
                .unwrap()
                .iter()
                .all(|d| d.is_string() && !d.as_str().unwrap().is_empty());
        if !valid {
            return Err(err("mapping dimensions must be a list of non-empty names"));
        }
    }
    if let Some(collection) = manifest.get("collection") {
        if !collection.is_object()
            || !unknown_keys(collection.as_object().unwrap(), &COLLECTION_KEYS).is_empty()
        {
            return Err(err(format!(
                "mapping collection may name only {COLLECTION_KEYS:?}"
            )));
        }
        if let Some(state) = collection.get("state") {
            if !state.is_null() && !COLLECTION_STATES.contains(&state.as_str().unwrap_or_default())
            {
                return Err(err(format!(
                    "mapping collection state must be one of {COLLECTION_STATES:?}"
                )));
            }
        }
        for flag in ["sampled", "thresholded", "truncated"] {
            if present(collection.get(flag)) && !collection[flag].is_boolean() {
                return Err(err(format!("mapping collection {flag} must be a boolean")));
            }
        }
    }
    let duplicate_policy = manifest
        .get("duplicate_policy")
        .cloned()
        .unwrap_or(Value::Null);
    if !duplicate_policy.is_null()
        && !DUPLICATE_POLICIES.contains(&duplicate_policy.as_str().unwrap_or_default())
    {
        return Err(err(format!(
            "duplicate_policy must be one of {DUPLICATE_POLICIES:?}"
        )));
    }
    let row_shape = manifest.get("row_shape").cloned().unwrap_or(Value::Null);
    if !row_shape.is_null() && !ROW_SHAPES.contains(&row_shape.as_str().unwrap_or_default()) {
        return Err(err(format!("row_shape must be one of {ROW_SHAPES:?}")));
    }
    Ok(manifest.clone())
}

/// Normalize an evidence period to `{start_date, end_date}`.
fn period_from(value: &Value) -> Option<Value> {
    let (start, end) = if let Some(map) = value.as_object() {
        (
            map.get("start_date").cloned().unwrap_or(Value::Null),
            map.get("end_date").cloned().unwrap_or(Value::Null),
        )
    } else if let Some((left, right)) = value.as_str().and_then(|t| t.split_once("..")) {
        (json!(left), json!(right))
    } else if is_iso_date(value) {
        (value.clone(), value.clone())
    } else {
        return None;
    };
    if is_iso_date(&start) && is_iso_date(&end) && start.as_str() <= end.as_str() {
        let mut out = Map::new();
        out.insert("start_date".into(), start);
        out.insert("end_date".into(), end);
        Some(Value::Object(out))
    } else {
        None
    }
}

/// Map a coverage flag to true or false; unknown stays null.
fn flag(value: &Value) -> Value {
    match value {
        Value::Bool(b) => json!(b),
        Value::String(text) if ["unknown", "", "none"].contains(&text.as_str()) => Value::Null,
        Value::Null => Value::Null,
        other => json!(py::truthy(Some(other))),
    }
}

/// Merge a declared manifest into `(resolved, provenance)`, with envelope and result absent.
pub fn resolve_mapping(
    manifest: &Value,
    kind: &str,
    columns: &[String],
    rows: &[Value],
) -> Result<(Value, Value), EvidenceImportError> {
    let manifest = validate_manifest(manifest)?;
    let declared = manifest.get("source").cloned().unwrap_or(json!({}));
    let mut fields = Map::new();

    macro_rules! pick {
        ($name:expr, $manifest_value:expr, $envelope_value:expr, $contract_key:expr) => {{
            let provider = fields
                .get("provider")
                .and_then(|f| f.get("value"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let mv = $manifest_value;
            let ev = $envelope_value;
            if !mv.is_null() {
                let mut f = Map::new();
                f.insert("value".into(), mv.clone());
                f.insert("origin".into(), json!("declared"));
                f.insert("declared_by".into(), json!("manifest"));
                fields.insert($name.to_string(), Value::Object(f));
                mv
            } else if !ev.is_null() {
                let mut f = Map::new();
                f.insert("value".into(), ev.clone());
                f.insert("origin".into(), json!("envelope"));
                fields.insert($name.to_string(), Value::Object(f));
                ev
            } else if let Some(key) = $contract_key {
                if let Some(cv) = provider_contract(&provider, key) {
                    let mut f = Map::new();
                    f.insert("value".into(), json!(cv));
                    f.insert("origin".into(), json!("declared"));
                    f.insert("declared_by".into(), json!("provider_contract"));
                    fields.insert($name.to_string(), Value::Object(f));
                    json!(cv)
                } else {
                    let mut f = Map::new();
                    f.insert("value".into(), Value::Null);
                    f.insert("origin".into(), json!("unknown"));
                    fields.insert($name.to_string(), Value::Object(f));
                    Value::Null
                }
            } else {
                let mut f = Map::new();
                f.insert("value".into(), Value::Null);
                f.insert("origin".into(), json!("unknown"));
                fields.insert($name.to_string(), Value::Object(f));
                Value::Null
            }
        }};
    }

    let declared_get = |name: &str| declared.get(name).cloned().unwrap_or(Value::Null);

    let provider = pick!("provider", declared_get("provider"), Value::Null, None);
    let operation = pick!("operation", declared_get("operation"), Value::Null, None);
    let reporting_identity = pick!(
        "reporting_identity",
        declared_get("reporting_identity"),
        Value::Null,
        None
    );
    let site_origin = pick!(
        "site_origin",
        validated_origin(&declared_get("site_origin"))?.map_or(Value::Null, |s| json!(s)),
        Value::Null,
        None
    );
    let search_engine = pick!(
        "search_engine",
        declared_get("search_engine"),
        Value::Null,
        Some("search_engine")
    );
    let search_type = pick!(
        "search_type",
        declared_get("search_type"),
        Value::Null,
        Some("search_type")
    );
    let attribution = pick!(
        "attribution",
        declared_get("attribution"),
        Value::Null,
        None
    );
    let timezone = pick!(
        "timezone",
        declared_get("timezone"),
        Value::Null,
        Some("timezone")
    );
    let collected_at = pick!(
        "collected_at",
        manifest
            .get("collection")
            .and_then(|c| c.get("collected_at"))
            .cloned()
            .unwrap_or(Value::Null),
        Value::Null,
        None
    );
    let collection_state = pick!(
        "collection_state",
        manifest
            .get("collection")
            .and_then(|c| c.get("state"))
            .cloned()
            .unwrap_or(Value::Null),
        Value::Null,
        None
    );
    let manifest_collection = manifest.get("collection").cloned().unwrap_or(json!({}));
    let mget = |name: &str| {
        manifest_collection
            .get(name)
            .cloned()
            .unwrap_or(Value::Null)
    };
    let sampled = pick!("sampled", mget("sampled"), flag(&Value::Null), None);
    let thresholded = pick!("thresholded", mget("thresholded"), flag(&Value::Null), None);
    let truncated = pick!("truncated", mget("truncated"), flag(&Value::Null), None);

    let manifest_period = period_from(&manifest.get("period").cloned().unwrap_or(Value::Null));
    // Envelope and result periods are absent on the inline path.
    let period = manifest_period.clone();
    {
        let mut f = Map::new();
        f.insert("value".into(), period.clone().unwrap_or(Value::Null));
        f.insert(
            "origin".into(),
            json!(if manifest_period.is_some() {
                "declared"
            } else {
                "unknown"
            }),
        );
        if manifest_period.is_some() {
            f.insert("declared_by".into(), json!("manifest"));
        }
        fields.insert("period".into(), Value::Object(f));
    }

    let privacy = if py::truthy(declared.get("privacy")) {
        declared_get("privacy")
    } else if kind == "provider_evidence" || !provider.is_null() {
        json!("restricted")
    } else {
        json!("supplied")
    };
    {
        let mut f = Map::new();
        f.insert("value".into(), privacy.clone());
        f.insert("origin".into(), json!("declared"));
        f.insert(
            "declared_by".into(),
            json!(if present(declared.get("privacy")) {
                "manifest"
            } else {
                "import_default"
            }),
        );
        fields.insert("privacy".into(), Value::Object(f));
    }

    let mut row_shape = manifest.get("row_shape").cloned().unwrap_or(Value::Null);
    if row_shape.is_null() {
        let sample: Vec<&Value> = rows.iter().take(50).filter(|r| r.is_object()).collect();
        if sample.iter().any(|r| r.get("metricValues").is_some()) {
            row_shape = json!("ga4");
        } else if sample.iter().any(|r| r.get("keys").is_some()) {
            row_shape = json!("gsc");
        } else if provider == json!("ga4") {
            row_shape = json!(if operation == json!("page_views") {
                "flat"
            } else {
                "ga4"
            });
        } else if provider == json!("gsc") {
            row_shape = json!("gsc");
        } else {
            row_shape = json!("flat");
        }
    }

    let manifest_url = manifest.get("url").cloned().unwrap_or(json!({}));
    let url_kind = manifest_url
        .get("kind")
        .cloned()
        .unwrap_or(json!("absolute"));
    let mut url_field = manifest_url.get("field").cloned().unwrap_or(Value::Null);
    let url_dimension = manifest_url
        .get("dimension")
        .cloned()
        .unwrap_or(Value::Null);
    let dimensions: Vec<Value> = manifest
        .get("dimensions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if url_field.is_null() && url_dimension.is_null() {
        // Envelope dimensions are absent on the inline path.
        if row_shape == json!("flat") && columns.iter().any(|c| c == "url") {
            url_field = json!("url");
        } else if row_shape == json!("flat") {
            return Err(err(
                "cannot determine the URL field; declare mapping url.field",
            ));
        } else {
            return Err(err(
                "cannot determine the URL dimension; declare mapping url.dimension",
            ));
        }
    }
    {
        let mut f = Map::new();
        f.insert(
            "value".into(),
            json!({"field": url_field, "dimension": url_dimension, "kind": url_kind}),
        );
        f.insert(
            "origin".into(),
            json!(if present(manifest.get("url")) || !url_field.is_null() {
                "declared"
            } else {
                "unknown"
            }),
        );
        f.insert(
            "declared_by".into(),
            json!(if present(manifest.get("url")) {
                "manifest"
            } else {
                "import_default"
            }),
        );
        fields.insert("url_binding".into(), Value::Object(f));
    }

    let mut metrics = manifest.get("metrics").cloned().unwrap_or(Value::Null);
    if metrics.is_null() {
        let mut list = Vec::new();
        if provider == json!("ga4")
            && operation == json!("page_views")
            && row_shape == json!("flat")
        {
            list.push(json!({"name": "screenPageViews", "type": "integer", "unit": "count"}));
        } else if row_shape == json!("ga4") {
            // Result metrics are absent on the inline path.
        } else if row_shape == json!("gsc") {
            let known = ["clicks", "impressions", "ctr", "position"];
            let source_rows: Vec<&Value> = rows.iter().take(50).collect();
            let present: HashSet<&str> = source_rows
                .iter()
                .flat_map(|r| {
                    r.as_object()
                        .map(|m| m.keys().map(|k| k.as_str()).collect::<Vec<_>>())
                        .unwrap_or_default()
                })
                .filter(|k| *k != "keys")
                .collect();
            for name in known {
                if present.contains(name) {
                    let unit = if name == "ctr" {
                        "ratio"
                    } else if name == "position" {
                        "position"
                    } else {
                        "count"
                    };
                    list.push(json!({"name": name, "type": "number", "unit": unit}));
                }
            }
        }
        metrics = Value::Array(list);
    }
    let metrics_list = metrics.as_array().cloned().unwrap_or_default();
    {
        let mut f = Map::new();
        f.insert(
            "value".into(),
            json!(metrics_list
                .iter()
                .map(|m| m.get("name").cloned().unwrap_or(Value::Null))
                .collect::<Vec<_>>()),
        );
        f.insert(
            "origin".into(),
            json!(if present(manifest.get("metrics")) {
                "declared"
            } else if !metrics_list.is_empty() {
                "envelope"
            } else {
                "unknown"
            }),
        );
        f.insert(
            "declared_by".into(),
            json!(if present(manifest.get("metrics")) {
                "manifest"
            } else {
                "import_default"
            }),
        );
        fields.insert("metrics".into(), Value::Object(f));
    }
    {
        let mut f = Map::new();
        f.insert("value".into(), json!(dimensions));
        f.insert(
            "origin".into(),
            json!(if present(manifest.get("dimensions")) {
                "declared"
            } else {
                "unknown"
            }),
        );
        f.insert(
            "declared_by".into(),
            json!(if present(manifest.get("dimensions")) {
                "manifest"
            } else {
                "import_default"
            }),
        );
        fields.insert("dimensions".into(), Value::Object(f));
    }

    let collection = json!({
        "collected_at": collected_at,
        "state": if collection_state.is_null() { json!("complete") } else { collection_state },
        "sampled": sampled,
        "thresholded": thresholded,
        "truncated": truncated,
        "reason": mget("reason"),
    });
    let resolved = json!({
        "format": MAPPING_FORMAT,
        "source": {
            "kind": kind,
            "provider": provider,
            "operation": operation,
            "reporting_identity": reporting_identity,
            "privacy": privacy,
            "search_engine": search_engine,
            "search_type": search_type,
            "attribution": attribution,
            "timezone": timezone,
            "site_origin": site_origin,
        },
        "period": period.unwrap_or(Value::Null),
        "url": {"field": url_field, "dimension": url_dimension, "kind": url_kind},
        "metrics": metrics_list
            .iter()
            .map(|m| {
                json!({
                    "name": m.get("name").cloned().unwrap_or(Value::Null),
                    "type": "number",
                    "unit": m.get("unit").cloned().unwrap_or(json!("count")),
                })
            })
            .collect::<Vec<_>>(),
        "dimensions": dimensions,
        "collection": collection,
        "duplicate_policy": manifest
            .get("duplicate_policy")
            .cloned()
            .unwrap_or(json!("mark")),
        "row_shape": row_shape,
    });
    Ok((resolved, json!({"fields": Value::Object(fields)})))
}

/// `lookup` — flat column by exact name, then provider tail segment.
fn lookup<'a>(row: &'a Map<String, Value>, name: &str) -> Option<&'a Value> {
    if let Some(v) = row.get(name) {
        return Some(v);
    }
    if let Some(pos) = name.rfind(':') {
        let tail = &name[pos + 1..];
        if tail != name {
            return row.get(tail);
        }
    }
    None
}

/// `typed_metric` — one metric cell; `0` is measured, absence is never zero.
fn typed_metric(raw: Option<&Value>) -> Value {
    let mut entry = Map::new();
    entry.insert("value".into(), Value::Null);
    entry.insert("state".into(), json!("unavailable"));
    entry.insert("reason".into(), Value::Null);
    entry.insert("raw".into(), raw.cloned().unwrap_or(Value::Null));
    let Some(raw) = raw else {
        entry.insert("reason".into(), json!("absent"));
        return Value::Object(entry);
    };
    if raw.is_null() {
        entry.insert("reason".into(), json!("null"));
        return Value::Object(entry);
    }
    if raw.is_boolean() {
        entry.insert("reason".into(), json!("invalid"));
        return Value::Object(entry);
    }
    if raw.is_number() {
        let finite = raw.as_f64().map(f64::is_finite).unwrap_or(false);
        if finite {
            let mut out = Map::new();
            out.insert("value".into(), raw.clone());
            out.insert("state".into(), json!("measured"));
            out.insert("reason".into(), Value::Null);
            out.insert("raw".into(), raw.clone());
            return Value::Object(out);
        }
        entry.insert("reason".into(), json!("non_finite"));
        return Value::Object(entry);
    }
    if let Some(text) = raw.as_str() {
        let text = text.trim();
        if text.is_empty() {
            entry.insert("reason".into(), json!("empty"));
            return Value::Object(entry);
        }
        if text.starts_with('=') {
            entry.insert("reason".into(), json!("formula"));
            return Value::Object(entry);
        }
        if text.starts_with('<')
            || text.starts_with('≤')
            || SUPPRESSED_STRINGS.contains(&text.to_lowercase().as_str())
        {
            entry.insert("reason".into(), json!("suppressed"));
            return Value::Object(entry);
        }
        let value: Result<f64, _> = python_float(text);
        match value {
            Ok(v) if v.is_finite() => {
                let mut out = Map::new();
                out.insert("value".into(), json!(v));
                out.insert("state".into(), json!("measured"));
                out.insert("reason".into(), Value::Null);
                out.insert("raw".into(), raw.clone());
                return Value::Object(out);
            }
            Ok(_) => {
                entry.insert("reason".into(), json!("non_finite"));
                return Value::Object(entry);
            }
            Err(_) => {
                entry.insert("reason".into(), json!("invalid"));
                return Value::Object(entry);
            }
        }
    }
    entry.insert("reason".into(), json!("invalid"));
    Value::Object(entry)
}

/// Parse a float from text: trims whitespace, allows underscores between digits.
fn python_float(text: &str) -> Result<f64, std::num::ParseFloatError> {
    let cleaned: String = text.chars().filter(|c| *c != '_').collect();
    cleaned.parse::<f64>()
}

/// `dimension_rows` — declared dimension values at the provider grain.
fn dimension_rows(
    row: &Map<String, Value>,
    shape: &str,
    dimensions: &[Value],
) -> Map<String, Value> {
    let mut out = Map::new();
    if shape == "gsc" {
        let keys = row
            .get("keys")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for (index, name) in dimensions.iter().enumerate() {
            out.insert(
                name.as_str().unwrap_or_default().to_string(),
                keys.get(index).cloned().unwrap_or(Value::Null),
            );
        }
        return out;
    }
    if shape == "ga4" {
        let values = row
            .get("dimensionValues")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for (index, name) in dimensions.iter().enumerate() {
            let cell = values.get(index);
            let v = cell
                .and_then(|c| c.get("value"))
                .cloned()
                .unwrap_or(Value::Null);
            out.insert(name.as_str().unwrap_or_default().to_string(), v);
        }
        return out;
    }
    for name in dimensions {
        let n = name.as_str().unwrap_or_default();
        out.insert(
            n.to_string(),
            lookup(row, n).cloned().unwrap_or(Value::Null),
        );
    }
    out
}

/// `metric_rows` — declared metric values out of one raw row.
fn metric_rows(row: &Map<String, Value>, shape: &str, metrics: &[Value]) -> Vec<(String, Value)> {
    let mut out = Vec::with_capacity(metrics.len());
    if shape == "ga4" {
        let values = row
            .get("metricValues")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for (index, metric) in metrics.iter().enumerate() {
            let name = metric
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let raw = values.get(index).and_then(|c| c.get("value"));
            out.push((name.to_string(), typed_metric(raw)));
        }
        return out;
    }
    for metric in metrics {
        let name = metric
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        out.push((name.to_string(), typed_metric(lookup(row, name))));
    }
    out
}

/// `row_url` — resolve one row's URL into a strict join key.
fn row_url(
    row: &Map<String, Value>,
    dims: &Map<String, Value>,
    url_spec: &Map<String, Value>,
    site_origin: Option<&str>,
    _shape: &str,
) -> Value {
    let mut url = Map::new();
    url.insert("raw".into(), Value::Null);
    url.insert("resolved".into(), Value::Null);
    url.insert("normalized".into(), Value::Null);
    url.insert("state".into(), json!("unkeyable"));
    url.insert("reason".into(), Value::Null);
    let raw = if py::truthy(url_spec.get("dimension")) {
        dims.get(url_spec["dimension"].as_str().unwrap_or_default())
    } else {
        lookup(row, url_spec["field"].as_str().unwrap_or_default())
    };
    let Some(raw) = raw else {
        url.insert("reason".into(), json!("url_absent"));
        return Value::Object(url);
    };
    url.insert("raw".into(), raw.clone());
    if raw.is_null() {
        url.insert("reason".into(), json!("url_absent"));
        return Value::Object(url);
    }
    let Some(text0) = raw.as_str() else {
        url.insert("reason".into(), json!("url_invalid"));
        return Value::Object(url);
    };
    let text = text0.trim();
    if text.is_empty() {
        url.insert("reason".into(), json!("url_empty"));
        return Value::Object(url);
    }
    if NON_URL_STRINGS.contains(&text.to_lowercase().as_str()) {
        url.insert("reason".into(), json!("non_url_value"));
        return Value::Object(url);
    }
    let mut candidate = text.to_string();
    if url_spec.get("kind").and_then(Value::as_str) == Some("relative_path")
        && crate::join::urlsplit(text).0.is_empty()
    {
        let Some(origin) = site_origin else {
            url.insert("reason".into(), json!("missing_site_origin"));
            return Value::Object(url);
        };
        candidate = format!(
            "{origin}{}",
            if text.starts_with('/') {
                text.to_string()
            } else {
                format!("/{text}")
            }
        );
    }
    url.insert("resolved".into(), json!(candidate));
    match normalize_join_key(Some(&candidate)) {
        Some(normalized) => {
            url.insert("normalized".into(), json!(normalized));
            url.insert("state".into(), json!("keyed"));
        }
        None => {
            url.insert("reason".into(), json!("url_invalid"));
        }
    }
    Value::Object(url)
}

/// `normalize_rows` — normalize raw rows into one normalized-evidence document.
pub fn normalize_rows(
    rows: &[Value],
    resolved: &Value,
    provenance: &Value,
    file_meta: &Value,
) -> Result<Value, EvidenceImportError> {
    let source = resolved.get("source").cloned().unwrap_or(json!({}));
    let url_spec = resolved.get("url").cloned().unwrap_or(json!({}));
    let url_spec_map = url_spec.as_object().cloned().unwrap_or_default();
    let dimensions = resolved
        .get("dimensions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let shape = resolved
        .get("row_shape")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let metrics = resolved
        .get("metrics")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let metric_names: HashSet<&str> = metrics
        .iter()
        .filter_map(|m| m.get("name").and_then(Value::as_str))
        .collect();
    if metric_names.len() != metrics.len() {
        return Err(err("metric names must be unique in the declared mapping"));
    }
    let site_origin = source
        .get("site_origin")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    let url_dimension = url_spec.get("dimension").cloned().unwrap_or(Value::Null);
    let url_field = url_spec.get("field").cloned().unwrap_or(Value::Null);
    let extra_dimensions: Vec<&Value> = dimensions
        .iter()
        .filter(|name| {
            let n = name.as_str().unwrap_or_default();
            n != url_dimension.as_str().unwrap_or_default()
                && n != url_field.as_str().unwrap_or_default()
        })
        .collect();

    let mut seen: HashMap<Vec<String>, usize> = HashMap::new();
    let mut ambiguous_indexes: HashSet<usize> = HashSet::new();
    let mut normalized_rows: Vec<Value> = Vec::with_capacity(rows.len());
    let mut unkeyable_reasons: BTreeMap<String, i64> = BTreeMap::new();
    let mut metric_summary: Map<String, Value> = Map::new();
    for metric in &metrics {
        let name = metric
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        metric_summary.insert(
            name.to_string(),
            json!({"measured": 0, "unavailable": 0, "reasons": {}}),
        );
    }
    let mut keyed = 0i64;
    for (index, row) in rows.iter().enumerate() {
        let row_map = row.as_object().cloned().unwrap_or_default();
        let dims = dimension_rows(&row_map, &shape, &dimensions);
        let url = row_url(
            &row_map,
            &dims,
            &url_spec_map,
            site_origin.as_deref(),
            &shape,
        );
        let metric_entries = metric_rows(&row_map, &shape, &metrics);
        for (name, metric) in &metric_entries {
            let bucket = metric_summary
                .get_mut(name)
                .and_then(Value::as_object_mut)
                .expect("metric summary bucket");
            if metric.get("state") == Some(&json!("measured")) {
                let measured = bucket.get_mut("measured").unwrap();
                *measured = json!(measured.as_i64().unwrap_or(0) + 1);
            } else {
                let unavailable = bucket.get_mut("unavailable").unwrap();
                *unavailable = json!(unavailable.as_i64().unwrap_or(0) + 1);
                let reason = metric.get("reason").cloned().unwrap_or(Value::Null);
                let key = py::str_value(&reason);
                let reasons = bucket
                    .get_mut("reasons")
                    .and_then(Value::as_object_mut)
                    .unwrap();
                let counter = reasons.entry(key).or_insert(json!(0));
                *counter = json!(counter.as_i64().unwrap_or(0) + 1);
            }
        }
        let mut natural: Vec<Value> = vec![url.get("raw").cloned().unwrap_or(Value::Null)];
        for name in &extra_dimensions {
            natural.push(
                dims.get(name.as_str().unwrap_or_default())
                    .cloned()
                    .unwrap_or(Value::Null),
            );
        }
        let natural_key: Vec<String> = natural.iter().map(dumps_spaced).collect();
        if let Some(&prev) = seen.get(&natural_key) {
            ambiguous_indexes.insert(prev);
            ambiguous_indexes.insert(index);
        } else {
            seen.insert(natural_key, index);
        }
        if url.get("state") == Some(&json!("keyed")) {
            keyed += 1;
        } else {
            let reason = url.get("reason").cloned().unwrap_or(Value::Null);
            *unkeyable_reasons.entry(py::str_value(&reason)).or_insert(0) += 1;
        }
        let mut metrics_obj = Map::new();
        for ((name, metric), spec) in metric_entries.iter().zip(metrics.iter()) {
            let mut entry = metric.as_object().cloned().unwrap_or_default();
            entry.insert(
                "unit".into(),
                spec.get("unit").cloned().unwrap_or(Value::Null),
            );
            metrics_obj.insert(name.clone(), Value::Object(entry));
        }
        normalized_rows.push(json!({
            "row_index": index,
            "natural_key": natural,
            "natural_key_sha256": sha256_hex(dumps_spaced(&Value::Array(natural)).as_bytes()),
            "dimensions": Value::Object(dims),
            "url": url,
            "metrics": Value::Object(metrics_obj),
            "ambiguous": false,
            "raw": row.clone(),
        }));
    }
    if !ambiguous_indexes.is_empty() && resolved.get("duplicate_policy") == Some(&json!("reject")) {
        return Err(err(format!(
            "{} rows share a duplicate natural key at the declared grain; duplicate_policy is reject",
            ambiguous_indexes.len()
        )));
    }
    for index in &ambiguous_indexes {
        normalized_rows[*index]
            .as_object_mut()
            .unwrap()
            .insert("ambiguous".into(), json!(true));
    }
    let mut unkeyable_obj = Map::new();
    for (reason, count) in unkeyable_reasons {
        unkeyable_obj.insert(reason, json!(count));
    }
    let total_rows = normalized_rows.len() as i64;
    let document = json!({
        "format": NORMALIZED_FORMAT,
        "mapping": resolved,
        "provenance": {
            "fields": provenance.get("fields").cloned().unwrap_or(json!({})),
            "file": file_meta,
            "artifact_reference": Value::Null,
        },
        "rows": normalized_rows,
        "summary": {
            "rows": total_rows,
            "keyed": keyed,
            "unkeyable": total_rows - keyed,
            "unkeyable_reasons": Value::Object(unkeyable_obj),
            "ambiguous": ambiguous_indexes.len() as i64,
            "metrics": Value::Object(metric_summary),
            "collection": resolved.get("collection").cloned().unwrap_or(Value::Null),
        },
    });
    Ok(document)
}

/// `normalize_inline` — normalize an already-parsed row list against a
/// declared manifest.
pub fn normalize_inline(rows: &Value, manifest: &Value) -> Result<Value, EvidenceImportError> {
    let row_list = rows.as_array();
    if row_list.is_none()
        || row_list.map(|r| r.len() > MAX_ROWS).unwrap_or(false)
        || row_list
            .map(|r| r.iter().any(|row| !row.is_object()))
            .unwrap_or(false)
    {
        return Err(err("rows must be a bounded list of row objects"));
    }
    let row_list = row_list.unwrap();
    let manifest = validate_manifest(manifest)?;
    let kind = manifest
        .get("source")
        .and_then(|s| s.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or("json")
        .to_string();
    let mut column_set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for row in row_list {
        for key in row.as_object().unwrap().keys() {
            column_set.insert(key.clone());
        }
    }
    let columns: Vec<String> = column_set.into_iter().collect();
    let (resolved, provenance) = resolve_mapping(&manifest, &kind, &columns, row_list)?;
    normalize_rows(
        row_list,
        &resolved,
        &provenance,
        &json!({"name": "inline", "sha256": Value::Null, "bytes": Value::Null, "kind": kind}),
    )
}

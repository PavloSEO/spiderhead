//! Split out of `evidence_import`.

use super::*;

pub(super) fn validated_origin(value: Option<&Value>) -> Result<Option<String>, CfError> {
    let Some(raw) = value else {
        return Ok(None);
    };
    let Some(text) = raw.as_str() else {
        return Err(err("site_origin must be an absolute http(s) origin string"));
    };
    let trimmed = text.trim();
    let (scheme, rest) = trimmed.split_once("://").unwrap_or(("", trimmed));
    let netloc = rest.split('/').next().unwrap_or("");
    let path = rest.strip_prefix(netloc).unwrap_or("");
    if !["http", "https"].contains(&scheme) || netloc.is_empty() || !["", "/"].contains(&path) {
        return Err(err(format!(
            "site_origin must be an absolute http(s) origin without a path, got '{}'",
            text.replace('\'', "\\'")
        )));
    }
    Ok(Some(format!("{scheme}://{}", netloc.to_lowercase())))
}

/// `validate_manifest(manifest)` — strict data-only manifest check.
pub fn validate_manifest(manifest: &Value) -> Result<Value, CfError> {
    if manifest.is_null() {
        return Ok(json!({}));
    }
    let map = manifest.as_object();
    if map.is_none() || manifest.get("format") != Some(&json!(MAPPING_FORMAT)) {
        return Err(err(format!("mapping requires format {MAPPING_FORMAT}")));
    }
    let map = map.expect("checked");
    let unknown = unknown_keys(map, &MANIFEST_KEYS);
    if !unknown.is_empty() {
        return Err(err(format!(
            "mapping has unsupported keys: {}",
            sorted_repr(unknown)
        )));
    }
    if let Some(source) = get(manifest, "source") {
        let Some(src) = source.as_object() else {
            return Err(err("mapping source must be an object"));
        };
        let unknown = unknown_keys(src, &SOURCE_KEYS);
        if !unknown.is_empty() {
            return Err(err(format!(
                "mapping source has unsupported keys: {}",
                sorted_repr(unknown)
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
            if let Some(value) = get(source, name) {
                if !value.is_string() {
                    return Err(err(format!("mapping source {name} must be a string")));
                }
            }
        }
        if let Some(kind) = str_field(source, "kind") {
            if !FILE_KINDS.contains(&kind) {
                return Err(err("mapping source kind must be one of ('csv', 'xlsx', 'json', 'provider_evidence')"));
            }
        }
        if let Some(privacy) = str_field(source, "privacy") {
            if !["restricted", "supplied"].contains(&privacy) {
                return Err(err("mapping source privacy must be restricted or supplied"));
            }
        }
        if let Some(tz) = get(source, "timezone") {
            let Some(name) = tz.as_str() else {
                return Err(err("mapping source timezone must be an IANA name string"));
            };
            if !tz_known(name) {
                return Err(err(format!(
                    "mapping source timezone is not a known IANA name: '{name}'"
                )));
            }
        }
        if get(source, "site_origin").is_some() {
            validated_origin(get(source, "site_origin"))?;
        }
    }
    if let Some(period) = get(manifest, "period") {
        let Some(spec) = period.as_object() else {
            return Err(err("mapping period must hold only start_date and end_date"));
        };
        if !unknown_keys(spec, &["start_date", "end_date"]).is_empty() {
            return Err(err("mapping period must hold only start_date and end_date"));
        }
        let start = spec.get("start_date").filter(|v| !v.is_null());
        let end = spec.get("end_date").filter(|v| !v.is_null());
        if start.is_none() != end.is_none() {
            return Err(err("mapping period requires both start_date and end_date"));
        }
        if let (Some(s), Some(e)) = (start, end) {
            if !is_iso_date(s) || !is_iso_date(e) || s.as_str() > e.as_str() {
                return Err(err(
                    "mapping period must be an inclusive ordered pair of YYYY-MM-DD dates",
                ));
            }
        }
    }
    if let Some(url_spec) = get(manifest, "url") {
        let Some(spec) = url_spec.as_object() else {
            return Err(err("mapping url may name only field, dimension and kind"));
        };
        if !unknown_keys(spec, &URL_KEYS).is_empty() {
            return Err(err("mapping url may name only field, dimension and kind"));
        }
        if let Some(kind) = str_field(url_spec, "kind") {
            if !URL_KINDS.contains(&kind) {
                return Err(err(
                    "mapping url kind must be one of ('absolute', 'relative_path')",
                ));
            }
        }
        for name in ["field", "dimension"] {
            if let Some(value) = get(url_spec, name) {
                if !value.is_string() {
                    return Err(err(format!("mapping url {name} must be a string")));
                }
            }
        }
    }
    if let Some(metrics) = get(manifest, "metrics") {
        let Some(list) = metrics.as_array() else {
            return Err(err("mapping metrics must be a list"));
        };
        for metric in list {
            let Some(spec) = metric.as_object() else {
                return Err(err("each metric may name only name, type and unit"));
            };
            if !unknown_keys(spec, &METRIC_KEYS).is_empty() {
                return Err(err("each metric may name only name, type and unit"));
            }
            match str_field(metric, "name") {
                Some(name) if !name.is_empty() => {}
                _ => return Err(err("each metric requires a non-empty name")),
            }
            let mtype = str_field(metric, "type").unwrap_or("number");
            if mtype != "number" {
                return Err(err(format!(
                    "metric '{}' has unsupported type '{}'; only numeric metrics join evidence",
                    pystr::repr_value(&metric["name"]),
                    mtype
                )));
            }
            if let Some(unit) = get(metric, "unit") {
                if !unit.is_string() {
                    return Err(err("metric unit must be a string"));
                }
            }
        }
    }
    if let Some(dimensions) = get(manifest, "dimensions") {
        let ok = dimensions
            .as_array()
            .map(|list| {
                list.iter()
                    .all(|v| v.as_str().is_some_and(|s| !s.is_empty()))
            })
            .unwrap_or(false);
        if !ok {
            return Err(err("mapping dimensions must be a list of non-empty names"));
        }
    }
    if let Some(collection) = get(manifest, "collection") {
        let Some(spec) = collection.as_object() else {
            return Err(err(format!(
                "mapping collection may name only {}",
                pystr::repr(&Value::Array(
                    [
                        "collected_at",
                        "reason",
                        "sampled",
                        "state",
                        "thresholded",
                        "truncated"
                    ]
                    .iter()
                    .map(|k| json!(k))
                    .collect()
                ))
            )));
        };
        let unknown = unknown_keys(spec, &COLLECTION_KEYS);
        if !unknown.is_empty() {
            return Err(err(format!(
                "mapping collection may name only {}",
                pystr::repr(&Value::Array(
                    [
                        "collected_at",
                        "reason",
                        "sampled",
                        "state",
                        "thresholded",
                        "truncated"
                    ]
                    .iter()
                    .map(|k| json!(k))
                    .collect()
                ))
            )));
        }
        if let Some(state) = str_field(collection, "state") {
            if !COLLECTION_STATES.contains(&state) {
                return Err(err("mapping collection state must be one of ('complete', 'partial', 'failed', 'skipped', 'not_configured')"));
            }
        }
        for flag in ["sampled", "thresholded", "truncated"] {
            if let Some(value) = get(collection, flag) {
                if !value.is_boolean() {
                    return Err(err(format!("mapping collection {flag} must be a boolean")));
                }
            }
        }
    }
    if let Some(policy) = str_field(manifest, "duplicate_policy") {
        if !DUPLICATE_POLICIES.contains(&policy) {
            return Err(err("duplicate_policy must be one of ('mark', 'reject')"));
        }
    }
    if let Some(shape) = str_field(manifest, "row_shape") {
        if !ROW_SHAPES.contains(&shape) {
            return Err(err("row_shape must be one of ('flat', 'gsc', 'ga4')"));
        }
    }
    Ok(manifest.clone())
}

#[allow(clippy::question_mark)]
pub(super) fn period_from(value: &Value) -> Option<Value> {
    let (start, end) = if let Value::Object(map) = value {
        (map.get("start_date"), map.get("end_date"))
    } else if let Some(text) = value.as_str() {
        if let Some((s, e)) = text.split_once("..") {
            return period_from(&json!({"start_date": s, "end_date": e}));
        }
        if is_iso_date(value) {
            (Some(value), Some(value))
        } else {
            return None;
        }
    } else {
        return None;
    };
    match (start, end) {
        (Some(s), Some(e)) if is_iso_date(s) && is_iso_date(e) && s.as_str() <= e.as_str() => {
            Some(json!({"start_date": s, "end_date": e}))
        }
        _ => None,
    }
}

pub(super) fn flag(value: Option<&Value>) -> Option<Value> {
    match value {
        Some(Value::Bool(b)) => Some(json!(b)),
        Some(Value::String(s)) if ["unknown", "", "none"].contains(&s.as_str()) => None,
        Some(v) => Some(json!(truthy(Some(v)))),
        None => None,
    }
}

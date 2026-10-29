//! Split out of `evidence_join`.

use super::*;

/// `_field(document, name)` — `provenance.fields[name].value`.
pub(super) fn field(document: &Value, name: &str) -> Result<Value, CfError> {
    let provenance = get(document, "provenance")?
        .filter(|v| pystr::truthy(Some(v)))
        .unwrap_or(json!({}));
    let fields = get(&provenance, "fields")?
        .filter(|v| pystr::truthy(Some(v)))
        .unwrap_or(json!({}));
    let entry = get(&fields, name)?
        .filter(|v| pystr::truthy(Some(v)))
        .unwrap_or(json!({}));
    Ok(get(&entry, "value")?.unwrap_or(Value::Null))
}

/// `_window(value)` → `(start_ord, end_ord)` day ordinals.
pub(super) fn window(value: &Value) -> Result<Option<(i64, i64)>, CfError> {
    if !value.is_object() {
        return Ok(None);
    }
    let parse = |key: &str| -> Result<Option<i64>, CfError> {
        let raw = value.get(key).filter(|v| pystr::truthy(Some(v)));
        match raw {
            None => Ok(pytime::date_fromisoformat("")),
            Some(Value::String(text)) => Ok(pytime::date_fromisoformat(text)),
            Some(other) => Err(CfError::Python(
                "TypeError",
                format!(
                    "fromisoformat: argument must be str, not {}",
                    pystr::type_name(other)
                ),
            )),
        }
    };
    let Some(start) = parse("start_date")? else {
        return Ok(None);
    };
    let Some(end) = parse("end_date")? else {
        return Ok(None);
    };
    Ok((start <= end).then_some((start, end)))
}

pub(super) fn validate_policy(policy: Option<&Value>) -> Result<Map<String, Value>, CfError> {
    let Some(spec) = policy.filter(|v| !v.is_null()) else {
        return Ok(Map::new());
    };
    let entries = spec.as_object().ok_or_else(|| {
        import_error(format!(
            "comparison policy may name only {}",
            pystr::repr(&json!(["boundary_policy", "cross_source", "quadrant"]))
        ))
    })?;
    if entries.keys().any(|k| !POLICY_KEYS.contains(&k.as_str())) {
        return Err(import_error(format!(
            "comparison policy may name only {}",
            pystr::repr(&json!(["boundary_policy", "cross_source", "quadrant"]))
        )));
    }
    if let Some(boundary) = entries.get("boundary_policy") {
        if !boundary.is_null()
            && !BOUNDARY_POLICIES.contains(&boundary.as_str().unwrap_or_default())
        {
            return Err(import_error(
                "boundary_policy must be one of ('strict', 'local_calendar')",
            ));
        }
    }
    if let Some(cross) = entries.get("cross_source") {
        if !cross.is_null() && cross != &json!("juxtapose") {
            return Err(import_error(
                "cross_source must be \"juxtapose\" when declared",
            ));
        }
    }
    if let Some(quadrant) = entries.get("quadrant") {
        if !quadrant.is_null() {
            let q = quadrant.as_object().ok_or_else(|| {
                import_error("policy quadrant may name only left_metric and right_metric")
            })?;
            if q.keys()
                .any(|k| !matches!(k.as_str(), "left_metric" | "right_metric"))
            {
                return Err(import_error(
                    "policy quadrant may name only left_metric and right_metric",
                ));
            }
            for name in ["left_metric", "right_metric"] {
                if !q
                    .get(name)
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty())
                {
                    return Err(import_error(format!(
                        "policy quadrant requires a {name} string"
                    )));
                }
            }
        }
    }
    Ok(entries.clone())
}

pub(super) fn aspect(
    reasons: &mut Vec<Value>,
    verdict: &str,
    reason: &str,
    aspect_name: &str,
    extra: &[(&str, Value)],
) {
    let mut entry = json!({"aspect": aspect_name, "verdict": verdict, "reason": reason});
    for (key, value) in extra {
        entry[key.to_string()] = value.clone();
    }
    reasons.push(entry);
}

pub(super) fn mapping_of(document: &Value) -> Map<String, Value> {
    document
        .get("mapping")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

pub(super) fn keyed_urls(document: &Value) -> Vec<String> {
    let mut keys = Vec::new();
    for row in document
        .get("rows")
        .filter(|v| pystr::truthy(Some(v)))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let url = row.get("url").cloned().unwrap_or(json!({}));
        if url.get("state") == Some(&json!("keyed")) {
            if let Some(key) = url.get("normalized").and_then(Value::as_str) {
                if !keys.contains(&key.to_owned()) {
                    keys.push(key.to_owned());
                }
            }
        }
    }
    keys
}

pub(super) fn measured_keys(document: &Value, metric: &str) -> Vec<String> {
    let mut keys = Vec::new();
    for row in document
        .get("rows")
        .filter(|v| pystr::truthy(Some(v)))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let url = row.get("url").cloned().unwrap_or(json!({}));
        if url.get("state") != Some(&json!("keyed")) {
            continue;
        }
        let entry = row
            .get("metrics")
            .and_then(|m| m.get(metric))
            .cloned()
            .unwrap_or(json!({}));
        if entry.get("state") == Some(&json!("measured")) {
            if let Some(key) = url.get("normalized").and_then(Value::as_str) {
                if !keys.contains(&key.to_owned()) {
                    keys.push(key.to_owned());
                }
            }
        }
    }
    keys
}

pub(super) fn zero_measured_keys(document: &Value, metric: &str) -> Vec<String> {
    let mut keys = Vec::new();
    for row in document
        .get("rows")
        .filter(|v| pystr::truthy(Some(v)))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let url = row.get("url").cloned().unwrap_or(json!({}));
        if url.get("state") != Some(&json!("keyed")) {
            continue;
        }
        let entry = row
            .get("metrics")
            .and_then(|m| m.get(metric))
            .cloned()
            .unwrap_or(json!({}));
        if entry.get("state") == Some(&json!("measured"))
            && entry.get("value").and_then(Value::as_f64) == Some(0.0)
        {
            if let Some(key) = url.get("normalized").and_then(Value::as_str) {
                if !keys.contains(&key.to_owned()) {
                    keys.push(key.to_owned());
                }
            }
        }
    }
    keys
}

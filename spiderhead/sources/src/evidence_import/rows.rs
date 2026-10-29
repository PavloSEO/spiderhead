//! Split out of `evidence_import`.

use super::*;

pub(super) fn pick(
    fields: &mut Map<String, Value>,
    name: &str,
    manifest_value: Option<&Value>,
    envelope_value: Option<&Value>,
    contract_key: Option<&str>,
    envelope: &Value,
) -> Option<Value> {
    if let Some(value) = manifest_value {
        fields.insert(
            name.to_owned(),
            json!({"value": value, "origin": "declared", "declared_by": "manifest"}),
        );
        return Some(value.clone());
    }
    if let Some(value) = envelope_value {
        fields.insert(
            name.to_owned(),
            json!({"value": value, "origin": "envelope"}),
        );
        return Some(value.clone());
    }
    if let Some(key) = contract_key {
        let provider = fields
            .get("provider")
            .and_then(|f| f.get("value"))
            .and_then(Value::as_str)
            .or_else(|| get(envelope, "provider").and_then(Value::as_str))
            .unwrap_or("");
        let contract_value = match (provider, key) {
            ("gsc", "search_engine") => Some("google"),
            ("gsc", "search_type") => Some("web"),
            ("gsc", "timezone") => Some("America/Los_Angeles"),
            ("bing_webmaster", "search_engine") => Some("bing"),
            ("bing_webmaster", "search_type") => Some("web"),
            ("yandex_webmaster", "search_engine") => Some("yandex"),
            ("yandex_webmaster", "search_type") => Some("web"),
            _ => None,
        };
        if let Some(value) = contract_value {
            fields.insert(
                name.to_owned(),
                json!({
                    "value": value,
                    "origin": "declared",
                    "declared_by": "provider_contract",
                }),
            );
            return Some(json!(value));
        }
    }
    fields.insert(
        name.to_owned(),
        json!({"value": Value::Null, "origin": "unknown"}),
    );
    None
}

pub(super) fn lookup<'a>(row: &'a Value, name: &str) -> Option<&'a Value> {
    let map = row.as_object()?;
    if let Some(value) = map.get(name) {
        return Some(value);
    }
    let tail = name.rsplit(':').next().unwrap_or(name);
    if tail != name {
        if let Some(value) = map.get(tail) {
            return Some(value);
        }
    }
    None
}

pub(super) fn typed_metric(raw: Option<&Value>) -> Value {
    let mut entry = json!({
        "value": Value::Null,
        "state": "unavailable",
        "reason": Value::Null,
        "raw": raw.cloned().unwrap_or(Value::Null),
    });
    let Some(raw) = raw else {
        entry["reason"] = json!("absent");
        return entry;
    };
    match raw {
        Value::Null => entry["reason"] = json!("null"),
        Value::Bool(_) => entry["reason"] = json!("invalid"),
        Value::Number(n) => {
            if n.as_f64().is_some_and(f64::is_finite) {
                return json!({"value": raw, "state": "measured", "reason": Value::Null, "raw": raw});
            }
            entry["reason"] = json!("non_finite");
        }
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                entry["reason"] = json!("empty");
            } else if trimmed.starts_with('=') {
                entry["reason"] = json!("formula");
            } else if trimmed.starts_with('<')
                || trimmed.starts_with('≤')
                || ["(other)", "(not set)"].contains(&trimmed.to_lowercase().as_str())
            {
                entry["reason"] = json!("suppressed");
            } else {
                match trimmed.parse::<f64>() {
                    Ok(value) if value.is_finite() => {
                        return json!({"value": value, "state": "measured", "reason": Value::Null, "raw": raw});
                    }
                    Ok(_) => entry["reason"] = json!("non_finite"),
                    Err(_) => entry["reason"] = json!("invalid"),
                }
            }
        }
        _ => entry["reason"] = json!("invalid"),
    }
    entry
}

pub(super) fn dimension_rows(
    row: &Value,
    shape: &str,
    dimensions: &[String],
) -> Map<String, Value> {
    let mut out = Map::new();
    match shape {
        "gsc" => {
            let keys = row.get("keys").and_then(Value::as_array);
            for (index, name) in dimensions.iter().enumerate() {
                out.insert(
                    name.clone(),
                    keys.and_then(|k| k.get(index))
                        .cloned()
                        .unwrap_or(Value::Null),
                );
            }
        }
        "ga4" => {
            let values = row.get("dimensionValues").and_then(Value::as_array);
            for (index, name) in dimensions.iter().enumerate() {
                let cell = values.and_then(|v| v.get(index));
                out.insert(
                    name.clone(),
                    cell.and_then(|c| c.get("value"))
                        .cloned()
                        .unwrap_or(Value::Null),
                );
            }
        }
        _ => {
            for name in dimensions {
                out.insert(
                    name.clone(),
                    lookup(row, name).cloned().unwrap_or(Value::Null),
                );
            }
        }
    }
    out
}

pub(super) fn metric_rows(row: &Value, shape: &str, metrics: &[Value]) -> Map<String, Value> {
    let mut out = Map::new();
    if shape == "ga4" {
        let values = row.get("metricValues").and_then(Value::as_array);
        for (index, metric) in metrics.iter().enumerate() {
            let name = metric
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let cell = values.and_then(|v| v.get(index));
            let raw = cell.and_then(|c| c.get("value"));
            out.insert(name.to_owned(), typed_metric(raw));
        }
        return out;
    }
    for metric in metrics {
        let name = metric
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        out.insert(name.to_owned(), typed_metric(lookup(row, name)));
    }
    out
}

pub(super) fn row_url(
    row: &Value,
    dims: &Map<String, Value>,
    url_spec: &Value,
    site_origin: Option<&str>,
) -> Value {
    let mut url = json!({
        "raw": Value::Null,
        "resolved": Value::Null,
        "normalized": Value::Null,
        "state": "unkeyable",
        "reason": Value::Null,
    });
    let raw = if let Some(dimension) = str_field(url_spec, "dimension") {
        dims.get(dimension)
    } else {
        str_field(url_spec, "field").and_then(|f| lookup(row, f))
    };
    let Some(raw) = raw else {
        url["reason"] = json!("url_absent");
        return url;
    };
    url["raw"] = raw.clone();
    if raw.is_null() {
        url["reason"] = json!("url_absent");
        return url;
    }
    let Some(text_raw) = raw.as_str() else {
        url["reason"] = json!("url_invalid");
        return url;
    };
    let text = text_raw.trim();
    if text.is_empty() {
        url["reason"] = json!("url_empty");
        return url;
    }
    if ["(not set)", "(other)"].contains(&text.to_lowercase().as_str()) {
        url["reason"] = json!("non_url_value");
        return url;
    }
    let mut candidate = text.to_owned();
    if str_field(url_spec, "kind") == Some("relative_path") && !text.contains("://") {
        let Some(origin) = site_origin else {
            url["reason"] = json!("missing_site_origin");
            return url;
        };
        candidate = format!(
            "{origin}{}",
            if text.starts_with('/') {
                text.to_owned()
            } else {
                format!("/{text}")
            }
        );
    }
    url["resolved"] = json!(candidate);
    match normalize_join_key(Some(&json!(candidate)), false, false, false) {
        Ok(Some(normalized)) => {
            url["normalized"] = json!(normalized);
            url["state"] = json!("keyed");
        }
        _ => url["reason"] = json!("url_invalid"),
    }
    url
}

pub(super) fn natural_component(value: &Value) -> String {
    match value {
        Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Null => {
            crate::pyjson::dumps_opts(value, false, false)
        }
        other => pystr::repr_value(other),
    }
}

/// `normalize_rows(rows, resolved, provenance, file_meta)`.
pub fn normalize_rows(
    rows: &[Value],
    resolved: &Value,
    provenance: &Value,
    file_meta: &Value,
) -> Result<Value, CfError> {
    let source = &resolved["source"];
    let url_spec = &resolved["url"];
    let dimensions: Vec<String> = resolved
        .get("dimensions")
        .and_then(Value::as_array)
        .map(|l| {
            l.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let shape = str_field(resolved, "row_shape").unwrap_or("flat");
    let metrics_spec = resolved
        .get("metrics")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let names: Vec<&str> = metrics_spec
        .iter()
        .filter_map(|m| m.get("name").and_then(Value::as_str))
        .collect();
    if names.iter().collect::<BTreeSet<_>>().len() != names.len() {
        return Err(err("metric names must be unique in the declared mapping"));
    }
    let site_origin = str_field(source, "site_origin");
    let url_dimension = str_field(url_spec, "dimension");
    let url_field = str_field(url_spec, "field");
    let extra: Vec<&String> = dimensions
        .iter()
        .filter(|name| Some(name.as_str()) != url_dimension && Some(name.as_str()) != url_field)
        .collect();
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut ambiguous: BTreeSet<usize> = BTreeSet::new();
    let mut out_rows = Vec::new();
    let mut unkeyable_reasons: Map<String, Value> = Map::new();
    let mut metric_summary = Map::new();
    for metric in &metrics_spec {
        let name = metric
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        metric_summary.insert(
            name.to_owned(),
            json!({"measured": 0, "unavailable": 0, "reasons": {}}),
        );
    }
    let mut keyed = 0usize;
    for (index, row) in rows.iter().enumerate() {
        let dims = dimension_rows(row, shape, &dimensions);
        let url = row_url(row, &dims, url_spec, site_origin);
        let metrics = metric_rows(row, shape, &metrics_spec);
        for (name, metric) in &metrics {
            if let Some(bucket) = metric_summary.get_mut(name) {
                if metric["state"] == "measured" {
                    bucket["measured"] = json!(bucket["measured"].as_i64().unwrap_or(0) + 1);
                } else {
                    bucket["unavailable"] = json!(bucket["unavailable"].as_i64().unwrap_or(0) + 1);
                    let reason = metric["reason"].as_str().unwrap_or("invalid");
                    let reasons = bucket["reasons"].as_object_mut().expect("obj");
                    let count = reasons.get(reason).and_then(Value::as_i64).unwrap_or(0);
                    reasons.insert(reason.to_owned(), json!(count + 1));
                }
            }
        }
        let mut natural: Vec<Value> = vec![url["raw"].clone()];
        for name in &extra {
            natural.push(dims.get(*name).cloned().unwrap_or(Value::Null));
        }
        let key: Vec<String> = natural.iter().map(natural_component).collect();
        let key_joined = key.join("\u{1f}");
        if let Some(first) = seen.get(&key_joined) {
            ambiguous.insert(*first);
            ambiguous.insert(index);
        } else {
            seen.insert(key_joined, index);
        }
        if url["state"] == "keyed" {
            keyed += 1;
        } else {
            let reason = url["reason"].as_str().unwrap_or("unkeyable").to_owned();
            let count = unkeyable_reasons
                .get(&reason)
                .and_then(Value::as_i64)
                .unwrap_or(0);
            unkeyable_reasons.insert(reason, json!(count + 1));
        }
        let digest = sha256(
            format!(
                "[{}]",
                natural
                    .iter()
                    .map(natural_component)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
            .as_bytes(),
        );
        let mut metrics_with_units = Map::new();
        for (i, (name, metric)) in metrics.iter().enumerate() {
            let mut m = metric.clone();
            m["unit"] = metrics_spec
                .get(i)
                .and_then(|s| s.get("unit"))
                .cloned()
                .unwrap_or(json!("count"));
            metrics_with_units.insert(name.clone(), m);
        }
        out_rows.push(json!({
            "row_index": index,
            "natural_key": natural,
            "natural_key_sha256": digest,
            "dimensions": Value::Object(dims),
            "url": url,
            "metrics": Value::Object(metrics_with_units),
            "ambiguous": false,
            "raw": row,
        }));
    }
    if !ambiguous.is_empty() && str_field(resolved, "duplicate_policy") == Some("reject") {
        return Err(err(format!(
            "{} rows share a duplicate natural key at the declared grain; \
             duplicate_policy is reject",
            ambiguous.len()
        )));
    }
    for index in &ambiguous {
        out_rows[*index]["ambiguous"] = json!(true);
    }
    Ok(json!({
        "format": NORMALIZED_FORMAT,
        "mapping": resolved,
        "provenance": {
            "fields": provenance.get("fields").cloned().unwrap_or(json!({})),
            "file": file_meta,
            "artifact_reference": Value::Null,
        },
        "rows": out_rows,
        "summary": {
            "rows": rows.len(),
            "keyed": keyed,
            "unkeyable": rows.len() - keyed,
            "unkeyable_reasons": Value::Object(unkeyable_reasons),
            "ambiguous": ambiguous.len(),
            "metrics": Value::Object(metric_summary),
            "collection": resolved.get("collection").cloned().unwrap_or(json!({})),
        },
    }))
}

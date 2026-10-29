//! Split out of `evidence_import`.

use super::*;

/// `resolve_mapping(manifest, *, kind, envelope, result, columns, rows)`.
pub fn resolve_mapping(manifest: &Value, loaded: &Value) -> Result<(Value, Value), CfError> {
    let manifest = if manifest.is_null() || manifest == &json!({}) {
        json!({})
    } else {
        validate_manifest(manifest)?
    };
    let declared = manifest.get("source").cloned().unwrap_or(json!({}));
    let envelope = loaded
        .get("envelope")
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or(json!({}));
    let result = loaded
        .get("result")
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or(json!({}));
    let kind = loaded.get("kind").and_then(Value::as_str).unwrap_or("json");
    let columns = loaded
        .get("columns")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let rows = loaded
        .get("rows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut fields = Map::new();

    macro_rules! pick {
        ($name:literal, $manifest:expr, $envelope:expr) => {
            pick(&mut fields, $name, $manifest, $envelope, None, &envelope)
        };
        ($name:literal, $manifest:expr, $envelope:expr, $contract:literal) => {
            pick(
                &mut fields,
                $name,
                $manifest,
                $envelope,
                Some($contract),
                &envelope,
            )
        };
    }

    let provider = pick!(
        "provider",
        get(&declared, "provider"),
        get(&envelope, "provider")
    );
    let operation = pick!(
        "operation",
        get(&declared, "operation"),
        get(&envelope, "operation")
    );
    let reporting_identity = pick!(
        "reporting_identity",
        get(&declared, "reporting_identity"),
        get(&envelope, "reporting_identity").or_else(|| get(&result, "reporting_identity"))
    );
    let site_origin = pick!(
        "site_origin",
        validated_origin(get(&declared, "site_origin"))?
            .map(Value::String)
            .as_ref(),
        get(&envelope, "site_origin")
    );
    let search_engine = pick!(
        "search_engine",
        get(&declared, "search_engine"),
        get(&envelope, "search_engine"),
        "search_engine"
    );
    let search_type = pick!(
        "search_type",
        get(&declared, "search_type"),
        get(&envelope, "search_type"),
        "search_type"
    );
    let attribution = pick!(
        "attribution",
        get(&declared, "attribution"),
        get(&envelope, "attribution")
    );
    let timezone = pick!(
        "timezone",
        get(&declared, "timezone"),
        get(&envelope, "timezone"),
        "timezone"
    );
    let manifest_collection = manifest.get("collection").cloned().unwrap_or(json!({}));
    let collected_at = pick!(
        "collected_at",
        get(&manifest_collection, "collected_at"),
        get(&envelope, "retrieved_at")
    );
    let collection_state = pick!(
        "collection_state",
        get(&manifest_collection, "state"),
        get(&envelope, "status")
    );
    let sampled = pick!(
        "sampled",
        get(&manifest_collection, "sampled"),
        flag(get(&envelope, "sampling")).as_ref()
    );
    let thresholded = pick!(
        "thresholded",
        get(&manifest_collection, "thresholded"),
        flag(get(&envelope, "privacy_thresholds")).as_ref()
    );
    let truncated_env = get(&envelope, "pagination")
        .and_then(|p| get(p, "truncated"))
        .or_else(|| get(&result, "truncated"));
    let truncated = pick!(
        "truncated",
        get(&manifest_collection, "truncated"),
        flag(truncated_env).as_ref()
    );

    let mut period = period_from(&manifest["period"]).or_else(|| period_from(&envelope["period"]));
    if period.is_none() {
        period = period_from(&result["period"]);
    }
    fields.insert(
        "period".to_owned(),
        json!({
            "value": period,
            "origin": if period_from(&manifest["period"]).is_some() {
                "declared"
            } else if period.is_some() {
                "envelope"
            } else {
                "unknown"
            },
            "declared_by": if period_from(&manifest["period"]).is_some() {
                Value::String("manifest".into())
            } else {
                Value::Null
            },
        }),
    );
    if period_from(&manifest["period"]).is_some() {
        fields["period"]["declared_by"] = json!("manifest");
    } else {
        fields["period"]
            .as_object_mut()
            .expect("obj")
            .remove("declared_by");
    }

    let privacy = get(&declared, "privacy")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            if kind == "provider_evidence" || provider.is_some() {
                "restricted".to_owned()
            } else {
                "supplied".to_owned()
            }
        });
    fields.insert(
        "privacy".to_owned(),
        json!({
            "value": privacy,
            "origin": "declared",
            "declared_by": if get(&declared, "privacy").is_some() { "manifest" } else { "import_default" },
        }),
    );

    let mut row_shape = str_field(&manifest, "row_shape").map(str::to_owned);
    if row_shape.is_none() {
        let sample: Vec<&Value> = rows
            .iter()
            .chain(
                result
                    .get("rows")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten(),
            )
            .take(50)
            .filter(|r| r.is_object())
            .collect();
        let provider_s = provider.as_ref().and_then(Value::as_str).unwrap_or("");
        let operation_s = operation.as_ref().and_then(Value::as_str).unwrap_or("");
        row_shape = Some(
            if sample.iter().any(|r| r.get("metricValues").is_some()) {
                "ga4"
            } else if sample.iter().any(|r| r.get("keys").is_some()) {
                "gsc"
            } else if provider_s == "ga4" {
                if operation_s == "page_views" {
                    "flat"
                } else {
                    "ga4"
                }
            } else if provider_s == "gsc" {
                "gsc"
            } else {
                "flat"
            }
            .to_owned(),
        );
    }
    let row_shape = row_shape.expect("set");

    let url_spec = manifest.get("url").cloned().unwrap_or(json!({}));
    let mut url_kind = str_field(&url_spec, "kind")
        .unwrap_or("absolute")
        .to_owned();
    let mut url_field = str_field(&url_spec, "field").map(str::to_owned);
    let mut url_dimension = str_field(&url_spec, "dimension").map(str::to_owned);
    let mut dimensions: Vec<String> = manifest
        .get("dimensions")
        .and_then(Value::as_array)
        .map(|l| {
            l.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let envelope_dims: Vec<String> = result
        .get("dimensions")
        .or_else(|| envelope.get("dimensions"))
        .and_then(Value::as_array)
        .map(|l| {
            l.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    if url_field.is_none() && url_dimension.is_none() {
        if row_shape == "ga4"
            || envelope_dims
                .iter()
                .any(|d| d == "landingPagePlusQueryString")
        {
            url_dimension = Some("landingPagePlusQueryString".to_owned());
            url_kind = "relative_path".to_owned();
        } else if envelope_dims.iter().any(|d| d == "page") {
            url_dimension = Some("page".to_owned());
        } else if row_shape == "flat" && columns.iter().any(|c| c == "url") {
            url_field = Some("url".to_owned());
        } else if row_shape == "flat" {
            return Err(err(
                "cannot determine the URL field; declare mapping url.field",
            ));
        } else {
            return Err(err(
                "cannot determine the URL dimension; declare mapping url.dimension",
            ));
        }
    }
    if dimensions.is_empty() {
        dimensions = envelope_dims.clone();
    }
    fields.insert(
        "url_binding".to_owned(),
        json!({
            "value": {"field": url_field, "dimension": url_dimension, "kind": url_kind},
            "origin": if manifest.get("url").is_some() || url_field.is_some() {
                "declared"
            } else if url_dimension.as_deref().is_some_and(|d| envelope_dims.iter().any(|e| e == d)) {
                "envelope"
            } else {
                "unknown"
            },
            "declared_by": if manifest.get("url").is_some() { "manifest" } else { "import_default" },
        }),
    );

    let mut metrics: Vec<Value> = manifest
        .get("metrics")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let metrics_declared = manifest.get("metrics").is_some();
    if !metrics_declared {
        let provider_s = provider.as_ref().and_then(Value::as_str).unwrap_or("");
        let operation_s = operation.as_ref().and_then(Value::as_str).unwrap_or("");
        if provider_s == "ga4" && operation_s == "page_views" && row_shape == "flat" {
            metrics = vec![json!({"name": "screenPageViews", "type": "integer", "unit": "count"})];
        } else if row_shape == "ga4" {
            metrics = result
                .get("metrics")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(|m| {
                            m.as_str().map(
                                |name| json!({"name": name, "type": "number", "unit": "count"}),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
        } else if row_shape == "gsc" {
            let source_rows: Vec<&Value> = rows
                .iter()
                .chain(
                    result
                        .get("rows")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten(),
                )
                .take(50)
                .collect();
            let mut present = BTreeSet::new();
            for r in source_rows {
                if let Value::Object(map) = r {
                    present.extend(map.keys().filter(|k| *k != "keys").cloned());
                }
            }
            metrics = ["clicks", "impressions", "ctr", "position"]
                .iter()
                .filter(|name| present.contains(**name))
                .map(|name| {
                    json!({
                        "name": name,
                        "type": "number",
                        "unit": match *name {
                            "ctr" => "ratio",
                            "position" => "position",
                            _ => "count",
                        },
                    })
                })
                .collect();
        }
    }
    fields.insert(
        "metrics".to_owned(),
        json!({
            "value": metrics.iter().map(|m| m.get("name").cloned().unwrap_or(Value::Null)).collect::<Vec<_>>(),
            "origin": if metrics_declared { "declared" } else if metrics.is_empty() { "unknown" } else { "envelope" },
            "declared_by": if metrics_declared { "manifest" } else { "import_default" },
        }),
    );
    fields.insert(
        "dimensions".to_owned(),
        json!({
            "value": dimensions,
            "origin": if manifest.get("dimensions").is_some() {
                "declared"
            } else if dimensions.is_empty() {
                "unknown"
            } else {
                "envelope"
            },
            "declared_by": if manifest.get("dimensions").is_some() { "manifest" } else { "import_default" },
        }),
    );

    let collection = json!({
        "collected_at": collected_at,
        "state": collection_state.unwrap_or_else(|| json!("complete")),
        "sampled": sampled,
        "thresholded": thresholded,
        "truncated": truncated,
        "reason": get(&manifest_collection, "reason"),
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
        "period": period,
        "url": {"field": url_field, "dimension": url_dimension, "kind": url_kind},
        "metrics": metrics.iter().map(|m| json!({
            "name": m.get("name").cloned().unwrap_or(Value::Null),
            "type": "number",
            "unit": m.get("unit").cloned().unwrap_or(json!("count")),
        })).collect::<Vec<_>>(),
        "dimensions": dimensions,
        "collection": collection,
        "duplicate_policy": str_field(&manifest, "duplicate_policy").unwrap_or("mark"),
        "row_shape": row_shape,
    });
    Ok((resolved, json!({"fields": Value::Object(fields)})))
}

//! Pairwise compatibility checks between normalized evidence documents.
//! Each axis reports whether two documents can be joined on that dimension.
use super::*;

/// Compare two normalized documents on each compatibility axis.
pub fn evidence_compatibility(
    left: &Value,
    right: &Value,
    policy: Option<&Value>,
) -> Result<Value, CfError> {
    for document in [left, right] {
        if !document.is_object() || document.get("format") != Some(&json!(NORMALIZED_FORMAT)) {
            return Err(import_error(format!(
                "expected {NORMALIZED_FORMAT} documents"
            )));
        }
    }
    let policy_map = validate_policy(policy)?;
    let boundary = policy_map
        .get("boundary_policy")
        .filter(|v| !v.is_null())
        .and_then(Value::as_str)
        .unwrap_or("strict");
    let juxtapose = policy_map.get("cross_source") == Some(&json!("juxtapose"));
    let mut reasons: Vec<Value> = Vec::new();
    let mut juxtaposed = false;

    let lw = window(&field(left, "period")?)?;
    let rw = window(&field(right, "period")?)?;
    match (lw, rw) {
        (Some(l), Some(r)) if l == r => {
            aspect(&mut reasons, "compatible", "period_equal", "period", &[]);
        }
        (Some(l), Some(r)) => {
            let latest_start = l.0.max(r.0);
            let earliest_end = l.1.min(r.1);
            let reason = if latest_start <= earliest_end {
                "period_overlap_unequal"
            } else if l.1 + 1 == r.0 || r.1 + 1 == l.0 {
                "period_adjacent"
            } else {
                "period_disjoint"
            };
            aspect(&mut reasons, "incompatible", reason, "period", &[]);
        }
        _ => aspect(
            &mut reasons,
            "unknown",
            "period_unknown",
            "period",
            &[("unverified", json!(true))],
        ),
    }

    let lt = field(left, "timezone")?;
    let rt = field(right, "timezone")?;
    if !lt.is_null() && !rt.is_null() && lt == rt {
        aspect(
            &mut reasons,
            "compatible",
            "timezone_equal",
            "timezone",
            &[],
        );
    } else if lt.is_null() || rt.is_null() {
        if boundary == "local_calendar" {
            aspect(
                &mut reasons,
                "compatible",
                "timezone_unverified_local_calendar",
                "timezone",
                &[],
            );
        } else {
            aspect(
                &mut reasons,
                "unknown",
                "timezone_unknown",
                "timezone",
                &[("unverified", json!(true))],
            );
        }
    } else if boundary == "local_calendar" {
        aspect(
            &mut reasons,
            "compatible",
            "timezone_differs_local_calendar",
            "timezone",
            &[],
        );
    } else {
        aspect(
            &mut reasons,
            "incompatible",
            "timezone_mismatch",
            "timezone",
            &[],
        );
    }

    let lp = field(left, "provider")?;
    let rp = field(right, "provider")?;
    let li = field(left, "reporting_identity")?;
    let ri = field(right, "reporting_identity")?;
    if lp == rp && !lp.is_null() && li == ri && !li.is_null() {
        aspect(
            &mut reasons,
            "compatible",
            "same_reporting_source",
            "reporting_identity",
            &[],
        );
    } else if lp == rp && !lp.is_null() && (li.is_null() || ri.is_null()) {
        if juxtapose {
            aspect(
                &mut reasons,
                "compatible",
                "identity_unverified_juxtaposed",
                "reporting_identity",
                &[("unverified", json!(true))],
            );
            juxtaposed = true;
        } else {
            aspect(
                &mut reasons,
                "unknown",
                "reporting_identity_unknown",
                "reporting_identity",
                &[("unverified", json!(true))],
            );
        }
    } else {
        // Covers both-null leftovers where the providers match but the identities differ.
        if lp != rp || li != ri {
            if juxtapose {
                aspect(
                    &mut reasons,
                    "compatible",
                    "juxtaposed_sources",
                    "reporting_identity",
                    &[],
                );
                juxtaposed = true;
            } else {
                aspect(
                    &mut reasons,
                    "incompatible",
                    "reporting_source_mismatch",
                    "reporting_identity",
                    &[],
                );
            }
        }
    }

    let la = field(left, "attribution")?;
    let ra = field(right, "attribution")?;
    if !la.is_null() && !ra.is_null() {
        if la == ra {
            aspect(
                &mut reasons,
                "compatible",
                "attribution_equal",
                "attribution",
                &[],
            );
        } else if juxtapose {
            aspect(
                &mut reasons,
                "compatible",
                "attribution_labeled",
                "attribution",
                &[],
            );
            juxtaposed = true;
        } else {
            aspect(
                &mut reasons,
                "incompatible",
                "attribution_mismatch",
                "attribution",
                &[],
            );
        }
    } else if juxtapose {
        aspect(
            &mut reasons,
            "compatible",
            "attribution_unverified_juxtaposed",
            "attribution",
            &[("unverified", json!(true))],
        );
        juxtaposed = true;
    } else {
        aspect(
            &mut reasons,
            "unknown",
            "attribution_unknown",
            "attribution",
            &[("unverified", json!(true))],
        );
    }

    for (aspect_name, field_name) in [
        ("search_engine", "search_engine"),
        ("search_type", "search_type"),
    ] {
        let lv = field(left, field_name)?;
        let rv = field(right, field_name)?;
        if !lv.is_null() && !rv.is_null() {
            if lv == rv {
                aspect(
                    &mut reasons,
                    "compatible",
                    &format!("{field_name}_equal"),
                    aspect_name,
                    &[],
                );
            } else if juxtapose {
                aspect(
                    &mut reasons,
                    "compatible",
                    &format!("{field_name}_labeled"),
                    aspect_name,
                    &[],
                );
                juxtaposed = true;
            } else {
                aspect(
                    &mut reasons,
                    "incompatible",
                    &format!("{field_name}_mismatch"),
                    aspect_name,
                    &[],
                );
            }
        } else if lv.is_null() && rv.is_null() {
            aspect(
                &mut reasons,
                "compatible",
                &format!("{field_name}_not_declared"),
                aspect_name,
                &[],
            );
        } else if juxtapose {
            aspect(
                &mut reasons,
                "compatible",
                &format!("{field_name}_differs_labeled"),
                aspect_name,
                &[],
            );
            juxtaposed = true;
        } else {
            aspect(
                &mut reasons,
                "unknown",
                &format!("{field_name}_unknown"),
                aspect_name,
                &[("unverified", json!(true))],
            );
        }
    }

    let ldims: Vec<String> = mapping_of(left)
        .get("dimensions")
        .and_then(Value::as_array)
        .map(|a| {
            let mut v: Vec<String> = a
                .iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect();
            v.sort();
            v.dedup();
            v
        })
        .unwrap_or_default();
    let rdims: Vec<String> = mapping_of(right)
        .get("dimensions")
        .and_then(Value::as_array)
        .map(|a| {
            let mut v: Vec<String> = a
                .iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect();
            v.sort();
            v.dedup();
            v
        })
        .unwrap_or_default();
    if ldims == rdims {
        aspect(&mut reasons, "compatible", "grain_equal", "grain", &[]);
    } else if juxtapose {
        aspect(
            &mut reasons,
            "compatible",
            "grain_differs_labeled",
            "grain",
            &[],
        );
        juxtaposed = true;
    } else {
        aspect(&mut reasons, "incompatible", "grain_mismatch", "grain", &[]);
    }

    aspect(
        &mut reasons,
        "compatible",
        "metrics_remain_distinct_per_source",
        "metrics",
        &[],
    );

    let mut degraded: Vec<String> = Vec::new();
    for (side, document) in [("left", left), ("right", right)] {
        let collection = mapping_of(document)
            .get("collection")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let state = collection.get("state").and_then(Value::as_str);
        let flags: Vec<&str> = ["sampled", "thresholded", "truncated"]
            .iter()
            .filter(|name| {
                collection
                    .get(**name)
                    .map(|v| pystr::truthy(Some(v)))
                    .unwrap_or(false)
            })
            .copied()
            .collect();
        if matches!(state, Some("failed" | "skipped" | "not_configured")) {
            aspect(
                &mut reasons,
                "unknown",
                &format!("collection_{}", state.unwrap_or_default()),
                "collection",
                &[("side", json!(side)), ("unverified", json!(true))],
            );
        } else if state == Some("partial") || !flags.is_empty() {
            let mut parts: Vec<String> = Vec::new();
            if state == Some("partial") {
                parts.push("partial".to_owned());
            }
            parts.extend(flags.iter().map(|f| f.to_string()));
            degraded.push(format!("{}:{}", side, parts.join(",")));
        }
    }
    if !degraded.is_empty() {
        aspect(
            &mut reasons,
            "compatible",
            "collection_coverage_degraded",
            "collection",
            &[("degraded", json!(degraded))],
        );
    }

    let verdict = if reasons
        .iter()
        .any(|r| r.get("verdict") == Some(&json!("incompatible")))
    {
        "incompatible"
    } else if reasons
        .iter()
        .any(|r| r.get("verdict") == Some(&json!("unknown")))
    {
        "unknown"
    } else {
        "compatible"
    };

    let axis = |document: &Value| -> Result<Value, CfError> {
        let mapping = mapping_of(document);
        let source = mapping
            .get("source")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut metrics = Vec::new();
        for metric in mapping
            .get("metrics")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            metrics.push(crate::cloudflare::py_subscript(&metric, "name")?.clone());
        }
        Ok(json!({
            "provider": source.get("provider").cloned().unwrap_or(Value::Null),
            "operation": source.get("operation").cloned().unwrap_or(Value::Null),
            "reporting_identity": source.get("reporting_identity").cloned().unwrap_or(Value::Null),
            "metrics": metrics,
            "dimensions": mapping.get("dimensions").cloned().unwrap_or(json!([])),
            "period": mapping.get("period").cloned().unwrap_or(Value::Null),
            "timezone": source.get("timezone").cloned().unwrap_or(Value::Null),
            "attribution": source.get("attribution").cloned().unwrap_or(Value::Null),
            "search_engine": source.get("search_engine").cloned().unwrap_or(Value::Null),
            "search_type": source.get("search_type").cloned().unwrap_or(Value::Null),
        }))
    };
    let mut result = json!({
        "format": COMPATIBILITY_FORMAT,
        "verdict": verdict,
        "juxtaposed": juxtaposed,
        "boundary_policy": if policy_map.is_empty() { Value::Null } else { json!(boundary) },
        "reasons": reasons,
        "axes": {"left": axis(left)?, "right": axis(right)?},
    });

    let quadrant_req = policy_map.get("quadrant").filter(|v| !v.is_null());
    let mut quadrant =
        json!({"requested": quadrant_req.is_some(), "eligible": false, "reasons": []});
    if let Some(req) = quadrant_req {
        let mut blockers: Vec<String> = Vec::new();
        if policy_map.get("boundary_policy").is_none_or(Value::is_null) {
            blockers.push("boundary_policy_undeclared".to_owned());
        }
        if verdict != "compatible" {
            blockers.push(format!("sources_{verdict}"));
        }
        if reasons
            .iter()
            .any(|r| r.get("unverified") == Some(&json!(true)))
        {
            blockers.push("unverified_scope".to_owned());
        }
        let metric_names = |document: &Value| -> Vec<String> {
            mapping_of(document)
                .get("metrics")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|m| m.get("name").and_then(Value::as_str).map(str::to_owned))
                .collect()
        };
        let left_metrics = metric_names(left);
        let right_metrics = metric_names(right);
        let lm = req["left_metric"].as_str().unwrap_or_default();
        let rm = req["right_metric"].as_str().unwrap_or_default();
        if !left_metrics.contains(&lm.to_owned()) {
            blockers.push("left_metric_undeclared".to_owned());
        }
        if !right_metrics.contains(&rm.to_owned()) {
            blockers.push("right_metric_undeclared".to_owned());
        }
        let left_keys = keyed_urls(left);
        let right_keys = keyed_urls(right);
        let left_measured = if left_metrics.contains(&lm.to_owned()) {
            measured_keys(left, lm)
        } else {
            Vec::new()
        };
        let right_measured = if right_metrics.contains(&rm.to_owned()) {
            measured_keys(right, rm)
        } else {
            Vec::new()
        };
        let shared: Vec<&String> = left_keys
            .iter()
            .filter(|k| right_keys.contains(k))
            .collect();
        let eligible: Vec<&String> = left_measured
            .iter()
            .filter(|k| right_measured.contains(k))
            .collect();
        let left_zero = zero_measured_keys(left, lm);
        let right_zero = zero_measured_keys(right, rm);
        let mut sorted_degraded = degraded.clone();
        sorted_degraded.sort();
        quadrant = json!({
            "requested": true,
            "eligible": blockers.is_empty(),
            "reasons": blockers,
            "eligible_key_count": if blockers.is_empty() { eligible.len() } else { 0 },
            "shared_key_count": shared.len(),
            "left_only_key_count": left_keys.iter().filter(|k| !right_keys.contains(k)).count(),
            "right_only_key_count": right_keys.iter().filter(|k| !left_keys.contains(k)).count(),
            "excluded": {
                "left_unmeasured": shared.iter().filter(|k| !left_measured.contains(*k)).count(),
                "right_unmeasured": shared.iter().filter(|k| !right_measured.contains(*k)).count(),
            },
            "both_measured_zero_keys": if blockers.is_empty() {
                eligible
                    .iter()
                    .filter(|k| left_zero.contains(*k) && right_zero.contains(*k))
                    .count()
            } else {
                0
            },
            "degraded": if sorted_degraded.is_empty() { json!([]) } else { json!(sorted_degraded) },
        });
    }
    result["quadrant"] = quadrant;
    Ok(result)
}

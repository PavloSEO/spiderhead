//! Provider and join evidence compatibility: whether two evidence documents can be compared.
//! Only the quadrant-candidate decision path consumes it.

use std::collections::BTreeSet;

use serde_json::{json, Map, Value};

use crate::bi::dates::window;
use crate::bi::schema::BIExportError;
use crate::py;

fn err(msg: impl Into<String>) -> BIExportError {
    BIExportError::new(msg)
}

/// Normalized evidence format tag.
pub const NORMALIZED_FORMAT: &str = "spiderhead.normalized-evidence.v1";

/// Compatibility format tag.
pub const COMPATIBILITY_FORMAT: &str = "spiderhead.evidence-compatibility.v1";

const BOUNDARY_POLICIES: [&str; 2] = ["strict", "local_calendar"];
const DEGRADED_STATES: [&str; 1] = ["partial"];
const UNUSABLE_STATES: [&str; 3] = ["failed", "skipped", "not_configured"];
const POLICY_KEYS: [&str; 3] = ["boundary_policy", "cross_source", "quadrant"];
const PRIVATE_AXES: [&str; 2] = ["reporting_identity", "site_origin"];

/// One provenance-recorded header field value.
fn field(document: &Value, name: &str) -> Value {
    document
        .get("provenance")
        .and_then(|p| p.get("fields"))
        .and_then(|f| f.get(name))
        .and_then(|e| e.get("value"))
        .cloned()
        .unwrap_or(Value::Null)
}

/// Append `{aspect, verdict, reason, ...extra}` to `reasons`.
fn aspect(
    reasons: &mut Vec<Value>,
    verdict: &str,
    reason: &str,
    name: &str,
    extra: Map<String, Value>,
) {
    let mut row = Map::new();
    row.insert("aspect".into(), json!(name));
    row.insert("verdict".into(), json!(verdict));
    row.insert("reason".into(), json!(reason));
    row.extend(extra);
    reasons.push(Value::Object(row));
}

fn unverified() -> Map<String, Value> {
    let mut extra = Map::new();
    extra.insert("unverified".into(), json!(true));
    extra
}

/// Period aspect: true only for equal windows.
fn period_aspect(left: &Value, right: &Value, reasons: &mut Vec<Value>) -> bool {
    let lw = window(Some(&field(left, "period")));
    let rw = window(Some(&field(right, "period")));
    let (Some(lw), Some(rw)) = (lw, rw) else {
        aspect(reasons, "unknown", "period_unknown", "period", unverified());
        return false;
    };
    if lw == rw {
        aspect(reasons, "compatible", "period_equal", "period", Map::new());
        return true;
    }
    let latest_start = std::cmp::max(lw.0, rw.0);
    let earliest_end = std::cmp::min(lw.1, rw.1);
    if latest_start <= earliest_end {
        aspect(
            reasons,
            "incompatible",
            "period_overlap_unequal",
            "period",
            Map::new(),
        );
    } else if lw.1.next_day() == rw.0 || rw.1.next_day() == lw.0 {
        aspect(
            reasons,
            "incompatible",
            "period_adjacent",
            "period",
            Map::new(),
        );
    } else {
        aspect(
            reasons,
            "incompatible",
            "period_disjoint",
            "period",
            Map::new(),
        );
    }
    false
}

/// URL keys with at least one measured `metric` row.
fn measured_keys(document: &Value, metric: &str) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for row in py::get_list(document, "rows") {
        let url = py::get_map(row, "url");
        if url.get("state").and_then(Value::as_str) != Some("keyed") {
            continue;
        }
        let entry = py::get_map(row, "metrics");
        let entry = entry.get(metric).cloned().unwrap_or(Value::Null);
        if entry.get("state").and_then(Value::as_str) == Some("measured") {
            if let Some(key) = url.get("normalized").and_then(Value::as_str) {
                keys.insert(key.to_owned());
            }
        }
    }
    keys
}

/// Index URLs by key.
fn keyed_urls(document: &Value) -> BTreeSet<String> {
    py::get_list(document, "rows")
        .iter()
        .map(|row| py::get_map(row, "url"))
        .filter(|url| url.get("state").and_then(Value::as_str) == Some("keyed"))
        .filter_map(|url| url.get("normalized").and_then(Value::as_str))
        .map(str::to_owned)
        .collect()
}

/// Measured keys whose value is exactly `0`.
fn zero_measured_keys(document: &Value, metric: &str) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for row in py::get_list(document, "rows") {
        let url = py::get_map(row, "url");
        if url.get("state").and_then(Value::as_str) != Some("keyed") {
            continue;
        }
        let entry = py::get_map(row, "metrics");
        let entry = entry.get(metric).cloned().unwrap_or(Value::Null);
        if entry.get("state").and_then(Value::as_str) == Some("measured")
            && entry.get("value") == Some(&json!(0))
        {
            if let Some(key) = url.get("normalized").and_then(Value::as_str) {
                keys.insert(key.to_owned());
            }
        }
    }
    keys
}

/// Validate the policy document.
fn validate_policy(policy: &Value) -> Result<Value, BIExportError> {
    if policy.is_null() {
        return Ok(json!({}));
    }
    let Some(map) = policy.as_object() else {
        return Err(err(
            "comparison policy may name only [boundary_policy, cross_source, quadrant]",
        ));
    };
    if map.keys().any(|k| !POLICY_KEYS.contains(&k.as_str())) {
        return Err(err(
            "comparison policy may name only [boundary_policy, cross_source, quadrant]",
        ));
    }
    if let Some(boundary) = policy.get("boundary_policy") {
        if !boundary.is_null() && !BOUNDARY_POLICIES.contains(&boundary.as_str().unwrap_or("")) {
            return Err(err(
                "boundary_policy must be one of [strict, local_calendar]",
            ));
        }
    }
    let cross = policy.get("cross_source");
    if cross.is_some_and(|c| !c.is_null()) && cross.and_then(Value::as_str) != Some("juxtapose") {
        return Err(err("cross_source must be \"juxtapose\" when declared"));
    }
    if let Some(quadrant) = policy.get("quadrant") {
        if quadrant.is_null() {
            return Ok(policy.clone());
        }
        let valid = quadrant.as_object().is_some_and(|q| {
            q.keys().all(|k| k == "left_metric" || k == "right_metric")
                && ["left_metric", "right_metric"].iter().all(|name| {
                    quadrant
                        .get(*name)
                        .and_then(Value::as_str)
                        .is_some_and(|s| !s.is_empty())
                })
        });
        if quadrant
            .as_object()
            .is_none_or(|q| q.keys().any(|k| k != "left_metric" && k != "right_metric"))
        {
            return Err(err(
                "policy quadrant may name only left_metric and right_metric",
            ));
        }
        if !valid {
            for name in ["left_metric", "right_metric"] {
                if !quadrant
                    .get(name)
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty())
                {
                    return Err(err(format!("policy quadrant requires a {name} string")));
                }
            }
        }
    }
    Ok(policy.clone())
}

/// Decide whether two normalized evidence
/// documents may be compared.
pub fn evidence_compatibility(
    left: &Value,
    right: &Value,
    policy: &Value,
) -> Result<Value, BIExportError> {
    for document in [left, right] {
        if !document.is_object()
            || document.get("format").and_then(Value::as_str) != Some(NORMALIZED_FORMAT)
        {
            return Err(err(format!("expected {NORMALIZED_FORMAT} documents")));
        }
    }
    let policy = validate_policy(policy)?;
    let boundary = policy
        .get("boundary_policy")
        .and_then(Value::as_str)
        .unwrap_or("strict");
    let juxtapose = policy.get("cross_source").and_then(Value::as_str) == Some("juxtapose");
    let mut reasons: Vec<Value> = Vec::new();
    let mut juxtaposed = false;

    period_aspect(left, right, &mut reasons);

    let (lt, rt) = (field(left, "timezone"), field(right, "timezone"));
    if !lt.is_null() && lt == rt {
        aspect(
            &mut reasons,
            "compatible",
            "timezone_equal",
            "timezone",
            Map::new(),
        );
    } else if lt.is_null() || rt.is_null() {
        if boundary == "local_calendar" {
            aspect(
                &mut reasons,
                "compatible",
                "timezone_unverified_local_calendar",
                "timezone",
                Map::new(),
            );
        } else {
            aspect(
                &mut reasons,
                "unknown",
                "timezone_unknown",
                "timezone",
                unverified(),
            );
        }
    } else if boundary == "local_calendar" {
        aspect(
            &mut reasons,
            "compatible",
            "timezone_differs_local_calendar",
            "timezone",
            Map::new(),
        );
    } else {
        aspect(
            &mut reasons,
            "incompatible",
            "timezone_mismatch",
            "timezone",
            Map::new(),
        );
    }

    let (lp, rp) = (field(left, "provider"), field(right, "provider"));
    let (li, ri) = (
        field(left, "reporting_identity"),
        field(right, "reporting_identity"),
    );
    if lp == rp && !lp.is_null() && li == ri && !li.is_null() {
        aspect(
            &mut reasons,
            "compatible",
            "same_reporting_source",
            "reporting_identity",
            Map::new(),
        );
    } else if lp == rp && !lp.is_null() && (li.is_null() || ri.is_null()) {
        if juxtapose {
            aspect(
                &mut reasons,
                "compatible",
                "identity_unverified_juxtaposed",
                "reporting_identity",
                unverified(),
            );
            juxtaposed = true;
        } else {
            aspect(
                &mut reasons,
                "unknown",
                "reporting_identity_unknown",
                "reporting_identity",
                unverified(),
            );
        }
    } else if lp != rp || li != ri {
        if juxtapose {
            aspect(
                &mut reasons,
                "compatible",
                "juxtaposed_sources",
                "reporting_identity",
                Map::new(),
            );
            juxtaposed = true;
        } else {
            aspect(
                &mut reasons,
                "incompatible",
                "reporting_source_mismatch",
                "reporting_identity",
                Map::new(),
            );
        }
    }

    let (la, ra) = (field(left, "attribution"), field(right, "attribution"));
    if !la.is_null() && !ra.is_null() {
        if la == ra {
            aspect(
                &mut reasons,
                "compatible",
                "attribution_equal",
                "attribution",
                Map::new(),
            );
        } else if juxtapose {
            aspect(
                &mut reasons,
                "compatible",
                "attribution_labeled",
                "attribution",
                Map::new(),
            );
            juxtaposed = true;
        } else {
            aspect(
                &mut reasons,
                "incompatible",
                "attribution_mismatch",
                "attribution",
                Map::new(),
            );
        }
    } else if juxtapose {
        aspect(
            &mut reasons,
            "compatible",
            "attribution_unverified_juxtaposed",
            "attribution",
            unverified(),
        );
        juxtaposed = true;
    } else {
        aspect(
            &mut reasons,
            "unknown",
            "attribution_unknown",
            "attribution",
            unverified(),
        );
    }

    for (name, field_name) in [
        ("search_engine", "search_engine"),
        ("search_type", "search_type"),
    ] {
        let (lv, rv) = (field(left, field_name), field(right, field_name));
        if !lv.is_null() && !rv.is_null() {
            if lv == rv {
                aspect(
                    &mut reasons,
                    "compatible",
                    &format!("{field_name}_equal"),
                    name,
                    Map::new(),
                );
            } else if juxtapose {
                aspect(
                    &mut reasons,
                    "compatible",
                    &format!("{field_name}_labeled"),
                    name,
                    Map::new(),
                );
                juxtaposed = true;
            } else {
                aspect(
                    &mut reasons,
                    "incompatible",
                    &format!("{field_name}_mismatch"),
                    name,
                    Map::new(),
                );
            }
        } else if lv.is_null() && rv.is_null() {
            aspect(
                &mut reasons,
                "compatible",
                &format!("{field_name}_not_declared"),
                name,
                Map::new(),
            );
        } else if juxtapose {
            aspect(
                &mut reasons,
                "compatible",
                &format!("{field_name}_differs_labeled"),
                name,
                Map::new(),
            );
            juxtaposed = true;
        } else {
            aspect(
                &mut reasons,
                "unknown",
                &format!("{field_name}_unknown"),
                name,
                unverified(),
            );
        }
    }

    let dims = |document: &Value| -> BTreeSet<String> {
        py::get_map(document, "mapping")
            .get("dimensions")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    let (ldims, rdims) = (dims(left), dims(right));
    if ldims == rdims {
        aspect(
            &mut reasons,
            "compatible",
            "grain_equal",
            "grain",
            Map::new(),
        );
    } else if juxtapose {
        aspect(
            &mut reasons,
            "compatible",
            "grain_differs_labeled",
            "grain",
            Map::new(),
        );
        juxtaposed = true;
    } else {
        aspect(
            &mut reasons,
            "incompatible",
            "grain_mismatch",
            "grain",
            Map::new(),
        );
    }

    aspect(
        &mut reasons,
        "compatible",
        "metrics_remain_distinct_per_source",
        "metrics",
        Map::new(),
    );

    let mut degraded: Vec<String> = Vec::new();
    for (side, document) in [("left", left), ("right", right)] {
        let collection_map = py::get_map(document, "mapping")
            .get("collection")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let state = collection_map.get("state").and_then(Value::as_str);
        let flags: Vec<&str> = ["sampled", "thresholded", "truncated"]
            .iter()
            .copied()
            .filter(|name| py::truthy(collection_map.get(*name)))
            .collect();
        if UNUSABLE_STATES.contains(&state.unwrap_or("")) {
            let mut extra = unverified();
            extra.insert("side".into(), json!(side));
            aspect(
                &mut reasons,
                "unknown",
                &format!("collection_{}", state.unwrap_or("")),
                "collection",
                extra,
            );
        } else if DEGRADED_STATES.contains(&state.unwrap_or("")) || !flags.is_empty() {
            let mut parts: Vec<String> = Vec::new();
            if DEGRADED_STATES.contains(&state.unwrap_or("")) {
                parts.push(state.unwrap_or("").to_owned());
            }
            parts.extend(flags.iter().map(|f| f.to_string()));
            degraded.push(format!("{side}:{}", parts.join(",")));
        }
    }
    if !degraded.is_empty() {
        let mut extra = Map::new();
        extra.insert("degraded".into(), json!(degraded));
        aspect(
            &mut reasons,
            "compatible",
            "collection_coverage_degraded",
            "collection",
            extra,
        );
    }

    let verdict = if reasons
        .iter()
        .any(|r| r.get("verdict").and_then(Value::as_str) == Some("incompatible"))
    {
        "incompatible"
    } else if reasons
        .iter()
        .any(|r| r.get("verdict").and_then(Value::as_str) == Some("unknown"))
    {
        "unknown"
    } else {
        "compatible"
    };

    let axis = |document: &Value| -> Value {
        let mapping = py::get_map(document, "mapping");
        let source = mapping.get("source").cloned().unwrap_or_else(|| json!({}));
        let metrics: Vec<Value> = mapping
            .get("metrics")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|m| m.get("name").cloned()).collect())
            .unwrap_or_default();
        json!({
            "provider": source.get("provider"),
            "operation": source.get("operation"),
            "reporting_identity": source.get("reporting_identity"),
            "metrics": metrics,
            "dimensions": mapping.get("dimensions").cloned().unwrap_or_else(|| json!([])),
            "period": mapping.get("period"),
            "timezone": source.get("timezone"),
            "attribution": source.get("attribution"),
            "search_engine": source.get("search_engine"),
            "search_type": source.get("search_type"),
        })
    };

    let mut result = json!({
        "format": COMPATIBILITY_FORMAT,
        "verdict": verdict,
        "juxtaposed": juxtaposed,
        "boundary_policy": if py::truthy(Some(&policy)) { json!(boundary) } else { Value::Null },
        "reasons": reasons.clone(),
        "axes": {"left": axis(left), "right": axis(right)},
    });

    let quadrant_req = policy.get("quadrant").cloned().unwrap_or(Value::Null);
    let mut quadrant = json!({
        "requested": py::truthy(Some(&quadrant_req)),
        "eligible": false,
        "reasons": [],
    });
    if py::truthy(Some(&quadrant_req)) {
        let mut blockers: Vec<String> = Vec::new();
        if policy.get("boundary_policy").is_none() {
            blockers.push("boundary_policy_undeclared".into());
        }
        if verdict != "compatible" {
            blockers.push(format!("sources_{verdict}"));
        }
        if reasons.iter().any(|r| py::truthy(r.get("unverified"))) {
            blockers.push("unverified_scope".into());
        }
        let metric_names = |document: &Value| -> BTreeSet<String> {
            py::get_map(document, "mapping")
                .get("metrics")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|m| m.get("name").and_then(Value::as_str))
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        let left_metrics = metric_names(left);
        let right_metrics = metric_names(right);
        let lm = quadrant_req
            .get("left_metric")
            .and_then(Value::as_str)
            .unwrap_or("");
        let rm = quadrant_req
            .get("right_metric")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !left_metrics.contains(lm) {
            blockers.push("left_metric_undeclared".into());
        }
        if !right_metrics.contains(rm) {
            blockers.push("right_metric_undeclared".into());
        }
        let left_keys = keyed_urls(left);
        let right_keys = keyed_urls(right);
        let left_measured = if left_metrics.contains(lm) {
            measured_keys(left, lm)
        } else {
            BTreeSet::new()
        };
        let right_measured = if right_metrics.contains(rm) {
            measured_keys(right, rm)
        } else {
            BTreeSet::new()
        };
        let shared: BTreeSet<String> = left_keys.intersection(&right_keys).cloned().collect();
        let eligible_keys: BTreeSet<String> = left_measured
            .intersection(&right_measured)
            .cloned()
            .collect();
        let both_zero = if blockers.is_empty() {
            let zl = zero_measured_keys(left, lm);
            let zr = zero_measured_keys(right, rm);
            eligible_keys
                .intersection(&zl)
                .cloned()
                .collect::<BTreeSet<_>>()
                .intersection(&zr)
                .count()
        } else {
            0
        };
        let mut sorted_degraded = degraded.clone();
        sorted_degraded.sort();
        quadrant = json!({
            "requested": true,
            "eligible": blockers.is_empty(),
            "reasons": blockers.iter().map(|b| json!(b)).collect::<Vec<_>>(),
            "eligible_key_count": if blockers.is_empty() { eligible_keys.len() } else { 0 },
            "shared_key_count": shared.len(),
            "left_only_key_count": left_keys.difference(&right_keys).count(),
            "right_only_key_count": right_keys.difference(&left_keys).count(),
            "excluded": {
                "left_unmeasured": shared.difference(&left_measured).count(),
                "right_unmeasured": shared.difference(&right_measured).count(),
            },
            "both_measured_zero_keys": both_zero,
            "degraded": sorted_degraded,
        });
    }
    result["quadrant"] = quadrant;
    Ok(result)
}

/// Public compatibility view: client-identifying axis values are redacted.
pub fn public_compatibility(result: &Value) -> Value {
    let mut out = result.clone();
    let mut axes = Map::new();
    for (side, axis) in py::get_map(result, "axes") {
        let redacted: Map<String, Value> = axis
            .as_object()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|(name, value)| {
                let value = if PRIVATE_AXES.contains(&name.as_str()) && !value.is_null() {
                    json!("redacted")
                } else {
                    value
                };
                (name, value)
            })
            .collect();
        axes.insert(side.clone(), Value::Object(redacted));
    }
    out["axes"] = Value::Object(axes);
    out
}

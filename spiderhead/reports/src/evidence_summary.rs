//! Saved capability and population metadata rows for human report writers.
//!
//! Formats only what the audit already recorded; never computes a measurement.

use serde_json::Value;

use crate::client_findings::check_title;
use crate::py;

/// The `[kind, measurement, state, scope-or-reason]` rows.
pub fn rows(summary: &Value) -> Vec<Vec<String>> {
    let mut output: Vec<Vec<String>> = Vec::new();
    if let Some(field_cwv) = py::get(summary, "field_cwv").filter(|v| v.is_object()) {
        let assessments = py::get_list(field_cwv, "assessments");
        if let Some(sampling) = py::get(field_cwv, "sampling").filter(|v| v.is_object()) {
            output.push(vec![
                "CrUX field CWV".into(),
                "Bounded URL sample".into(),
                py::get(field_cwv, "state")
                    .map(py::str_value)
                    .unwrap_or_else(|| "unavailable".into()),
                [
                    "requested",
                    "sampled",
                    "omitted",
                    "requests",
                    "cache_hits",
                    "duplicate_record_targets",
                    "cache_max_age_hours",
                ]
                .iter()
                .map(|key| {
                    format!(
                        "{key}: {}",
                        py::str_value(sampling.get(key).unwrap_or(&Value::Null))
                    )
                })
                .collect::<Vec<_>>()
                .join("; "),
            ]);
        }
        if assessments.is_empty() {
            output.push(vec![
                "CrUX field CWV".into(),
                "LCP / INP / CLS".into(),
                py::get(field_cwv, "state")
                    .map(py::str_value)
                    .unwrap_or_else(|| "unavailable".into()),
                "no supplied field record".into(),
            ]);
        }
        for assessment in assessments {
            let period = py::get_map(assessment, "collection_period");
            let scope = format!(
                "{}: {}; {}; {}..{}; retrieved {}; {}; {}, {}",
                py::str_value(assessment.get("target_kind").unwrap_or(&Value::Null)),
                py::str_value(assessment.get("target").unwrap_or(&Value::Null)),
                py::str_value(assessment.get("form_factor").unwrap_or(&Value::Null)),
                py::str_value(period.get("first_date").unwrap_or(&Value::Null)),
                py::str_value(period.get("last_date").unwrap_or(&Value::Null)),
                py::str_value(assessment.get("retrieved_at").unwrap_or(&Value::Null)),
                py::str_value(assessment.get("policy").unwrap_or(&Value::Null)),
                py::str_value(assessment.get("provider_access").unwrap_or(&Value::Null)),
                py::str_value(assessment.get("cost_mode").unwrap_or(&Value::Null)),
            ) + &format!(
                ", {}",
                py::str_value(assessment.get("quota_mode").unwrap_or(&Value::Null))
            );
            if let Some(metrics) = py::get(assessment, "metrics").and_then(Value::as_object) {
                for (name, metric) in metrics {
                    let measurement = metric
                        .get("label")
                        .map(py::str_value)
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| name.clone())
                        + &if metric.get("p75").is_some_and(|v| !v.is_null()) {
                            format!(
                                " p75 {} {}",
                                py::str_value(metric.get("p75").unwrap()),
                                py::str_value(metric.get("unit").unwrap_or(&Value::Null))
                            )
                        } else {
                            String::new()
                        };
                    output.push(vec![
                        "CrUX field CWV".into(),
                        measurement,
                        metric
                            .get("state")
                            .map(py::str_value)
                            .unwrap_or_else(|| "unavailable".into()),
                        scope.clone()
                            + &metric
                                .get("reason")
                                .map(py::str_value)
                                .filter(|s| !s.is_empty())
                                .map(|r| format!("; {r}"))
                                .unwrap_or_default(),
                    ]);
                }
            }
        }
    }
    let Some(contract) = py::get(summary, "evidence_contract").filter(|v| v.is_object()) else {
        return output;
    };
    output.push(vec![
        "Run".into(),
        "Saved scan identity".into(),
        contract
            .get("scan_identity_state")
            .map(py::str_value)
            .unwrap_or_else(|| "unavailable".into()),
        contract
            .get("scan_uuid")
            .or_else(|| contract.get("scan_identity_reason"))
            .map(py::str_value)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "not retained".into()),
    ]);
    if let Some(population) = py::get(contract, "population").filter(|v| v.is_object()) {
        output.push(vec![
            "Run".into(),
            "Measured population".into(),
            if py::truthy(population.get("crawl_partial")) {
                "partial".into()
            } else {
                "recorded".into()
            },
            format!(
                "URLs: {}; Scope: {}",
                population
                    .get("urls_crawled")
                    .map(py::str_value)
                    .unwrap_or_else(|| "unknown".into()),
                population
                    .get("scope_reason")
                    .map(py::str_value)
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "saved crawl population".into())
            ),
        ]);
    }
    for row in py::get_list(contract, "capability_rows") {
        if !row.is_object() {
            continue;
        }
        // An empty reason falls through to capability, so it must not hide it.
        let mut reason = row
            .get("reason")
            .map(py::str_value)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                row.get("capability")
                    .map(py::str_value)
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or_else(|| "not recorded".into());
        if let Some(prereq) = row.get("prerequisites").filter(|v| v.is_object()) {
            let evidence_state = prereq
                .get("required_evidence")
                .and_then(|v| v.get("state"))
                .map(py::str_value)
                .unwrap_or_else(|| "unknown".into());
            let population = prereq
                .get("population")
                .and_then(|v| v.get("value"))
                .map(py::str_value)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "not declared".into());
            reason +=
                &format!("; prerequisite metadata: {evidence_state}; population: {population}");
        }
        output.push(vec![
            "Check".into(),
            check_title(row.get("check").unwrap_or(&Value::Null)),
            row.get("state")
                .map(py::str_value)
                .unwrap_or_else(|| "unmeasured".into()),
            reason,
        ]);
    }
    output
}

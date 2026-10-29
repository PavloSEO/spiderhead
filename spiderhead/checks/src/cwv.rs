//! Versioned interpretation of CrUX current-window field Core Web Vitals.
//!
//! One retained CrUX record in, one `spiderhead.field-cwv/1` assessment out.
//! Missing data never becomes a passing metric: every unavailable metric carries
//! an explicit `reason`, and the overall state degrades accordingly.

use serde_json::{json, Map, Value as Json};

/// The threshold policy this assessment interprets.
pub const POLICY: &str = "web-vitals-2024-03";
pub const POLICY_URL: &str = "https://web.dev/articles/defining-core-web-vitals-thresholds";

/// Official p75 thresholds: (metric, good ceiling, poor ceiling, unit).
const THRESHOLDS: [(&str, f64, f64, &str); 3] = [
    ("largest_contentful_paint", 2500.0, 4000.0, "ms"),
    ("interaction_to_next_paint", 200.0, 500.0, "ms"),
    ("cumulative_layout_shift", 0.1, 0.25, "score"),
];

/// The short name reports use for each metric.
fn label_of(name: &str) -> &'static str {
    match name {
        "largest_contentful_paint" => "LCP",
        "interaction_to_next_paint" => "INP",
        _ => "CLS",
    }
}

/// Truthiness for the `or`-chain fallbacks used when reading records.
pub(crate) fn truthy(value: &Json) -> bool {
    match value {
        Json::Null => false,
        Json::Bool(b) => *b,
        Json::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Json::String(s) => !s.is_empty(),
        Json::Array(a) => !a.is_empty(),
        Json::Object(o) => !o.is_empty(),
    }
}

/// `datetime.date(...).isoformat()`: the three parts must be non-bool integers
/// naming a real calendar day (`2026-02-30` names none and yields `None`).
fn iso_date(value: &Json) -> Option<String> {
    let object = value.as_object()?;
    let mut parts = [0i64; 3];
    for (slot, name) in parts.iter_mut().zip(["year", "month", "day"]) {
        let part = object.get(name)?;
        if part.is_boolean() || !part.is_i64() {
            return None;
        }
        *slot = part.as_i64()?;
    }
    let [year, month, day] = parts;
    if !(1..=9999).contains(&year) || !(1..=12).contains(&month) {
        return None;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let last_day = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if !(1..=last_day).contains(&day) {
        return None;
    }
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

/// Float parsing plus the finite, non-negative gate: `nan`, `inf`,
/// negatives and non-numeric text all mean "no usable p75".
fn p75_value(value: &Json) -> Option<f64> {
    if value.is_boolean() || value.is_null() || value.is_object() || value.is_array() {
        return None;
    }
    let number = match value {
        Json::Number(n) => n.as_f64()?,
        Json::String(s) => {
            if s.trim().is_empty() {
                return None;
            }
            s.trim().parse::<f64>().ok()?
        }
        _ => return None,
    };
    if number.is_finite() && number >= 0.0 {
        Some(number)
    } else {
        None
    }
}

/// Assess one retained CrUX current record; missing data never becomes a
/// passing metric. The error reports a record
/// that is not CrUX current field data.
pub fn assess(record: &Json) -> Result<Json, String> {
    let record = record
        .as_object()
        .ok_or_else(|| "CWV findings require CrUX current field data".to_string())?;
    if record.get("metric_source").and_then(Json::as_str) != Some("CrUX current field data") {
        return Err("CWV findings require CrUX current field data".to_string());
    }
    let period = record.get("collection_period").and_then(Json::as_object);
    let null = Json::Null;
    let first = period.and_then(|p| iso_date(p.get("firstDate").unwrap_or(&null)));
    // `period.get("lastDate") or period.get("endDate")`: its own key
    // first, the provider's alternative spelling only when the first is falsy.
    let last = period.and_then(|p| {
        let chosen = match p.get("lastDate") {
            Some(value) if truthy(value) => value,
            _ => p.get("endDate").unwrap_or(&null),
        };
        iso_date(chosen)
    });
    let valid_period = first.is_some() && last.is_some() && first.as_deref() <= last.as_deref();

    let metrics = record.get("metrics").and_then(Json::as_object);
    let record_state = record.get("state").and_then(Json::as_str);
    let ok_is_false = matches!(record.get("ok"), Some(Json::Bool(false)));

    let mut results = Map::new();
    for (name, good, poor, unit) in THRESHOLDS {
        let entry = metrics.and_then(|m| m.get(name));
        let raw = match entry {
            Some(e) if e.is_object() => e.get("p75").unwrap_or(&null),
            _ => &null,
        };
        let value = p75_value(raw);
        let reason = if record_state == Some("not_configured") {
            Some("not_configured")
        } else if record_state == Some("no_field_data") {
            Some("no_field_data")
        } else if matches!(
            record_state,
            Some("response_too_large") | Some("cache_too_large") | Some("cache_invalid")
        ) {
            record_state
        } else if ok_is_false {
            Some("provider_failed")
        } else if value.is_none() {
            Some("missing_or_invalid_p75")
        } else if !valid_period {
            Some("missing_or_invalid_collection_period")
        } else {
            None
        };
        let state = match reason {
            Some(_) => "unavailable",
            None => {
                let p75 = value.unwrap_or_default();
                if p75 <= good {
                    "good"
                } else if p75 <= poor {
                    "needs_improvement"
                } else {
                    "poor"
                }
            }
        };
        results.insert(
            name.to_string(),
            json!({
                "label": label_of(name),
                "p75": if reason.is_none() { value } else { None },
                "unit": unit,
                "state": state,
                "reason": reason,
            }),
        );
    }

    let states: Vec<&str> = results
        .values()
        .filter_map(|entry| entry.get("state").and_then(Json::as_str))
        .collect();
    let overall = if states.iter().all(|s| *s == "unavailable") {
        "unavailable"
    } else if states.contains(&"unavailable") {
        "partial"
    } else if states.contains(&"poor") {
        "poor"
    } else if states.contains(&"needs_improvement") {
        "needs_improvement"
    } else {
        "good"
    };

    // `record.get("record_target") or record.get("target")` and the same
    // fallback for the form factor: the provider's narrower scope wins when it
    // is present, the requested scope stands in when it is not.
    let target = match record.get("record_target") {
        Some(value) if truthy(value) => value.clone(),
        _ => record.get("target").cloned().unwrap_or(Json::Null),
    };
    let form_factor = match record.get("form_factor") {
        Some(value) if truthy(value) => value.clone(),
        _ => json!("ALL_FORM_FACTORS"),
    };

    let missing: Vec<String> = results
        .iter()
        .filter(|(_, entry)| entry.get("state").and_then(Json::as_str) == Some("unavailable"))
        .map(|(name, _)| name.clone())
        .collect();

    let mut out = Map::new();
    out.insert("schema".into(), json!("spiderhead.field-cwv/1"));
    out.insert("provider".into(), json!("crux"));
    out.insert("metric_source".into(), json!("field"));
    out.insert("policy".into(), json!(POLICY));
    out.insert("policy_url".into(), json!(POLICY_URL));
    out.insert("target".into(), target);
    out.insert(
        "requested_target".into(),
        record.get("target").cloned().unwrap_or(Json::Null),
    );
    out.insert(
        "record_target".into(),
        record.get("record_target").cloned().unwrap_or(Json::Null),
    );
    out.insert(
        "target_kind".into(),
        record.get("target_kind").cloned().unwrap_or(Json::Null),
    );
    out.insert("form_factor".into(), form_factor);
    out.insert(
        "collection_period".into(),
        json!({"first_date": first, "last_date": last}),
    );
    out.insert(
        "retrieved_at".into(),
        record.get("retrieved_at").cloned().unwrap_or(Json::Null),
    );
    out.insert("provider_access".into(), json!("read_only"));
    out.insert("cost_mode".into(), json!("free_within_quota"));
    out.insert("quota_mode".into(), json!("Google Cloud API quota"));
    out.insert("overall".into(), json!(overall));
    out.insert("metrics".into(), Json::Object(results));
    out.insert("missing_metrics".into(), json!(missing));
    Ok(Json::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One retained CrUX current record, the shape the audit hands to `assess`.
    fn record(lcp: Json, inp: Json, cls: Json) -> Json {
        json!({
            "metric_source": "CrUX current field data",
            "provider": "crux",
            "target": "https://example.test/page",
            "record_target": "https://example.test/page",
            "target_kind": "url",
            "form_factor": "PHONE",
            "state": "complete",
            "ok": true,
            "collection_period": {
                "firstDate": {"year": 2026, "month": 8, "day": 1},
                "lastDate": {"year": 2026, "month": 8, "day": 28},
            },
            "metrics": {
                "largest_contentful_paint": {"p75": lcp},
                "interaction_to_next_paint": {"p75": inp},
                "cumulative_layout_shift": {"p75": cls},
            },
            "retrieved_at": "2026-08-29T00:00:00+00:00",
        })
    }

    #[test]
    fn official_p75_boundaries() {
        for (name, good, poor, _) in THRESHOLDS {
            for (value, expected) in [
                (good, "good"),
                (good + 0.001, "needs_improvement"),
                (poor, "needs_improvement"),
                (poor + 0.001, "poor"),
            ] {
                let mut body = record(json!(2500), json!(200), json!("0.1"));
                body["metrics"][name]["p75"] = json!(value);
                let assessment = assess(&body).expect("assess");
                assert_eq!(
                    assessment["metrics"][name]["state"],
                    json!(expected),
                    "{name} at {value}"
                );
                assert_eq!(assessment["policy"], json!(POLICY), "{name} at {value}");
            }
        }
    }

    #[test]
    fn scope_form_factor_period_and_partial_metrics() {
        let mut origin = record(json!(2500), json!(200), json!("0.1"));
        origin["target_kind"] = json!("origin");
        origin["form_factor"] = Json::Null;
        let assessment = assess(&origin).expect("assess");
        assert_eq!(assessment["target_kind"], json!("origin"));
        assert_eq!(assessment["form_factor"], json!("ALL_FORM_FACTORS"));
        assert_eq!(
            assessment["collection_period"],
            json!({"first_date": "2026-08-01", "last_date": "2026-08-28"})
        );
        assert_eq!(assessment["provider_access"], json!("read_only"));
        assert_eq!(assessment["cost_mode"], json!("free_within_quota"));

        let mut desktop = record(json!(2500), json!(200), json!("0.1"));
        desktop["form_factor"] = json!("DESKTOP");
        assert_eq!(
            assess(&desktop).expect("assess")["form_factor"],
            json!("DESKTOP")
        );

        let mut partial = record(json!(2500), json!(200), json!("0.1"));
        partial["metrics"]
            .as_object_mut()
            .expect("metrics")
            .remove("interaction_to_next_paint");
        let assessment = assess(&partial).expect("assess");
        assert_eq!(assessment["overall"], json!("partial"));
        assert_eq!(
            assessment["missing_metrics"],
            json!(["interaction_to_next_paint"])
        );
        assert_eq!(
            assessment["metrics"]["largest_contentful_paint"]["state"],
            json!("good")
        );
    }

    #[test]
    fn crux_returned_url_is_the_measured_scope() {
        let mut body = record(json!(2500), json!(200), json!("0.1"));
        body["target"] = json!("https://example.test/page?campaign=synthetic");
        let assessment = assess(&body).expect("assess");
        assert_eq!(assessment["target"], json!("https://example.test/page"));
        assert_eq!(
            assessment["requested_target"],
            json!("https://example.test/page?campaign=synthetic")
        );
    }

    #[test]
    fn invalid_p75_is_unavailable_not_zero() {
        for bad in [
            Json::Null,
            json!("nan"),
            json!("inf"),
            json!(-1),
            json!("n/a"),
            json!(true),
        ] {
            let body = record(json!(2500), json!(200), bad);
            let assessment = assess(&body).expect("assess");
            assert_eq!(
                assessment["metrics"]["cumulative_layout_shift"],
                json!({
                    "label": "CLS",
                    "p75": null,
                    "unit": "score",
                    "state": "unavailable",
                    "reason": "missing_or_invalid_p75",
                })
            );
            assert_eq!(assessment["overall"], json!("partial"));
        }
    }

    #[test]
    fn invalid_period_and_no_field_data_are_unavailable() {
        let mut invalid = record(json!(2500), json!(200), json!("0.1"));
        invalid["collection_period"] = json!({"firstDate": {"year": 2026, "month": 2, "day": 30}});
        assert_eq!(
            assess(&invalid).expect("assess")["overall"],
            json!("unavailable")
        );

        let mut absent = record(json!(2500), json!(200), json!("0.1"));
        absent["state"] = json!("no_field_data");
        assert_eq!(
            assess(&absent).expect("assess")["overall"],
            json!("unavailable")
        );
        assert_eq!(
            assess(&absent).expect("assess")["target_kind"],
            json!("url")
        );
    }

    #[test]
    fn missing_credential_and_provider_failure_remain_distinct() {
        let mut unconfigured = record(json!(2500), json!(200), json!("0.1"));
        unconfigured["state"] = json!("not_configured");
        let assessment = assess(&unconfigured).expect("assess");
        assert_eq!(assessment["overall"], json!("unavailable"));
        assert_eq!(
            assessment["metrics"]["largest_contentful_paint"]["reason"],
            json!("not_configured")
        );

        let mut failed = record(json!(2500), json!(200), json!("0.1"));
        failed["ok"] = json!(false);
        let assessment = assess(&failed).expect("assess");
        assert_eq!(
            assessment["metrics"]["largest_contentful_paint"]["reason"],
            json!("provider_failed")
        );
    }

    #[test]
    fn psi_lab_cannot_be_assessed_as_field() {
        let mut lab = record(json!(2500), json!(200), json!("0.1"));
        lab["metric_source"] = json!("PSI Lighthouse lab");
        let error = assess(&lab).unwrap_err();
        assert!(error.contains("CrUX current"), "{error}");
    }
}

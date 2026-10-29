//! Project retained provider history into the registered audit issue stream.
//!
//! [`emit_provider_issues`] is the checks-layer half of provider findings: it reads a
//! `provider_findings.v1` evidence document (built by the sources layer) and emits issues
//! and skips. A check that cannot support a negative claim is skipped with its reason.

use serde_json::{Map, Value};

use super::py_truthy;
use crate::audit::context::AuditContext;

/// Issues emitted per provider check inside the audit stream.
pub const MAX_ISSUES_PER_PROVIDER_CHECK: usize = 500;

/// GSC search-analytics check ids, in registry order.
pub const GSC_ANALYTICS_CHECKS: [&str; 3] = [
    "GSC_ORPHAN",
    "NO_SEARCH_DATA",
    "NON_INDEXABLE_WITH_SEARCH_DATA",
];
/// GSC URL-Inspection check ids, in registry order.
pub const GSC_INSPECTION_CHECKS: [&str; 6] = [
    "GSC_INDEXED_WITH_ISSUES",
    "GSC_AMP_INVALID",
    "GSC_RICH_RESULTS_INVALID",
    "GSC_INDEXABLE_NOT_INDEXED",
    "GSC_USER_CANONICAL_NOT_SELECTED",
    "GSC_URL_NOT_ON_GOOGLE",
];
/// `analytics_findings` source check → audit issue id, per provider.
pub const ANALYTICS_CHECKS: [(&str, [(&str, &str); 4]); 2] = [
    (
        "ga4",
        [
            ("GA_ORPHAN_URL", "GA_ORPHAN_URL"),
            ("NO_GA_DATA", "NO_GA_DATA"),
            ("NON_INDEXABLE_WITH_GA_DATA", "NON_INDEXABLE_WITH_GA_DATA"),
            ("BOUNCE_RATE_ABOVE_70", "BOUNCE_RATE_ABOVE_70"),
        ],
    ),
    (
        "metrika",
        [
            ("GA_ORPHAN_URL", "METRIKA_ORPHAN_URL"),
            ("NO_GA_DATA", "NO_METRIKA_DATA"),
            (
                "NON_INDEXABLE_WITH_GA_DATA",
                "NON_INDEXABLE_WITH_METRIKA_DATA",
            ),
            ("BOUNCE_RATE_ABOVE_70", "METRIKA_BOUNCE_RATE_ABOVE_70"),
        ],
    ),
];

/// `value.get(key)` as an owned object — absent/non-object becomes `{}`.
fn obj(value: &Value, key: &str) -> Map<String, Value> {
    value
        .get(key)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

/// Scalar values render as plain text; containers use repr quoting.
fn py_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bool(b) => {
            if *b {
                "True".into()
            } else {
                "False".into()
            }
        }
        Value::Number(n) => n.to_string(),
        Value::Null => "None".into(),
        other => py_repr(other),
    }
}

/// `repr(value)`: strings take single quotes inside containers.
fn py_repr(value: &Value) -> String {
    match value {
        Value::String(s) => format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'")),
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(py_repr).collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Object(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("'{}': {}", k, py_repr(v)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
        other => py_str(other),
    }
}

/// `emit_provider_issues`: measured source findings join the audit's issue stream.
pub fn emit_provider_issues(ctx: &mut AuditContext, evidence: &Value) {
    if evidence.get("state").and_then(Value::as_str) == Some("unavailable")
        && obj(evidence, "sources").is_empty()
    {
        let reason = evidence
            .get("reason")
            .filter(|r| py_truthy(Some(r)))
            .map(py_str)
            .unwrap_or_else(|| "no validated project-bound provider history".to_owned());
        for check_id in GSC_ANALYTICS_CHECKS
            .iter()
            .chain(GSC_INSPECTION_CHECKS.iter())
        {
            if !ctx.settled(check_id) {
                ctx.skip(check_id, &reason);
            }
        }
        for (_, mapping) in &ANALYTICS_CHECKS {
            for (_, issue_check) in mapping {
                if !ctx.settled(issue_check) {
                    ctx.skip(issue_check, &reason);
                }
            }
        }
        return;
    }

    let sources = obj(evidence, "sources");
    let gsc = obj(&Value::Object(sources.clone()), "gsc");
    let search = obj(&Value::Object(gsc.clone()), "findings");
    let search_skipped = obj(&Value::Object(search.clone()), "_skipped");
    let search_coverage = obj(&Value::Object(gsc.clone()), "coverage");
    let source_detail = Map::from_iter([
        ("provider".into(), Value::from("gsc")),
        ("operation".into(), Value::from("search_analytics")),
        (
            "period".into(),
            search_coverage
                .get("period")
                .cloned()
                .unwrap_or(Value::Null),
        ),
        ("coverage".into(), Value::Object(search_coverage)),
        (
            "evidence_scope".into(),
            Value::from("retained project-bound Search Analytics history"),
        ),
    ]);
    let gsc_state = gsc.get("state").and_then(Value::as_str);
    let gsc_reason = gsc.get("reason").filter(|r| py_truthy(Some(r))).map(py_str);
    let truncated = py_truthy(search.get("truncated_findings"));
    for check_id in GSC_ANALYTICS_CHECKS {
        let found = search
            .get(check_id)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for item in found.iter().take(MAX_ISSUES_PER_PROVIDER_CHECK) {
            let Some(url) = item.get("url").and_then(Value::as_str) else {
                continue;
            };
            if url.is_empty() {
                continue;
            }
            let mut details = source_detail.clone();
            details.extend(obj(item, "details"));
            details.insert("safe_url".into(), item["safe_url"].clone());
            details.insert("url_id".into(), item["url_id"].clone());
            details.insert("evidence_truncated".into(), Value::from(truncated));
            ctx.add_source(
                check_id,
                Some(url),
                None,
                1,
                ("retained GSC Search Analytics", Vec::new()),
                details,
            );
        }
        let reason = search_skipped.get(check_id).filter(|r| py_truthy(Some(r)));
        if let Some(reason) = reason.filter(|_| found.is_empty()) {
            ctx.skip(check_id, &py_str(reason));
        } else if matches!(gsc_state, Some("unavailable" | "not_configured")) && found.is_empty() {
            let reason = gsc_reason
                .clone()
                .unwrap_or_else(|| "Search Console history is unavailable".to_owned());
            ctx.skip(check_id, &reason);
        } else if gsc_state != Some("complete") && found.is_empty() {
            ctx.skip(
                check_id,
                "retained Search Console source coverage is partial; absence is not established",
            );
        }
    }

    let inspection = obj(&Value::Object(gsc.clone()), "inspection");
    let inspection_findings = obj(&Value::Object(inspection.clone()), "findings");
    let inspection_details = Map::from_iter([
        ("provider".into(), Value::from("gsc")),
        ("operation".into(), Value::from("url_inspection")),
        (
            "scope".into(),
            inspection.get("scope").cloned().unwrap_or(Value::Null),
        ),
        (
            "requests_made".into(),
            inspection
                .get("requests_made")
                .cloned()
                .unwrap_or(Value::Null),
        ),
        (
            "inspected".into(),
            inspection.get("inspected").cloned().unwrap_or(Value::Null),
        ),
        (
            "crawl_pages".into(),
            inspection
                .get("crawl_pages")
                .cloned()
                .unwrap_or(Value::Null),
        ),
        (
            "crawl_urls_inspected".into(),
            inspection
                .get("crawl_urls_inspected")
                .cloned()
                .unwrap_or(Value::Null),
        ),
        (
            "sample_population_complete".into(),
            inspection
                .get("sample_population_complete")
                .cloned()
                .unwrap_or(Value::Null),
        ),
        (
            "failed".into(),
            inspection.get("failed").cloned().unwrap_or(Value::Null),
        ),
        (
            "deferred".into(),
            inspection.get("deferred").cloned().unwrap_or(Value::Null),
        ),
    ]);
    for check_id in GSC_INSPECTION_CHECKS {
        let found = inspection_findings
            .get(check_id)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for item in found.iter().take(MAX_ISSUES_PER_PROVIDER_CHECK) {
            let Some(url) = item.get("url").and_then(Value::as_str) else {
                continue;
            };
            if url.is_empty() {
                continue;
            }
            let mut details = inspection_details.clone();
            details.extend(obj(item, "details"));
            details.insert("safe_url".into(), item["safe_url"].clone());
            details.insert("url_id".into(), item["url_id"].clone());
            ctx.add_source(
                check_id,
                Some(url),
                None,
                1,
                ("sampled Search Console URL Inspection", Vec::new()),
                details,
            );
        }
        let sample_incomplete = !py_truthy(inspection.get("inspected"))
            || py_truthy(inspection.get("failed"))
            || py_truthy(inspection.get("deferred"))
            || !py_truthy(inspection.get("sample_population_complete"));
        if found.is_empty() && sample_incomplete {
            let reason = inspection
                .get("reason")
                .filter(|r| py_truthy(Some(r)))
                .map(py_str)
                .unwrap_or_else(|| {
                    "no complete Search Console URL Inspection sample covers this check".to_owned()
                });
            ctx.skip(check_id, &reason);
        }
    }

    for (provider, mapping) in ANALYTICS_CHECKS {
        let data = obj(&Value::Object(sources.clone()), provider);
        let findings = obj(&Value::Object(data.clone()), "findings");
        let coverage = obj(&Value::Object(data.clone()), "coverage");
        let data_state = data.get("state").and_then(Value::as_str);
        let data_reason = data
            .get("reason")
            .filter(|r| py_truthy(Some(r)))
            .map(py_str);
        let evidence_truncated = py_truthy(data.get("truncated_findings"));
        for (source_check, issue_check) in mapping {
            let item = obj(&Value::Object(findings.clone()), source_check);
            let urls = item
                .get("urls")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let safe_urls = item
                .get("safe_urls")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let url_ids = item
                .get("url_ids")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let detail = Map::from_iter([
                ("provider".into(), Value::from(provider)),
                (
                    "period".into(),
                    coverage.get("period").cloned().unwrap_or(Value::Null),
                ),
                ("coverage".into(), Value::Object(coverage.clone())),
                (
                    "evidence_scope".into(),
                    Value::from(format!(
                        "retained project-bound {provider} landing-page history"
                    )),
                ),
                (
                    "finding_state".into(),
                    item.get("state").cloned().unwrap_or(Value::Null),
                ),
                (
                    "unreadable_metrics".into(),
                    item.get("unreadable").cloned().unwrap_or(Value::from(0)),
                ),
                ("evidence_truncated".into(), Value::from(evidence_truncated)),
            ]);
            let source = format!("retained {provider} history");
            for (index, url) in urls.iter().take(MAX_ISSUES_PER_PROVIDER_CHECK).enumerate() {
                let Some(url) = url.as_str() else {
                    continue;
                };
                if url.is_empty() {
                    continue;
                }
                let mut details = detail.clone();
                details.insert(
                    "safe_url".into(),
                    safe_urls.get(index).cloned().unwrap_or(Value::Null),
                );
                details.insert(
                    "url_id".into(),
                    url_ids.get(index).cloned().unwrap_or(Value::Null),
                );
                ctx.add_source(
                    issue_check,
                    Some(url),
                    None,
                    1,
                    (&source, Vec::new()),
                    details,
                );
            }
            let item_state = item.get("state").and_then(Value::as_str);
            if (matches!(item_state, Some("withheld" | "skipped" | "partial"))
                || py_truthy(item.get("unreadable")))
                && urls.is_empty()
            {
                let reason = item
                    .get("reason")
                    .filter(|r| py_truthy(Some(r)))
                    .map(py_str)
                    .unwrap_or_else(|| format!("{provider} evidence is incomplete"));
                ctx.skip(issue_check, &reason);
            } else if matches!(data_state, Some("not_configured" | "unavailable"))
                && urls.is_empty()
            {
                let reason = data_reason
                    .clone()
                    .unwrap_or_else(|| format!("{provider} history is unavailable"));
                ctx.skip(issue_check, &reason);
            } else if data_state != Some("complete") && urls.is_empty() {
                ctx.skip(
                    issue_check,
                    &format!("retained {provider} coverage is partial; absence is not established"),
                );
            }
        }
    }
}

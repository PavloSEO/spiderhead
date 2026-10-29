//! The optional evidence blocks of `audit.md`: internal-linking shape and the
//! retained provider-history section (`summary.provider_findings`).
//!
//! Rendering of the optional `audit.md` evidence blocks, kept apart from the main renderer.

use serde_json::Value;

use super::md::{code, esc, py_str, py_str_or};
use crate::client_findings::check_title;
use crate::py;

/// `write_internal_linking`: the link graph's shape, printed with the health
/// summary — a measurement that could not be taken prints its reason.
pub(super) fn write_internal_linking(lines: &mut Vec<String>, block: Option<&Value>) {
    let Some(block) = block.filter(|b| py::truthy(Some(b))) else {
        return;
    };
    lines.push("**Internal linking**".to_owned());
    lines.push(String::new());
    if !py::truthy(block.get("measured")) {
        let reason = block
            .get("reason")
            .filter(|r| py::truthy(Some(r)))
            .map(|r| esc(Some(r)))
            .unwrap_or_else(|| "no reason recorded".to_owned());
        lines.push(format!("> Not measured. {reason}."));
        lines.push(String::new());
        return;
    }
    let empty = serde_json::Map::new();
    let depth = block
        .get("click_depth")
        .filter(|d| py::truthy(Some(d)))
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    if py::truthy(depth.get("measured")) {
        let within = depth.get("within").and_then(Value::as_object);
        let within_at = |key: &str| {
            within
                .and_then(|w| w.get(key))
                .map(py::str_value)
                .unwrap_or_else(|| "0".to_owned())
        };
        lines.push(format!(
            "- Click depth from `{}`, over {}:",
            code(depth.get("seed")),
            py_str(depth.get("edges")),
        ));
        lines.push(format!(
            "  within 3 clicks **{}**, within 5 **{}**, within 10 **{}**, maximum **{}**",
            within_at("3"),
            within_at("5"),
            within_at("10"),
            py_str(depth.get("max")),
        ));
        lines.push(format!(
            "- Reachable by internal links: **{}** of {} HTML pages ({} reached by no link at all)",
            py_str(depth.get("reachable")),
            py_str(depth.get("pages")),
            py_str(depth.get("unreachable")),
        ));
        lines.push(format!(
            "- Configured depth floor: **{}**",
            py_str(depth.get("floor_used")),
        ));
    } else {
        let reason = depth
            .get("reason")
            .filter(|r| py::truthy(Some(r)))
            .map(|r| esc(Some(r)))
            .unwrap_or_else(|| "no reason recorded".to_owned());
        lines.push(format!("- Click depth not measured: {reason}."));
    }
    let positions = block
        .get("by_position")
        .filter(|p| py::truthy(Some(p)))
        .and_then(Value::as_object);
    let unclassified = block.get("unclassified").cloned().unwrap_or(Value::from(0));
    let total = block.get("edges_total").cloned().unwrap_or(Value::from(0));
    if let Some(positions) = positions.filter(|p| !p.is_empty()) {
        let shown = positions
            .iter()
            .map(|(name, count)| format!("{} {}", name, py::str_value(count)))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("- Internal edges by position: {shown}"));
    }
    lines.push(format!(
        "- Unclassified edges (position never recorded): **{}** of {}. Never counted as content.",
        py::str_value(&unclassified),
        py::str_value(&total),
    ));
    let fraction = block
        .get("duplicate_fraction")
        .filter(|f| py::truthy(Some(f)))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    lines.push(format!(
        "- Repeated edges (same source, destination and anchor): **{}** ({:.1}% of all internal edges)",
        py_str_or(block.get("duplicate_edges"), "0"),
        fraction * 100.0,
    ));
    lines.push(String::new());
}

/// The `summary.provider_findings` section: retained provider history with
/// source periods, coverage and sample scope.
pub(super) fn write_provider_findings(lines: &mut Vec<String>, findings: Option<&Value>) {
    let Some(provider_findings) = findings.and_then(Value::as_object) else {
        return;
    };
    lines.push("## Search, analytics, and Cloudflare evidence".to_owned());
    lines.push(String::new());
    lines.push(
        concat!(
            "These measurements come from retained, project-bound provider history. ",
            "Registered per-URL findings also appear in the ordinary audit issue list; ",
            "this section preserves source periods, coverage, and sample scope."
        )
        .to_owned(),
    );
    lines.push(String::new());
    lines.push(format!(
        "- Evidence state: **{}**",
        esc(provider_findings.get("state")),
    ));
    if py::truthy(provider_findings.get("network_requests")) {
        lines.push("- URL Inspection: explicitly requested for this run".to_owned());
    } else {
        lines.push("- Provider history only; no URL Inspection request was made".to_owned());
    }
    if py::truthy(provider_findings.get("crawl_pages_omitted")) {
        lines.push(format!(
            "- Crawl pages omitted from the provider join: **{}**",
            py_str(provider_findings.get("crawl_pages_omitted")),
        ));
    }
    let sources = provider_findings
        .get("sources")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for (source, source_data) in &sources {
        let Some(source_data) = source_data.as_object() else {
            continue;
        };
        lines.push(String::new());
        lines.push(format!(
            "### {} — {}",
            esc(Some(&Value::String(source.clone()))),
            esc(source_data.get("state")),
        ));
        let coverage = source_data.get("coverage").and_then(Value::as_object);
        let period = coverage
            .and_then(|c| c.get("period"))
            .filter(|p| py::truthy(Some(p)));
        if let Some(period) = period {
            lines.push(format!(
                "- Period: {} to {}",
                esc(period.get("from")),
                esc(period.get("to")),
            ));
        }
        if let Some(coverage) = coverage {
            if py::truthy(coverage.get("days_expected")) {
                lines.push(format!(
                    "- Retained days complete: {} / {} (empty days: {})",
                    py_str_or(coverage.get("days_complete"), "0"),
                    py_str(coverage.get("days_expected")),
                    py_str_or(coverage.get("days_empty"), "0"),
                ));
            }
            if py::truthy(coverage.get("truncated")) {
                lines.push(
                    "- Provider rows reached the local read cap; absence findings are withheld."
                        .to_owned(),
                );
            }
        }
        if py::truthy(source_data.get("truncated_findings")) {
            lines.push("- URL examples are capped at 500 per finding.".to_owned());
        }
        if py::truthy(source_data.get("reason")) {
            lines.push(format!("- Reason: {}", esc(source_data.get("reason"))));
        }
        if let Some(findings) = source_data.get("findings").and_then(Value::as_object) {
            for (check, finding) in findings {
                write_finding(lines, check, finding);
            }
        }
        write_url_facts(lines, source_data);
        write_inspection(lines, source_data);
    }
    lines.push(String::new());
}

/// One `findings` entry of a provider source: the `_skipped` reason map, a
/// per-URL list, or a state/reason object.
fn write_finding(lines: &mut Vec<String>, check: &str, finding: &Value) {
    if check == "_skipped" {
        if let Some(skipped) = finding.as_object() {
            for (skipped_check, reason) in skipped {
                lines.push(format!(
                    "- {} skipped: {}",
                    check_title(&Value::String(skipped_check.clone())),
                    esc(Some(reason)),
                ));
            }
        }
        return;
    }
    let (state, reason, urls, safe_urls): (Value, Option<Value>, Vec<Value>, Vec<Value>);
    if let Some(list) = finding.as_array() {
        let entries: Vec<&serde_json::Map<String, Value>> =
            list.iter().filter_map(Value::as_object).collect();
        state = Value::String("reported".to_owned());
        reason = Some(Value::String("measured URL evidence".to_owned()));
        urls = entries
            .iter()
            .filter_map(|e| e.get("url").cloned())
            .collect();
        safe_urls = entries
            .iter()
            .map(|e| match e.get("safe_url") {
                Some(v) if py::truthy(Some(v)) => v.clone(),
                _ => e.get("url").cloned().unwrap_or(Value::Null),
            })
            .collect();
    } else if let Some(map) = finding.as_object() {
        state = map
            .get("state")
            .cloned()
            .unwrap_or_else(|| Value::String("unavailable".to_owned()));
        reason = map.get("reason").cloned();
        urls = map
            .get("urls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        safe_urls = map
            .get("safe_urls")
            .and_then(Value::as_array)
            .filter(|rows| !rows.is_empty())
            .cloned()
            .unwrap_or_else(|| urls.clone());
    } else {
        return;
    }
    lines.push(format!(
        "- **{}** — {}: {}",
        check_title(&Value::String(check.to_owned())),
        esc(Some(&state)),
        esc(reason.as_ref()),
    ));
    for url in safe_urls.iter().take(20) {
        lines.push(format!("  - {}", esc(Some(url))));
    }
    if urls.len() > 20 {
        lines.push(format!("  - … {} more", urls.len() - 20));
    }
}

/// Grouped Cloudflare URL/day facts of one provider source.
fn write_url_facts(lines: &mut Vec<String>, source_data: &serde_json::Map<String, Value>) {
    let Some(facts) = source_data.get("url_facts").and_then(Value::as_array) else {
        return;
    };
    lines.push(String::new());
    lines.push(format!(
        "- Grouped Cloudflare URL/day facts: {} retained; {} omitted by display cap.",
        facts.len(),
        py_str_or(source_data.get("omitted_url_facts"), "0"),
    ));
    for fact in facts.iter().take(20) {
        lines.push(format!(
            "  - `{}{}` ({}): {} requests, edge {}, origin {}, cache {}, {}",
            esc(fact.get("host")),
            esc(fact.get("path")),
            py_str(fact.get("date")),
            py_str(fact.get("requests")),
            py_str(fact.get("edge_status")),
            py_str(fact.get("origin_status")),
            esc(fact.get("cache_status")),
            esc(fact.get("bot_name")),
        ));
    }
    if facts.len() > 20 {
        lines.push(format!("  - … {} more", facts.len() - 20));
    }
}

/// The bounded URL-Inspection sample of one provider source.
fn write_inspection(lines: &mut Vec<String>, source_data: &serde_json::Map<String, Value>) {
    let Some(inspection) = source_data
        .get("inspection")
        .filter(|i| py::truthy(Some(i)))
        .and_then(Value::as_object)
    else {
        return;
    };
    let zero = Value::from(0);
    lines.push(String::new());
    lines.push(format!(
        "URL Inspection sample: {} URLs inspected; {} deferred; state {}.",
        esc(Some(super::md::get_or(inspection, "inspected", &zero))),
        esc(Some(super::md::get_or(inspection, "deferred", &zero))),
        esc(inspection.get("state")),
    ));
    if !py::truthy(inspection.get("sample_population_complete")) {
        lines.push(
            "- Unsampled crawl URLs remain unknown; no site-wide clean conclusion is drawn from this bounded inspection sample."
                .to_owned(),
        );
    }
    let sf_checks = inspection.get("sf_checks").and_then(Value::as_object);
    if let Some(findings) = inspection.get("findings").and_then(Value::as_object) {
        for (check, rows) in findings {
            let label = sf_checks
                .and_then(|m| m.get(check))
                .cloned()
                .unwrap_or_else(|| Value::String(check.clone()));
            let rows: &[Value] = rows.as_array().map(Vec::as_slice).unwrap_or(&[]);
            lines.push(format!(
                "- **{}** — {} sampled findings",
                esc(Some(&label)),
                rows.len()
            ));
            for row in rows.iter().take(20) {
                let url = match row.get("safe_url") {
                    Some(v) if py::truthy(Some(v)) => v.clone(),
                    _ => row.get("url").cloned().unwrap_or(Value::Null),
                };
                lines.push(format!("  - {}", esc(Some(&url))));
            }
            if rows.len() > 20 {
                lines.push(format!("  - … {} more", rows.len() - 20));
            }
        }
    }
}

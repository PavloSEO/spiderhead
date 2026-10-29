//! Reader-facing finding labels and reproductions for human report formats.
//!
//! The audit documents remain the machine-evidence source; this module only builds
//! an immutable display projection for Markdown, CSV, XLSX and DOCX.
//!

use regex::Regex;
use serde_json::{json, Map, Value};

use crate::check_meta;
use crate::py;

const MAX_EVIDENCE_ITEMS: usize = 10;

fn check_identifier() -> &'static Regex {
    static RE: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"\b[A-Z][A-Z0-9]{2,}(?:_[A-Z0-9]+)+\b").unwrap());
    &RE
}

fn attribution_i() -> &'static Regex {
    static RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        regex::RegexBuilder::new(
            r"^(?:seohead|screaming frog)\s+(?:found|reported|detected)\s+(.+?)\.?$",
        )
        .case_insensitive(true)
        .build()
        .unwrap()
    });
    &RE
}

fn protected_evidence() -> &'static Regex {
    static RE: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r#"https?://\S+|`[^`]*`|"[^"]*"|'[^']*'"#).unwrap());
    &RE
}

fn producer_reason() -> &'static Regex {
    static RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        regex::RegexBuilder::new(r"\b(?:seohead|screaming frog)\b")
            .case_insensitive(true)
            .build()
            .unwrap()
    });
    &RE
}

fn finding_view_label_map(column: &str) -> &str {
    match column {
        "severity" => "Severity",
        "check" => "Check",
        "url" => "URL",
        "text" => "Observation",
        "status_code" => "Status",
        "occurrences_count" => "Occurrences",
        "fix_hint" => "Fix hint",
        "details" => "Evidence",
        "locations" => "Locations",
        "segment" => "Segment",
        other => other,
    }
}

/// Explain that a saved view is a displayed subset while audit totals remain
/// source-wide.
pub fn finding_view_notice(summary: &Value) -> Option<String> {
    let view = py::get(summary, "finding_view").filter(|v| v.is_object())?;
    let counts = py::get_map(view, "counts");
    let page = py::get_map(view, "pagination");
    let mut note = format!(
        "Saved finding view {} (view revision {}, config revision {}): showing {} \
         rows at offset {} of {} matches from {} source findings; {} did not match.",
        py::str_value(view.get("name").unwrap_or(&Value::Null)),
        py::str_value(view.get("revision").unwrap_or(&Value::Null)),
        py::str_value(view.get("config_revision").unwrap_or(&Value::Null)),
        counts
            .get("returned")
            .map(py::str_value)
            .unwrap_or_else(|| "0".into()),
        page.get("offset")
            .map(py::str_value)
            .unwrap_or_else(|| "0".into()),
        counts
            .get("matched")
            .map(py::str_value)
            .unwrap_or_else(|| "0".into()),
        counts
            .get("source")
            .map(py::str_value)
            .unwrap_or_else(|| "0".into()),
        counts
            .get("filtered")
            .map(py::str_value)
            .unwrap_or_else(|| "0".into()),
    );
    if py::truthy(page.get("truncated")) {
        note.push_str(" More matching rows remain; use the next offset to continue.");
    }
    if view.get("state").and_then(Value::as_str) == Some("partial") {
        let mut missing: std::collections::BTreeMap<String, i64> = counts
            .get("missing_filter_fields")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), py::to_i64(v).unwrap_or(0)))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(fields) = counts
            .get("missing_projection_fields")
            .and_then(Value::as_object)
        {
            for (field, count) in fields {
                let count = py::to_i64(count).unwrap_or(0);
                let entry = missing.entry(field.clone()).or_insert(0);
                *entry = (*entry).max(count);
            }
        }
        let detail: Vec<String> = missing
            .iter()
            .map(|(field, count)| format!("{field}={count}"))
            .collect();
        note.push_str(&format!(
            " Some filter or projection fields were unavailable ({}).",
            detail.join(", ")
        ));
    }
    note.push_str(" Audit totals, evidence coverage, and scores describe the full source audit.");
    if let Some(exclusions) = py::get(summary, "finding_exclusions").filter(|v| v.is_object()) {
        let suppressed = py::get(exclusions, "suppressed_total");
        if let Some(total) = suppressed.and_then(py::to_i64).filter(|t| *t > 0) {
            note.push_str(&format!(
                " The source audit also records {total} excluded findings."
            ));
            if let Some(by_rule) = py::get(exclusions, "by_rule").and_then(Value::as_object) {
                if !by_rule.is_empty() {
                    let mut rule_rows: Vec<(&String, &Value)> = by_rule.iter().collect();
                    rule_rows.sort_by(|a, b| a.0.cmp(b.0));
                    let details: Vec<String> = rule_rows
                        .iter()
                        .take(20)
                        .map(|(name, count)| {
                            let clean: String = name
                                .chars()
                                .map(|c| {
                                    if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-'
                                    {
                                        c
                                    } else {
                                        '?'
                                    }
                                })
                                .take(64)
                                .collect();
                            let num =
                                if py::is_int(count) && py::to_i64(count).is_some_and(|c| c >= 0) {
                                    py::str_value(count)
                                } else {
                                    "unknown".to_owned()
                                };
                            format!("{clean}={num}")
                        })
                        .collect();
                    let suffix = if rule_rows.len() > 20 {
                        format!(" (+{} more)", rule_rows.len() - 20)
                    } else {
                        String::new()
                    };
                    note.push_str(&format!(
                        " Exclusion counts by rule: {}{}.",
                        details.join(", "),
                        suffix
                    ));
                }
            }
        }
    }
    Some(note)
}

/// The view's declared column names, when a saved view is active.
pub fn finding_view_columns(summary: &Value) -> Option<Vec<String>> {
    let view = py::get(summary, "finding_view").filter(|v| v.is_object())?;
    let columns = view.get("columns")?.as_array()?;
    Some(columns.iter().map(py::str_value).collect())
}

/// Display label for a saved-view column.
pub fn finding_view_label(column: &str) -> String {
    finding_view_label_map(column).to_owned()
}

/// Reader-facing title without exposing a registry identifier.
pub fn check_title(check: &Value) -> String {
    let check_id = py::str_value(check);
    if check_meta::contains(&check_id) {
        return check_meta::message(&check_id)
            .unwrap_or("Audit finding")
            .to_owned();
    }
    static SNAKE: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"[a-z][a-z0-9]*(?:_[a-z0-9]+)+").unwrap());
    if SNAKE.is_match(&check_id)
        && SNAKE.find(&check_id).map(|m| m.range()) == Some(0..check_id.len())
    {
        let mut out = check_id.replace('_', " ");
        if let Some(first) = out.get_mut(0..1) {
            first.make_ascii_uppercase();
        }
        return out;
    }
    "Audit finding".to_owned()
}

/// Join saved exclusion rules to their recorded counts without recomputing them.
pub fn finding_exclusion_report(
    summary: &Value,
    suppressed_issues: Option<&[Value]>,
) -> Option<Value> {
    let empty = Map::new();
    let exclusion = py::get(summary, "finding_exclusions")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let policy: Vec<&Value> = py::get(summary, "finding_exclusion_policy")
        .and_then(Value::as_array)
        .map(|items| items.iter().collect())
        .unwrap_or_default();
    let suppressed: &[Value] = suppressed_issues.unwrap_or(&[]);
    if exclusion.is_empty() && policy.is_empty() && suppressed.is_empty() {
        return None;
    }

    let mut counts_by_rule: Map<String, Value> = Map::new();
    match exclusion.get("by_rule") {
        Some(Value::Array(rows)) => {
            for row in rows {
                if let (Some(m), Some(id)) = (row.as_object(), py::get(row, "id")) {
                    if !id.is_null() {
                        counts_by_rule.insert(py::str_value(id), row.clone());
                        let _ = m;
                    }
                }
            }
        }
        Some(Value::Object(rows)) => {
            for (rule_id, count) in rows {
                let value = if count.is_object() {
                    count.clone()
                } else {
                    json!({"suppressed_findings": count})
                };
                counts_by_rule.insert(rule_id.clone(), value);
            }
        }
        _ => {}
    }

    let mut policy_by_id: Map<String, Value> = Map::new();
    for row in &policy {
        if row.is_object() && py::get(row, "id").is_some_and(|id| !id.is_null()) {
            policy_by_id.insert(py::str_value(py::get(row, "id").unwrap()), (*row).clone());
        }
    }
    for issue in suppressed {
        let marker = py::get(issue, "suppression").and_then(Value::as_object);
        if let Some(marker) = marker {
            if let Some(rule_id) = marker.get("rule_id").filter(|v| !v.is_null()) {
                let rule_id = py::str_value(rule_id);
                policy_by_id.entry(rule_id.clone()).or_insert_with(|| {
                    json!({
                        "id": rule_id,
                        "pattern": marker.get("pattern").cloned().unwrap_or(json!("")),
                        "reason": marker.get("reason").cloned().unwrap_or(json!("")),
                        "checks": [],
                    })
                });
            }
        }
    }

    let mut rules: Vec<Value> = Vec::new();
    for (rule_id, rule) in &policy_by_id {
        let counts = counts_by_rule.get(rule_id).cloned().unwrap_or(json!({}));
        rules.push(json!({
            "id": rule_id,
            "pattern": py::get(rule, "pattern").cloned().unwrap_or(json!("")),
            "checks": py::get(rule, "checks").cloned().unwrap_or(json!([])),
            "reason": py::get(rule, "reason").filter(|v| py::truthy(Some(*v)))
                .cloned()
                .unwrap_or_else(|| py::get(&counts, "reason").cloned().unwrap_or(json!(""))),
            "suppressed_findings": py::get(&counts, "suppressed_findings").cloned().unwrap_or(json!(0)),
            "suppressed_occurrences": py::get(&counts, "suppressed_occurrences").cloned().unwrap_or(json!(0)),
        }));
    }
    for (rule_id, counts) in &counts_by_rule {
        if !policy_by_id.contains_key(rule_id) {
            rules.push(json!({
                "id": rule_id,
                "pattern": "",
                "checks": [],
                "reason": py::get(counts, "reason").cloned().unwrap_or(json!("")),
                "suppressed_findings": py::get(counts, "suppressed_findings").cloned().unwrap_or(json!(0)),
                "suppressed_occurrences": py::get(counts, "suppressed_occurrences").cloned().unwrap_or(json!(0)),
            }));
        }
    }

    let mut total = exclusion
        .get("suppressed_total")
        .and_then(py::to_i64)
        .unwrap_or(-1);
    if total < 0 {
        total = suppressed.len() as i64;
    }
    let mut rules_configured = exclusion
        .get("rules_configured")
        .and_then(py::to_i64)
        .unwrap_or(-1);
    if rules_configured < 0 {
        rules_configured = policy_by_id.len() as i64;
    }
    let totals = py::get_map(summary, "totals");
    let mut occurrences = totals
        .get("suppressed_occurrences")
        .and_then(py::to_i64)
        .unwrap_or(-1);
    if occurrences < 0 {
        occurrences = rules
            .iter()
            .map(|rule| {
                py::get(rule, "suppressed_occurrences")
                    .and_then(py::to_i64)
                    .unwrap_or(0)
            })
            .filter(|v| *v >= 0)
            .sum();
    }
    Some(json!({
        "suppressed_total": total,
        "suppressed_occurrences": occurrences,
        "rules_configured": rules_configured,
        "rules": rules,
        "issues": suppressed,
    }))
}

/// Keep primitive recorded details, with readable labels and bounded shape.
fn detail_rows(details: &Value) -> Vec<String> {
    let Some(map) = details.as_object() else {
        return Vec::new();
    };

    fn record(value: &Value) -> String {
        let Some(map) = value.as_object() else {
            return String::new();
        };
        let mut entries: Vec<(&String, &Value)> = map.iter().collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        let bits: Vec<String> = entries
            .iter()
            .filter(|(_, item)| primitive_kept(item))
            .map(|(name, item)| format!("{}: {}", capitalize(name), py::str_value(item)))
            .collect();
        if bits.is_empty() {
            "Structured record retained in the saved audit".to_owned()
        } else {
            bits.join("; ")
        }
    }

    let mut rows = Vec::new();
    let mut entries: Vec<(&String, &Value)> = map.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    for (key, value) in entries {
        let label = capitalize(key);
        if primitive_kept(value) {
            rows.push(format!("{label}: {}", py::str_value(value)));
        } else if let Some(items) = value.as_array() {
            let values: Vec<String> = items
                .iter()
                .filter(|item| matches!(item, Value::String(_) | Value::Number(_) | Value::Bool(_)))
                .map(py::str_value)
                .collect();
            let records: Vec<String> = items
                .iter()
                .filter(|item| item.is_object())
                .map(record)
                .collect();
            let unsupported = items.len() - values.len() - records.len();
            let mut parts: Vec<String> = Vec::new();
            let mut remaining = MAX_EVIDENCE_ITEMS;
            if !values.is_empty() {
                let shown: Vec<String> = values.iter().take(remaining).cloned().collect();
                remaining -= shown.len();
                let suffix = if values.len() > shown.len() {
                    format!("; {} more values omitted", values.len() - shown.len())
                } else {
                    String::new()
                };
                parts.push(format!("{}{}", shown.join(", "), suffix));
            }
            if !records.is_empty() {
                let shown: Vec<String> = records
                    .iter()
                    .take(MAX_EVIDENCE_ITEMS)
                    .take(remaining)
                    .cloned()
                    .collect();
                let suffix = if records.len() > shown.len() {
                    format!(
                        "; {} more structured records omitted",
                        records.len() - shown.len()
                    )
                } else {
                    String::new()
                };
                parts.push(format!("{}{}", shown.join(" | "), suffix));
            }
            if unsupported > 0 {
                parts.push(format!("{unsupported} unsupported values omitted"));
            }
            if !parts.is_empty() {
                rows.push(format!("{label}: {}", parts.join("; ")));
            }
        } else if value.is_object() {
            rows.push(format!("{label}: {}", record(value)));
        }
    }
    rows
}

fn primitive_kept(value: &Value) -> bool {
    match value {
        Value::String(text) => !text.is_empty(),
        Value::Number(_) | Value::Bool(_) => true,
        _ => false,
    }
}

fn capitalize(text: &str) -> String {
    let mut out = text.replace('_', " ");
    if let Some(first) = out.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    out
}

fn observation(value: &Value) -> String {
    let text = py::str_value(value).trim().to_owned();
    if text.is_empty() {
        return String::new();
    }
    let text = match attribution_i().captures(&text) {
        Some(caps) => caps.get(1).map(|m| m.as_str()).unwrap_or(&text).to_owned(),
        None => text,
    };
    if check_identifier().is_match(&text)
        && check_identifier().find(&text).map(|m| m.range()) == Some(0..text.len())
        && check_meta::contains(&text)
    {
        return String::new();
    }
    translate_unprotected(&text, |part| {
        check_identifier()
            .replace_all(part, |caps: &regex::Captures| {
                let id = caps.get(0).map(|m| m.as_str()).unwrap_or_default();
                if check_meta::contains(id) {
                    check_title(&json!(id))
                } else {
                    id.to_owned()
                }
            })
            .into_owned()
    })
}

fn translate_unprotected(text: &str, translate: impl Fn(&str) -> String) -> String {
    let mut pieces = String::new();
    let mut cursor = 0;
    for m in protected_evidence().find_iter(text) {
        pieces.push_str(&translate(&text[cursor..m.start()]));
        pieces.push_str(m.as_str());
        cursor = m.end();
    }
    pieces.push_str(&translate(&text[cursor..]));
    pieces
}

/// Translate known collector wrappers in a failure reason, preserving its cause.
pub fn client_reason(value: &Value) -> String {
    let text = observation(value);
    translate_unprotected(&text, |part| {
        producer_reason()
            .replace_all(part, "The audit")
            .into_owned()
    })
}

fn location_rows(locations: &Value) -> Vec<String> {
    let Some(items) = locations.as_array() else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for location in items {
        let Some(loc) = location.as_object() else {
            continue;
        };
        let mut bits: Vec<String> = Vec::new();
        if let Some(text) = loc
            .get("source_url")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            bits.push(format!("Source: {text}"));
        }
        if let Some(text) = loc
            .get("anchor")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            bits.push(format!("Anchor: {text}"));
        }
        if let Some(text) = loc
            .get("link_position")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            bits.push(format!("Position: {text}"));
        }
        if let Some(text) = loc
            .get("link_path")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            bits.push(format!("XPath: {text}"));
        }
        if !bits.is_empty() {
            rows.push(bits.join("; "));
        }
    }
    let shown: Vec<String> = rows.iter().take(MAX_EVIDENCE_ITEMS).cloned().collect();
    let mut shown = shown;
    if rows.len() > shown.len() {
        shown.push(format!(
            "{} more locations omitted",
            rows.len() - shown.len()
        ));
    }
    shown
}

fn evidence_reference(value: &Value) -> Value {
    let mut contract = py::get(value, "contract").filter(|v| v.is_object());
    if contract.is_none() {
        return json!({
            "state": "unavailable",
            "reason": "no stable saved-evidence reference is present in this audit",
        });
    }
    if let Some(observations) = contract
        .and_then(|c| py::get(c, "observations"))
        .and_then(Value::as_array)
    {
        for observation in observations {
            if !observation.is_object() {
                continue;
            }
            if matches!(
                observation.get("state").and_then(Value::as_str),
                Some("measured") | Some("imported_projection")
            ) {
                contract = Some(observation);
                break;
            }
        }
    }
    let contract = contract.unwrap_or(&Value::Null);
    let state = contract.get("state").and_then(Value::as_str);
    if !matches!(state, Some("measured") | Some("imported_projection")) {
        return json!({
            "state": "unavailable",
            "reason": contract.get("reason").map(py::str_value).filter(|s| !s.is_empty())
                .unwrap_or_else(|| "saved evidence is unavailable".to_owned()),
        });
    }
    let identifier = contract.get("id").and_then(Value::as_str);
    let source_table = contract.get("source_table").and_then(Value::as_str);
    let observation_id = contract.get("observation_id").and_then(Value::as_str);
    let complete = [identifier, source_table, observation_id]
        .iter()
        .all(|item| item.is_some_and(|s| !s.is_empty()));
    if !complete {
        return json!({
            "state": "unavailable",
            "reason": "saved evidence reference is incomplete",
        });
    }
    json!({
        "state": state,
        "id": identifier,
        "source_table": source_table,
        "observation_id": observation_id,
        "role": contract.get("role").map(py::str_value).filter(|s| !s.is_empty()).unwrap_or_else(|| "observation".to_owned()),
    })
}

/// State only the primitive observation the saved audit can support.
pub fn reproduction(finding: &Value, observation_arg: &str) -> String {
    let url = py::get(finding, "url");
    let status = py::get(finding, "status_code");
    let obs = if observation_arg.is_empty() {
        observation(py::get(finding, "text").unwrap_or(&Value::Null))
    } else {
        observation_arg.to_owned()
    };
    if let Some(url_text) = url.and_then(Value::as_str).filter(|s| !s.is_empty()) {
        let details: Vec<String> = detail_rows(py::get(finding, "details").unwrap_or(&Value::Null))
            .into_iter()
            .filter(|row| {
                !row.contains("Structured record retained")
                    && !row.contains("unsupported values omitted")
            })
            .collect();
        if let Some(code) = status.and_then(py::to_i64) {
            let result = format!("{url_text} returned HTTP {code}.");
            let observed = details.first().cloned().unwrap_or(obs);
            return if observed.is_empty() {
                result
            } else {
                format!("{result} Recorded observation: {observed}")
            };
        }
        if let Some(first) = details.first() {
            return format!("At {url_text}: {first}.");
        }
        if !obs.is_empty() {
            return format!("At {url_text}: {obs}");
        }
    }
    if let Some(locations) = py::get(finding, "locations").and_then(Value::as_array) {
        for location in locations {
            if location.is_object() && location.get("source_url").is_some_and(|v| v.is_string()) {
                let target = url
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(|u| format!(" to {u}"))
                    .unwrap_or_default();
                let path = location
                    .get("link_path")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(|p| format!(" at {p}"))
                    .unwrap_or_default();
                return format!(
                    "Link from {}{}{}.",
                    location["source_url"].as_str().unwrap_or_default(),
                    path,
                    target
                );
            }
        }
    }
    let target = url
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(|u| format!(" Target URL: {u}."))
        .unwrap_or_default();
    format!("Reproduction unavailable from the saved audit.{target}")
}

/// Copy one finding with client-only display fields.
pub fn project_finding(finding: &Value) -> Value {
    let mut projected = finding.clone();
    let obs = observation(py::get(finding, "text").unwrap_or(&Value::Null));
    let details = detail_rows(py::get(finding, "details").unwrap_or(&Value::Null));
    let locations = location_rows(py::get(finding, "locations").unwrap_or(&Value::Null));
    let evidence = evidence_reference(py::get(finding, "evidence").unwrap_or(&Value::Null));
    if let Some(m) = projected.as_object_mut() {
        m.insert(
            "client_title".into(),
            json!(check_title(
                py::get(finding, "check").unwrap_or(&Value::Null)
            )),
        );
        m.insert("client_observation".into(), json!(obs));
        m.insert(
            "client_reproduction".into(),
            json!(reproduction(finding, &obs)),
        );
        m.insert("client_details".into(), json!(details));
        m.insert("client_locations".into(), json!(locations));
        m.insert("client_evidence".into(), evidence.clone());
    }
    if py::truthy(py::get(finding, "evidence")) {
        if let Some(m) = projected.as_object_mut() {
            let mut details = m
                .get("client_details")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if let Some(id) = py::get(&evidence, "id").and_then(Value::as_str) {
                details.push(json!(format!("Saved observation: {id}")));
            } else {
                details.push(json!(format!(
                    "Saved observation unavailable: {}",
                    py::get(&evidence, "reason")
                        .map(py::str_value)
                        .unwrap_or_else(|| "not captured".into())
                )));
            }
            m.insert("client_details".into(), json!(details));
        }
    }
    projected
}

/// Return a document copy whose findings carry display fields.
pub fn project_document(document: &Value) -> Value {
    let mut projected = document.clone();
    let mut summary = py::get_map(document, "summary").clone();
    summary.insert(
        "checks_disabled".into(),
        json!(py::get_list(
            document.get("summary").unwrap_or(&Value::Null),
            "checks_disabled"
        )
        .iter()
        .filter(|item| item.is_object())
        .map(|item| {
            let mut row = item.clone();
            if let Some(m) = row.as_object_mut() {
                m.insert(
                    "reason".into(),
                    json!(client_reason(
                        py::get(item, "reason").unwrap_or(&Value::Null)
                    )),
                );
            }
            row
        })
        .collect::<Vec<_>>()),
    );
    summary.insert(
        "tools_failed".into(),
        json!(py::get_list(
            document.get("summary").unwrap_or(&Value::Null),
            "tools_failed"
        )
        .iter()
        .filter(|item| item.is_object())
        .map(|item| {
            let mut row = item.clone();
            if let Some(m) = row.as_object_mut() {
                m.insert(
                    "error".into(),
                    json!(client_reason(
                        py::get(item, "error").unwrap_or(&Value::Null)
                    )),
                );
            }
            row
        })
        .collect::<Vec<_>>()),
    );
    let findings: Vec<Value> = py::get_list(document, "findings")
        .iter()
        .filter(|f| f.is_object())
        .map(project_finding)
        .collect();
    let columns = finding_view_columns(&Value::Object(summary.clone()));
    let findings = if columns.is_some() {
        findings
            .into_iter()
            .map(|mut finding| {
                let field = |key: &str| py::get(&finding, key).cloned().unwrap_or(Value::Null);
                let client_details = py::get_list(&finding, "client_details")
                    .iter()
                    .map(py::str_value)
                    .collect::<Vec<_>>()
                    .join("; ");
                let client_locations = py::get_list(&finding, "client_locations")
                    .iter()
                    .map(py::str_value)
                    .collect::<Vec<_>>()
                    .join("; ");
                let view_fields = json!({
                    "severity": field("severity"),
                    "check": field("check"),
                    "url": field("url"),
                    "text": field("client_observation"),
                    "status_code": field("status_code"),
                    "occurrences_count": field("occurrences_count"),
                    "fix_hint": field("fix_hint"),
                    "details": if client_details.is_empty() { Value::Null } else { json!(client_details) },
                    "locations": if client_locations.is_empty() { Value::Null } else { json!(client_locations) },
                    "segment": field("__view_segment"),
                });
                if let Some(m) = finding.as_object_mut() {
                    m.insert("view_fields".into(), view_fields);
                }
                finding
            })
            .collect()
    } else {
        findings
    };
    if let Some(m) = projected.as_object_mut() {
        m.insert("summary".into(), Value::Object(summary));
        m.insert("findings".into(), json!(findings));
    }
    projected
}

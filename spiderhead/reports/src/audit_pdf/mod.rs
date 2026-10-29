//! Render a retained technical-audit PDF model as self-contained localized HTML.
//!
//! Output is print-ready for headless Chromium:
//! inline CSS/SVG only, no scripts and no external resources.

use std::collections::HashMap;
use std::sync::LazyLock;

use serde_json::{Map, Value};

use crate::py::str_value;
use crate::svg_charts::{bar_chart, ChartOpts, Series};
use crate::traffic_dashboard::{load_brand, Brand};

mod labels;
#[allow(unused_imports)]
use self::labels::*;

const MODEL_SCHEMA: &str = "spiderhead.technical-audit-pdf/1";

fn mapping(value: Option<&Value>) -> Map<String, Value> {
    value
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn mapping_ref(value: Option<&Value>) -> Option<&Map<String, Value>> {
    value.and_then(Value::as_object)
}

/// HTML-escape the text form of a value, including quote characters.
fn escape_value(value: &Value) -> String {
    escape_html(&str_value(value))
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

fn is_number(value: &Value) -> bool {
    value.as_f64().is_some() && !value.is_boolean()
}

/// Integer with thousands separators.
fn thousands(value: i64) -> String {
    let negative = value < 0;
    let digits = value.unsigned_abs().to_string();
    let mut out = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    if negative {
        format!("-{out}")
    } else {
        out
    }
}

fn count_text(value: &Value, lang: &str) -> String {
    let Some(number) = value.as_f64() else {
        return l(lang, "not_reported").to_owned();
    };
    if value.is_boolean() || !number.is_finite() || number < 0.0 {
        return l(lang, "not_reported").to_owned();
    }
    let integer = number as i64;
    let formatted = thousands(integer);
    if lang == "en" {
        formatted
    } else {
        formatted.replace(',', "\u{202f}")
    }
}

fn key_title(value: &Value, lang: &str) -> String {
    let raw = if value.is_null() {
        String::new()
    } else {
        str_value(value)
    };
    let text = raw.replace('_', " ").trim().to_owned();
    let lower = raw.to_lowercase();
    let localized = labels(lang)
        .get(lower.as_str())
        .or_else(|| labels(lang).get(format!("{lower}_label").as_str()));
    if let Some(localized) = localized {
        return localized.to_string();
    }
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn field_rows(value: &Value, lang: &str) -> Vec<(String, String)> {
    let Some(map) = value.as_object() else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    fn flatten(key: &str, item: &Value, lang: &str, rows: &mut Vec<(String, String)>) {
        if item.is_null() || item.as_str() == Some("") {
            return;
        }
        if let Some(child_map) = item.as_object() {
            for (child_key, child) in child_map {
                let next = if key.is_empty() {
                    child_key.clone()
                } else {
                    format!("{key}.{child_key}")
                };
                flatten(&next, child, lang, rows);
            }
        } else if let Some(list) = item.as_array() {
            if list.is_empty() {
                return;
            }
            for (index, child) in list.iter().enumerate() {
                flatten(&format!("{key} {}", index + 1), child, lang, rows);
            }
        } else if item.is_string() || item.is_number() || item.is_boolean() {
            let indexed = regex::Regex::new(r"^(.+?) (\d+)$").unwrap();
            let mut title_parts = Vec::new();
            for part in key.split('.') {
                if let Some(caps) = indexed.captures(part) {
                    let name = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                    let num = caps.get(2).map(|m| m.as_str()).unwrap_or("");
                    let suffix = if lang == "en" {
                        format!(" #{num}")
                    } else {
                        format!(" №{num}")
                    };
                    title_parts.push(format!("{}{}", key_title(&Value::from(name), lang), suffix));
                } else {
                    title_parts.push(key_title(&Value::from(part), lang));
                }
            }
            rows.push((title_parts.join(" · "), str_value(item)));
        }
    }
    for (key, item) in map {
        flatten(key, item, lang, &mut rows);
    }
    rows
}

fn readable_record(value: &Value, lang: &str) -> String {
    if value.is_object() {
        return field_rows(value, lang)
            .iter()
            .map(|(key, item)| format!("{key}: {item}"))
            .collect::<Vec<_>>()
            .join("; ");
    }
    if let Some(list) = value.as_array() {
        return list
            .iter()
            .filter(|item| !item.is_null())
            .map(|item| readable_record(item, lang))
            .collect::<Vec<_>>()
            .join("; ");
    }
    if value.is_null() {
        String::new()
    } else {
        str_value(value)
    }
}

fn reference_text(value: Option<&Value>, lang: &str) -> String {
    let source_ref = mapping_ref(value);
    let Some(source_ref) = source_ref else {
        return String::new();
    };
    if let Some(pointer) = source_ref.get("pointer").and_then(Value::as_str) {
        return pointer.to_owned();
    }
    let mut bits = Vec::new();
    if let Some(collection) = source_ref.get("collection") {
        bits.push(str_value(collection));
    }
    if let Some(index) = source_ref.get("index").filter(|v| !v.is_null()) {
        bits.push(format!("{}: {}", l(lang, "index_label"), str_value(index)));
    }
    bits.join("; ")
}

fn localize_generated_text(value: &Value, lang: &str, check: Option<&Value>) -> Value {
    let Some(text) = value.as_str() else {
        return value.clone();
    };
    if lang != "ru" {
        return value.clone();
    }
    let check_str = check.and_then(Value::as_str);
    if text == "Audit finding" {
        return Value::from(l(lang, "audit_finding"));
    }
    if text == "Title element is missing" || check_str == Some("TITLE_MISSING") {
        return Value::from(l(lang, "title_element_is_missing"));
    }
    let key = match text {
        "no stable saved-evidence reference is present in this audit" => {
            Some("evidence_reason_no_stable")
        }
        "saved evidence is unavailable" => Some("evidence_reason_unavailable"),
        "saved evidence reference is incomplete" => Some("evidence_reason_incomplete"),
        _ => None,
    };
    match key.and_then(|k| labels(lang).get(k)) {
        Some(localized) => Value::from(*localized),
        None => value.clone(),
    }
}

fn state_label(value: &Value, lang: &str) -> String {
    let raw = if value.is_null() { "unknown" } else { "" };
    let state = if value.is_null() {
        "unknown".to_owned()
    } else {
        str_value(value).to_lowercase()
    };
    let _ = raw;
    match state.as_str() {
        "complete" => l(lang, "complete").to_owned(),
        "partial" => l(lang, "partial").to_owned(),
        "failed" => l(lang, "failed").to_owned(),
        "unknown" => l(lang, "unknown").to_owned(),
        "not_requested" => l(lang, "not_requested").to_owned(),
        "recorded" | "reported" => l(lang, "recorded").to_owned(),
        "unavailable" => l(lang, "unavailable").to_owned(),
        "measured" => {
            if lang == "en" {
                "Measured".to_owned()
            } else {
                "Измерено".to_owned()
            }
        }
        "absent" => {
            if lang == "en" {
                "Absent".to_owned()
            } else {
                "Отсутствует".to_owned()
            }
        }
        "skipped" => l(lang, "skipped").to_owned(),
        "disabled" => l(lang, "disabled").to_owned(),
        "not_run" => {
            if lang == "en" {
                "Not run".to_owned()
            } else {
                "Не запускалось".to_owned()
            }
        }
        "run" => {
            if lang == "en" {
                "Run".to_owned()
            } else {
                "Запуск".to_owned()
            }
        }
        "ran" => {
            if lang == "en" {
                "Ran".to_owned()
            } else {
                "Выполнена".to_owned()
            }
        }
        "stale" => {
            if lang == "en" {
                "Stale".to_owned()
            } else {
                "Устарело".to_owned()
            }
        }
        "not_applicable" => {
            if lang == "en" {
                "Not applicable".to_owned()
            } else {
                "Не применимо".to_owned()
            }
        }
        "open" => {
            if lang == "en" {
                "Open".to_owned()
            } else {
                "Открыто".to_owned()
            }
        }
        "verified" => {
            if lang == "en" {
                "Verified".to_owned()
            } else {
                "Проверено".to_owned()
            }
        }
        "imported_projection" => {
            if lang == "en" {
                "Imported evidence projection".to_owned()
            } else {
                "Импортированная проекция свидетельства".to_owned()
            }
        }
        _ => {
            if value.is_null() {
                l(lang, "unknown").to_owned()
            } else {
                str_value(value)
            }
        }
    }
}

fn severity_label(value: &Value, lang: &str) -> String {
    match str_value(value).to_lowercase().as_str() {
        "critical" => l(lang, "critical").to_owned(),
        "warning" => l(lang, "warning").to_owned(),
        "notice" => l(lang, "notice").to_owned(),
        _ => {
            if value.is_null() {
                l(lang, "not_reported").to_owned()
            } else {
                str_value(value)
            }
        }
    }
}

fn evidence_reference_rows(value: &Map<String, Value>, lang: &str) -> Vec<(String, Value)> {
    let fields: [(&str, &str); 6] = [
        ("state", "evidence_status"),
        ("reason", "evidence_reason"),
        ("id", "evidence_id"),
        ("source_table", "evidence_source_table"),
        ("observation_id", "evidence_observation_id"),
        ("role", "evidence_role"),
    ];
    let mut rows = Vec::new();
    for (key, title_key) in fields {
        let Some(content) = value.get(key).filter(|v| !v.is_null()) else {
            continue;
        };
        let content = if key == "state" {
            Value::from(state_label(content, lang))
        } else if key == "reason" {
            localize_generated_text(content, lang, None)
        } else {
            content.clone()
        };
        rows.push((l(lang, title_key).to_owned(), content));
    }
    let known: Vec<&str> = fields.iter().map(|(k, _)| *k).collect();
    let mut extras: Vec<(&String, &Value)> = value
        .iter()
        .filter(|(k, v)| !known.contains(&k.as_str()) && !v.is_null())
        .collect();
    extras.sort_by_key(|(k, _)| (*k).clone());
    for (key, content) in extras {
        let content = if content.is_object() || content.is_array() {
            Value::from(readable_record(content, lang))
        } else {
            content.clone()
        };
        rows.push((key_title(&Value::from(key.as_str()), lang), content));
    }
    rows
}

fn table(headers: &[String], rows: &[Vec<Value>], lang: &str) -> String {
    let header_html: String = headers
        .iter()
        .map(|label| format!("<th scope=\"col\">{}</th>", escape_html(label)))
        .collect();
    if rows.is_empty() {
        return format!("<p class=\"empty\">{}</p>", escape_html(l(lang, "none")));
    }
    let body_html: String = rows
        .iter()
        .map(|row| {
            format!(
                "<tr>{}</tr>",
                row.iter()
                    .map(|value| format!("<td>{}</td>", escape_value(value)))
                    .collect::<String>()
            )
        })
        .collect();
    format!(
        "<div class=\"table-wrap\"><table><thead><tr>{header_html}</tr></thead><tbody>{body_html}</tbody></table></div>"
    )
}

fn chapter(number: &str, title: &str, kicker: &str) -> String {
    format!(
        "<div class=\"chapter\"><span class=\"chapter-number\">{}</span><div>{}<h2>{}</h2></div></div>",
        escape_html(number),
        if kicker.is_empty() {
            String::new()
        } else {
            format!("<p class=\"kicker\">{}</p>", escape_html(kicker))
        },
        escape_html(title)
    )
}

fn reason_text(reason: &Value, lang: &str) -> Value {
    if let Some(map) = reason.as_object() {
        let field = map.get("source_field");
        let value = map.get("value");
        let label = field
            .and_then(Value::as_str)
            .and_then(|f| labels(lang).get(format!("{f}_label").as_str()));
        if let (Some(label), Some(v)) = (label, value.filter(|v| !v.is_null())) {
            return Value::from(format!("{label}: {}", str_value(v)));
        }
        return match value.filter(|v| !v.is_null()) {
            Some(v) => v.clone(),
            None => reason.clone(),
        };
    }
    reason.clone()
}

fn scope_text(scope: &Value, lang: &str) -> Value {
    let empty = scope.is_null()
        || scope.as_str() == Some("")
        || scope.as_object().is_some_and(|o| o.is_empty())
        || scope.as_array().is_some_and(|a| a.is_empty());
    if empty {
        return Value::from(l(lang, "no_reported_scope"));
    }
    if let Some(map) = scope.as_object() {
        let mut parts = Vec::new();
        for (key, value) in map {
            let operation = if key == "operation" {
                value
                    .as_str()
                    .and_then(|v| labels(lang).get(format!("operation_{v}").as_str()))
                    .map(|s| (*s).to_owned())
            } else {
                None
            };
            let rendered = if value.is_object() || value.is_array() {
                readable_record(value, lang)
            } else {
                str_value(value)
            };
            parts.push(format!(
                "{}: {}",
                key_title(&Value::from(key.as_str()), lang),
                operation.unwrap_or(rendered)
            ));
        }
        return Value::from(parts.join("; "));
    }
    if let Some(list) = scope.as_array() {
        return Value::from(list.iter().map(str_value).collect::<Vec<_>>().join("; "));
    }
    scope.clone()
}

fn status_block(run: &Map<String, Value>, lang: &str) -> String {
    let value = run.get("state").and_then(Value::as_str).unwrap_or("");
    let state = if ["complete", "partial", "failed", "unknown"].contains(&value) {
        value.to_owned()
    } else {
        "unknown".to_owned()
    };
    let reasons = run
        .get("reasons")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let reason_html: String = reasons
        .iter()
        .map(|reason| format!("<li>{}</li>", escape_value(&reason_text(reason, lang))))
        .collect();
    let scope = run.get("scope").cloned().unwrap_or(Value::Null);
    let scope_rendered = scope_text(&scope, lang);
    let note_key = match state.as_str() {
        "complete" => "complete_note",
        "partial" => "partial_warning",
        "failed" => "failed_warning",
        _ => "unknown_warning",
    };
    let note = l(lang, note_key);
    format!(
        "<aside class=\"run-state state-{state}\" data-state=\"{state}\"><div class=\"state-label\">{} · {}</div>{}<p><b>{}:</b> {}</p>{}{}</aside>",
        escape_html(l(lang, "state")),
        escape_html(l(lang, state.as_str())),
        if note.is_empty() {
            String::new()
        } else {
            format!("<p class=\"state-note\">{}</p>", escape_html(note))
        },
        escape_html(l(lang, "scope")),
        escape_value(&scope_rendered),
        if scope.as_object().is_some_and(|o| o.contains_key("finding_view")) {
            format!(
                "<p class=\"state-note\">{}</p>",
                escape_html(l(lang, "view_scope_note"))
            )
        } else {
            String::new()
        },
        if reason_html.is_empty() {
            String::new()
        } else {
            format!("<ul>{reason_html}</ul>")
        }
    )
}

fn count_value(section: &Map<String, Value>, key: &str) -> Value {
    section
        .get(key)
        .and_then(Value::as_object)
        .and_then(|b| b.get("projected_count"))
        .cloned()
        .unwrap_or(Value::Null)
}

fn declared_value(value: Option<&Value>) -> Value {
    match value {
        Some(v) if v.is_object() => {
            if v.get("state").and_then(Value::as_str) == Some("reported") {
                v.get("value").cloned().unwrap_or(Value::Null)
            } else {
                Value::Null
            }
        }
        Some(v) => v.clone(),
        None => Value::Null,
    }
}

fn summary_cards(
    summary: &Map<String, Value>,
    coverage: &Map<String, Value>,
    lang: &str,
    source_totals: bool,
) -> String {
    let counts = mapping(summary.get("counts"));
    let mut cards = String::new();
    for (key, label) in [
        ("findings", l(lang, "findings")),
        ("pages", l(lang, "pages")),
        ("checks", l(lang, "checks")),
        ("backlog", l(lang, "backlog")),
    ] {
        let record = mapping(counts.get(key));
        let mut value = record
            .get("projected_count")
            .cloned()
            .unwrap_or(Value::Null);
        if key == "checks" {
            value = if coverage.get("state").and_then(Value::as_str) == Some("reported") {
                coverage
                    .get("checks")
                    .and_then(Value::as_array)
                    .map(|c| Value::from(c.len() as i64))
                    .unwrap_or(Value::Null)
            } else {
                Value::Null
            };
        }
        let source_count = record
            .get("source_count")
            .or_else(|| record.get("source_total"))
            .cloned()
            .unwrap_or(Value::Null);
        if source_totals && (key == "findings" || key == "pages") {
            value = source_count.clone();
        }
        let declared = declared_value(
            record
                .get("declared_count")
                .or_else(|| record.get("declared_total")),
        );
        let mut display_value = count_text(&value, lang);
        if key == "backlog" {
            if let Some(state) = record.get("state").and_then(Value::as_str) {
                if state == "not_requested" || state == "unavailable" {
                    display_value = l(lang, state).to_owned();
                }
            }
        }
        let mut details = Vec::new();
        if !source_count.is_null() {
            details.push(format!(
                "{}: {}",
                escape_html(l(lang, "source")),
                escape_html(&count_text(&source_count, lang))
            ));
        }
        if !declared.is_null() && declared != source_count {
            let detail_label = if key == "checks" {
                l(lang, "check_inventory")
            } else {
                l(lang, "declared")
            };
            details.push(format!(
                "{}: {}",
                escape_html(detail_label),
                escape_html(&count_text(&declared, lang))
            ));
        }
        cards.push_str(&format!(
            "<div class=\"metric-card\"><span>{}</span><strong>{}</strong><small>{}</small></div>",
            escape_html(label),
            escape_html(&display_value),
            details.join(" · ")
        ));
    }
    format!("<div class=\"metric-grid\">{cards}</div>")
}

fn chart(
    title: &str,
    chart_labels: &[String],
    values: &[f64],
    lang: &str,
    brand: &Brand,
) -> String {
    if chart_labels.is_empty() || values.iter().any(|v| !v.is_finite() || *v < 0.0) {
        return format!(
            "<figure class=\"chart-card\"><figcaption>{}</figcaption><p class=\"empty\">{}</p></figure>",
            escape_html(title),
            escape_html(l(lang, "not_reported"))
        );
    }
    let series = Series {
        name: title.to_owned(),
        values: values.iter().map(|v| Some(*v)).collect(),
        color: brand.accent.clone(),
        dashed: false,
        width: 2.4,
        labels: true,
    };
    let mut opts = ChartOpts::new(520.0, 210.0, {
        let lang = lang.to_owned();
        move |value| count_text(&Value::from(value), &lang)
    });
    opts.ink = brand.ink.clone();
    opts.grid = brand.table_header.clone();
    opts.title = Some(title.to_owned());
    let svg = bar_chart(chart_labels, &[series], &opts);
    format!(
        "<figure class=\"chart-card\"><figcaption>{}</figcaption>{svg}</figure>",
        escape_html(title)
    )
}

fn severity_chart(summary: &Map<String, Value>, lang: &str, brand: &Brand) -> String {
    let source = mapping(summary.get("source"));
    let counts = ["findings_by_severity", "severity_counts", "by_severity"]
        .iter()
        .find_map(|key| source.get(*key).and_then(Value::as_object))
        .cloned()
        .unwrap_or_default();
    let values: Vec<Value> = ["critical", "warning", "notice"]
        .iter()
        .map(|key| counts.get(*key).cloned().unwrap_or(Value::Null))
        .collect();
    let title = l(lang, "findings_by_severity");
    if values
        .iter()
        .any(|v| !is_number(v) || !v.as_f64().unwrap_or(f64::NAN).is_finite())
    {
        return format!(
            "<figure class=\"chart-card\"><figcaption>{}</figcaption><p class=\"empty\">{}</p></figure>",
            escape_html(title),
            escape_html(l(lang, "no_severity_data"))
        );
    }
    let chart_labels: Vec<String> = ["critical", "warning", "notice"]
        .iter()
        .map(|key| l(lang, key).to_owned())
        .collect();
    let numbers: Vec<f64> = values.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect();
    chart(title, &chart_labels, &numbers, lang, brand)
}

fn coverage_state(item: &Value) -> Value {
    let check = mapping_ref(Some(item)).cloned().unwrap_or_default();
    let state = check.get("state").cloned().unwrap_or(Value::Null);
    let record = mapping(check.get("record"));
    if state.is_null()
        || state.as_str() == Some("unreported")
        || state.as_str() == Some("source_reported")
    {
        return record
            .get("state")
            .cloned()
            .filter(|s| !s.is_null())
            .unwrap_or(state);
    }
    state
}

fn checks_chart(
    summary: &Map<String, Value>,
    coverage: &Map<String, Value>,
    source_kind: Option<&Value>,
    lang: &str,
    brand: &Brand,
) -> String {
    let counts = mapping(mapping(summary.get("counts")).get("checks"));
    let check_records = coverage.get("checks");
    let capability_group = mapping(counts.get("capabilities"));
    let is_sf = source_kind.and_then(Value::as_str) == Some("sf-audit");
    if capability_group.get("state").and_then(Value::as_str) == Some("reported")
        && check_records
            .and_then(Value::as_array)
            .is_some_and(|r| !r.is_empty())
    {
        let capabilities: Vec<&Value> = check_records
            .and_then(Value::as_array)
            .map(|records| {
                records
                    .iter()
                    .filter(|item| {
                        mapping_ref(item.get("source_ref"))
                            .and_then(|sr| sr.get("collection"))
                            .and_then(Value::as_str)
                            == Some("summary.evidence_contract.capability_rows")
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut status_counts: HashMap<String, i64> = HashMap::new();
        let mut order: Vec<String> = Vec::new();
        for item in &capabilities {
            let state = coverage_state(item);
            if let Some(state) = state.as_str() {
                if !status_counts.contains_key(state) {
                    order.push(state.to_owned());
                }
                *status_counts.entry(state.to_owned()).or_insert(0) += 1;
            }
        }
        if !status_counts.is_empty() {
            let statuses = [
                "measured",
                "partial",
                "absent",
                "unavailable",
                "not_requested",
                "unreported",
            ];
            let chart_labels: Vec<String> = statuses
                .iter()
                .filter(|s| status_counts.contains_key(**s))
                .map(|s| state_label(&Value::from(*s), lang))
                .collect();
            let values: Vec<f64> = statuses
                .iter()
                .filter_map(|s| status_counts.get(*s).map(|n| *n as f64))
                .collect();
            let _ = order;
            return chart(
                l(lang, "capabilities_by_state"),
                &chart_labels,
                &values,
                lang,
                brand,
            );
        }
    }
    let fields: [(&str, String); 6] = [
        (
            "ran",
            if is_sf {
                l(lang, "group_fired").to_owned()
            } else {
                l(lang, "ran").to_owned()
            },
        ),
        ("silent", l(lang, "group_silent").to_owned()),
        ("failed", l(lang, "failed_checks").to_owned()),
        ("skipped", l(lang, "skipped").to_owned()),
        ("disabled", l(lang, "disabled").to_owned()),
        (
            "page_tools_failed",
            l(lang, "group_page_tools_failed").to_owned(),
        ),
    ];
    let mut available: Vec<(String, f64)> = Vec::new();
    let mut unavailable: Vec<String> = Vec::new();
    for (key, label) in &fields {
        let group = mapping(counts.get(*key));
        let value = group.get("projected_count");
        let valid = group.get("state").and_then(Value::as_str) == Some("reported")
            && value.is_some_and(|v| {
                is_number(v) && v.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0)
            });
        if valid {
            available.push((label.clone(), value.and_then(Value::as_f64).unwrap_or(0.0)));
        } else {
            unavailable.push(label.clone());
        }
    }
    if available.is_empty() && coverage.get("state").and_then(Value::as_str) == Some("reported") {
        if let Some(records) = check_records.and_then(Value::as_array) {
            let mut statuses: HashMap<String, i64> = HashMap::new();
            for item in records {
                let state = coverage_state(item);
                if let Some(state) = state.as_str() {
                    *statuses.entry(state.to_owned()).or_insert(0) += 1;
                }
            }
            for (key, label) in [
                ("measured", l(lang, "ran")),
                ("failed", l(lang, "failed_checks")),
                ("skipped", l(lang, "skipped")),
                ("disabled", l(lang, "disabled")),
                ("unavailable", l(lang, "unavailable")),
                ("not_requested", l(lang, "not_requested")),
                ("partial", l(lang, "partial")),
            ] {
                if let Some(count) = statuses.get(key) {
                    available.push((label.to_owned(), *count as f64));
                }
            }
        }
    }
    if available.is_empty() {
        return format!(
            "<figure class=\"chart-card\"><figcaption>{}</figcaption><p class=\"empty\">{}</p></figure>",
            escape_html(l(lang, "checks_by_state")),
            escape_html(l(lang, "no_check_counts"))
        );
    }
    let rendered = chart(
        l(lang, "checks_by_state"),
        &available.iter().map(|(l, _)| l.clone()).collect::<Vec<_>>(),
        &available.iter().map(|(_, v)| *v).collect::<Vec<_>>(),
        lang,
        brand,
    );
    let missing = if unavailable.is_empty() {
        String::new()
    } else {
        format!(
            "<p class=\"empty\">{}: {}</p>",
            escape_html(l(lang, "unreported_groups")),
            unavailable
                .iter()
                .map(|label| escape_html(label))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    rendered.replace("</figure>", &(missing + "</figure>"))
}

fn finding_cards(findings: &[Value], lang: &str) -> String {
    let mut output = Vec::new();
    for (index, value) in findings.iter().enumerate() {
        let finding = mapping(Some(value));
        let display = mapping(finding.get("display"));
        let record = mapping(finding.get("record"));
        let severity = record
            .get("severity")
            .filter(|v| !v.is_null() && v.as_str() != Some(""))
            .or_else(|| display.get("severity"))
            .cloned()
            .unwrap_or(Value::Null);
        let severity_text = severity_label(&severity, lang);
        let check = display
            .get("check_key")
            .filter(|v| !v.is_null() && v.as_str() != Some(""))
            .or_else(|| record.get("check"))
            .or_else(|| record.get("source"))
            .cloned()
            .unwrap_or(Value::from(""));
        let mut title = display
            .get("title")
            .filter(|v| !v.is_null() && v.as_str() != Some(""))
            .or_else(|| record.get("title"))
            .cloned()
            .unwrap_or_else(|| Value::from(l(lang, "finding")));
        if title.as_str() == Some("Audit finding") || check.as_str() == Some("TITLE_MISSING") {
            title = localize_generated_text(&title, lang, Some(&check));
        }
        let observation = display.get("observation");
        let mut reproduction = display.get("reproduction").cloned().unwrap_or(Value::Null);
        if let Some(text) = reproduction.as_str() {
            if lang == "ru" && text.starts_with("At ") {
                reproduction = Value::from(format!("На {}", &text[3..]));
            }
        }
        let url = record.get("url");
        let source_ref = mapping(finding.get("source_ref"));
        let source_id = source_ref
            .get("id")
            .filter(|v| !v.is_null() && v.as_str() != Some(""))
            .or_else(|| source_ref.get("id_if_present"));
        let mut parts = vec![format!(
            "<article class=\"finding-card severity-{}\">",
            escape_html(&str_value(&severity).to_lowercase())
        )];
        parts.push("<div class=\"finding-heading\">".to_owned());
        parts.push(format!(
            "<span class=\"finding-number\">{:02}</span>",
            index + 1
        ));
        let mut meta = format!(
            "<div><p class=\"finding-meta\">{}",
            escape_html(&severity_text)
        );
        let check_text = str_value(&check);
        if !check_text.is_empty() {
            meta.push_str(&format!(" · {}", escape_html(&check_text)));
        }
        if let Some(id) = source_id {
            let id_text = str_value(id);
            if !id_text.is_empty() {
                meta.push_str(&format!(" · {}", escape_html(&id_text)));
            }
        }
        meta.push_str("</p>");
        meta.push_str(&format!("<h3>{}</h3></div></div>", escape_value(&title)));
        parts.push(meta);
        if observation.is_some_and(|v| !v.is_null() && v.as_str() != Some("")) {
            parts.push(format!(
                "<p><b>{}:</b> {}</p>",
                escape_html(l(lang, "observation")),
                escape_value(observation.unwrap_or(&Value::Null))
            ));
        }
        if !reproduction.is_null() && reproduction.as_str() != Some("") {
            parts.push(format!(
                "<p><b>{}:</b> {}</p>",
                escape_html(l(lang, "reproduction")),
                escape_value(&reproduction)
            ));
        }
        if url.is_some_and(|v| !v.is_null() && v.as_str() != Some("")) {
            parts.push(format!(
                "<p class=\"url\"><b>{}:</b> {}</p>",
                escape_html(l(lang, "page_url")),
                escape_value(url.unwrap_or(&Value::Null))
            ));
        }
        for key in [
            "status_code",
            "occurrences_count",
            "occurrence_count",
            "fix_hint",
            "remediation_status",
            "verification_status",
        ] {
            let raw = record.get(key);
            let present = raw.is_some_and(|v| {
                !v.is_null()
                    && v.as_str() != Some("")
                    && !v.as_array().is_some_and(|a| a.is_empty())
                    && !v.as_object().is_some_and(|o| o.is_empty())
            });
            if !present {
                continue;
            }
            let raw = raw.unwrap_or(&Value::Null);
            let mut rendered = if raw.is_object() || raw.is_array() {
                readable_record(raw, lang)
            } else {
                str_value(raw)
            };
            if key.ends_with("_status") || key == "status" {
                rendered = state_label(&Value::from(rendered), lang);
            }
            parts.push(format!(
                "<p class=\"evidence-extra\"><b>{}:</b> {}</p>",
                escape_html(&key_title(&Value::from(key), lang)),
                escape_html(&rendered)
            ));
        }
        for key in ["details", "locations"] {
            let label = if key == "details" {
                l(lang, "details_label")
            } else {
                l(lang, "locations_label")
            };
            let display_values = display.get(key);
            if display_values
                .and_then(Value::as_array)
                .is_some_and(|a| !a.is_empty())
            {
                for detail in display_values.and_then(Value::as_array).unwrap_or(&vec![]) {
                    parts.push(format!(
                        "<p class=\"evidence-extra\"><b>{}:</b> {}</p>",
                        escape_html(label),
                        escape_value(detail)
                    ));
                }
                continue;
            }
            let raw = record.get(key);
            let present = raw.is_some_and(|v| {
                !v.is_null()
                    && v.as_str() != Some("")
                    && !v.as_array().is_some_and(|a| a.is_empty())
                    && !v.as_object().is_some_and(|o| o.is_empty())
            });
            if present {
                let raw = raw.unwrap_or(&Value::Null);
                let rendered = if raw.is_object() || raw.is_array() {
                    readable_record(raw, lang)
                } else {
                    str_value(raw)
                };
                parts.push(format!(
                    "<p class=\"evidence-extra\"><b>{}:</b> {}</p>",
                    escape_html(label),
                    escape_html(&rendered)
                ));
            }
        }
        if let Some(evidence_ref) = display.get("evidence_reference").and_then(Value::as_object) {
            if !evidence_ref.is_empty() {
                let reference = evidence_reference_rows(evidence_ref, lang)
                    .iter()
                    .map(|(label, value)| format!("{label}: {}", str_value(value)))
                    .collect::<Vec<_>>()
                    .join("; ");
                parts.push(format!(
                    "<p class=\"evidence-extra\"><b>{}:</b> {}</p>",
                    escape_html(l(lang, "source")),
                    escape_html(&reference)
                ));
            }
        }
        parts.push("</article>".to_owned());
        output.push(parts.join(""));
    }
    if output.is_empty() {
        format!("<p class=\"empty\">{}</p>", escape_html(l(lang, "none")))
    } else {
        output.join("")
    }
}

fn page_rows(pages: &[Value]) -> Vec<Vec<Value>> {
    pages
        .iter()
        .map(|value| {
            let record = mapping(value.get("record"));
            vec![
                record.get("url").cloned().unwrap_or(Value::Null),
                record
                    .get("status")
                    .or_else(|| record.get("status_code"))
                    .cloned()
                    .unwrap_or(Value::Null),
                record.get("title").cloned().unwrap_or(Value::Null),
            ]
        })
        .collect()
}

fn coverage_source_details(
    section: &str,
    record: &Value,
    coverage: &Map<String, Value>,
    lang: &str,
) -> String {
    if section != "source_evidence" || !record.is_object() {
        return readable_record(record, lang);
    }
    let capability_checks: Vec<Value> = coverage
        .get("checks")
        .and_then(Value::as_array)
        .map(|checks| {
            checks
                .iter()
                .filter(|check| {
                    mapping_ref(check.get("source_ref"))
                        .and_then(|sr| sr.get("collection"))
                        .and_then(Value::as_str)
                        == Some("summary.evidence_contract.capability_rows")
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let projected: Vec<Value> = capability_checks
        .iter()
        .map(|check| check.get("record").cloned().unwrap_or(Value::Null))
        .collect();

    fn remove_projected(value: &Value, projected: &[Value]) -> (Value, usize) {
        if let Some(map) = value.as_object() {
            let mut result = Map::new();
            let mut removed = 0;
            for (key, item) in map {
                if key == "capability_rows"
                    && item
                        .as_array()
                        .is_some_and(|list| list.to_vec() == projected)
                {
                    removed += item.as_array().map(|l| l.len()).unwrap_or(0);
                    continue;
                }
                let (clean, child_removed) = remove_projected(item, projected);
                result.insert(key.clone(), clean);
                removed += child_removed;
            }
            return (Value::Object(result), removed);
        }
        if let Some(list) = value.as_array() {
            let mut result = Vec::new();
            let mut removed = 0;
            for item in list {
                let (clean, child_removed) = remove_projected(item, projected);
                result.push(clean);
                removed += child_removed;
            }
            return (Value::Array(result), removed);
        }
        (value.clone(), 0)
    }

    let (clean_record, removed_count) = remove_projected(record, &projected);
    let mut parts = Vec::new();
    let readable = readable_record(&clean_record, lang);
    if !readable.is_empty() {
        parts.push(readable);
    }
    if removed_count > 0 {
        parts.push(format!(
            "{removed_count} {}",
            l(lang, "capability_records_below")
        ));
    }
    parts.join("; ")
}

fn coverage_rows(coverage: &Map<String, Value>, lang: &str, compact: bool) -> Vec<Vec<Value>> {
    let mut rows = Vec::new();
    for (section, label_key) in [
        ("source_evidence", "source"),
        ("source_check_coverage", "source_check_coverage"),
    ] {
        let Some(evidence) = coverage.get(section).and_then(Value::as_object) else {
            continue;
        };
        let state = evidence.get("state").and_then(Value::as_str);
        let record = evidence.get("record");
        let mut details = if record.is_none() || record == Some(&Value::Null) {
            if state == Some("unavailable") {
                l(lang, "unavailable").to_owned()
            } else {
                l(lang, "not_reported").to_owned()
            }
        } else if compact
            && section == "source_check_coverage"
            && record.is_some_and(Value::is_object)
        {
            let record_map = mapping(record);
            let scalar_only: Map<String, Value> = record_map
                .iter()
                .filter(|(_, v)| !v.is_object() && !v.is_array())
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            readable_record(&Value::Object(scalar_only), lang)
                + if lang == "en" {
                    "; audit.json#/summary/check_coverage; check identifiers listed below"
                } else {
                    "; audit.json#/summary/check_coverage; идентификаторы проверок перечислены ниже"
                }
        } else if record.is_some_and(|r| r.is_object() || r.is_array()) {
            coverage_source_details(section, record.unwrap_or(&Value::Null), coverage, lang)
        } else {
            str_value(record.unwrap_or(&Value::Null))
        };
        let reference = reference_text(evidence.get("source_ref"), lang);
        if !reference.is_empty() {
            let ref_label = if lang == "en" {
                "Source reference"
            } else {
                "Ссылка на источник"
            };
            details = if details.is_empty() {
                format!("{ref_label}: {reference}")
            } else {
                format!("{details}; {ref_label}: {reference}")
            };
        }
        rows.push(vec![
            Value::from(l(lang, label_key)),
            Value::from(match state {
                Some(s) => state_label(&Value::from(s), lang),
                None => l(lang, "not_reported").to_owned(),
            }),
            Value::from(details),
        ]);
    }

    let groups = mapping(coverage.get("groups"));
    for (name, value) in &groups {
        let group = mapping(Some(value));
        let label = labels(lang)
            .get(format!("group_{name}").as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| key_title(&Value::from(name.as_str()), lang));
        let source_count = group.get("source_count").cloned().unwrap_or(Value::Null);
        let projected_count = group.get("projected_count").cloned().unwrap_or(Value::Null);
        let details = format!(
            "{}: {}; {}: {}",
            l(lang, "group_source_count"),
            count_text(&source_count, lang),
            l(lang, "group_projected_count"),
            count_text(&projected_count, lang)
        );
        let state = group.get("state").and_then(Value::as_str);
        rows.push(vec![
            Value::from(label),
            Value::from(match state {
                Some(s) => state_label(&Value::from(s), lang),
                None => l(lang, "not_reported").to_owned(),
            }),
            Value::from(details),
        ]);
    }

    for value in coverage
        .get("checks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let check = mapping(Some(&value));
        let record_value = check.get("record").cloned().unwrap_or(Value::Null);
        let record = mapping(check.get("record"));
        let display = mapping(check.get("display"));
        let source_ref = mapping(check.get("source_ref"));
        let key = [
            check.get("id"),
            display.get("title"),
            record.get("check"),
            record.get("id"),
            record.get("tool"),
            record.get("name"),
            source_ref.get("id"),
            source_ref.get("pointer"),
        ]
        .iter()
        .copied()
        .flatten()
        .find(|v| !v.is_null() && v.as_str() != Some(""))
        .cloned()
        .unwrap_or_else(|| Value::from(l(lang, "check")));
        let mut raw_state = check.get("state").cloned().unwrap_or(Value::Null);
        let record_state = record.get("state").cloned().unwrap_or(Value::Null);
        if (raw_state.is_null()
            || raw_state.as_str() == Some("unreported")
            || raw_state.as_str() == Some("source_reported"))
            && record_state.is_string()
        {
            raw_state = record_state;
        }
        let state = if raw_state.is_null() {
            l(lang, "not_reported").to_owned()
        } else {
            state_label(&raw_state, lang)
        };
        let mut reason = [
            check.get("reason"),
            record.get("reason"),
            record.get("error"),
        ]
        .iter()
        .copied()
        .flatten()
        .find(|v| !v.is_null() && v.as_str() != Some(""))
        .cloned()
        .unwrap_or(Value::Null);
        reason = localize_generated_text(&reason, lang, None);
        let detail_fields: Map<String, Value> = record
            .iter()
            .filter(|(field, item)| {
                !["id", "name", "check", "tool", "state", "reason", "error"]
                    .contains(&field.as_str())
                    && (!compact || !(item.is_object() || item.is_array()))
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if !detail_fields.is_empty() {
            let mut detail_text = readable_record(&Value::Object(detail_fields), lang);
            if record.get("failed_pages").is_some_and(|v| !v.is_null())
                && record.get("pages_checked").is_some_and(|v| !v.is_null())
            {
                let failed = count_text(record.get("failed_pages").unwrap_or(&Value::Null), lang);
                let checked = count_text(record.get("pages_checked").unwrap_or(&Value::Null), lang);
                detail_text = if lang == "en" {
                    format!("Errors on {failed} of {checked} pages")
                } else {
                    format!("Ошибки на {failed} из {checked} страниц")
                };
            }
            reason = if reason.is_null() || reason.as_str() == Some("") {
                Value::from(detail_text)
            } else {
                Value::from(format!("{}; {detail_text}", str_value(&reason)))
            };
        } else if record_value.is_string() && (reason.is_null() || reason.as_str() == Some("")) {
            reason = record_value.clone();
        }
        let reference = reference_text(check.get("source_ref"), lang);
        let mut reason_text_value = str_value(&reason);
        if !reference.is_empty() {
            let ref_label = if lang == "en" {
                "Source reference"
            } else {
                "Ссылка на источник"
            };
            reason_text_value = if reason_text_value.is_empty() {
                format!("{ref_label}: {reference}")
            } else {
                format!("{reason_text_value}; {ref_label}: {reference}")
            };
        }
        rows.push(vec![
            key,
            Value::from(state),
            Value::from(reason_text_value),
        ]);
    }
    rows
}

fn backlog_rows(backlog: &Map<String, Value>, lang: &str) -> Vec<Vec<Value>> {
    let mut rows = Vec::new();
    for value in backlog
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let wrapper = mapping(Some(&value));
        let item = wrapper
            .get("record")
            .filter(|r| r.is_object())
            .cloned()
            .map(|r| mapping(Some(&r)))
            .unwrap_or_else(|| wrapper.clone());
        let source_ref = mapping(wrapper.get("source_ref"));
        let reference = [
            source_ref.get("id"),
            source_ref.get("pointer"),
            item.get("id"),
        ]
        .iter()
        .copied()
        .flatten()
        .find(|v| !v.is_null() && v.as_str() != Some(""))
        .cloned()
        .unwrap_or_else(|| Value::from(l(lang, "not_reported")));
        let title = [item.get("title"), item.get("name"), item.get("id")]
            .iter()
            .copied()
            .flatten()
            .find(|v| !v.is_null() && v.as_str() != Some(""))
            .cloned()
            .unwrap_or_else(|| Value::from(l(lang, "item")));
        let raw_state = [
            item.get("state"),
            item.get("attempt_status"),
            item.get("status"),
        ]
        .iter()
        .copied()
        .flatten()
        .find(|v| !v.is_null() && v.as_str() != Some(""))
        .cloned()
        .unwrap_or(Value::Null);
        let raw_verification = [item.get("verification"), item.get("verification_status")]
            .iter()
            .copied()
            .flatten()
            .find(|v| !v.is_null() && v.as_str() != Some(""))
            .cloned()
            .unwrap_or(Value::Null);
        let state = if raw_state.is_null() {
            l(lang, "not_reported").to_owned()
        } else {
            state_label(&raw_state, lang)
        };
        let verification = if raw_verification.is_null() {
            l(lang, "not_reported").to_owned()
        } else {
            state_label(&raw_verification, lang)
        };
        let reason = [item.get("reason"), item.get("blocked_by")]
            .iter()
            .copied()
            .flatten()
            .find(|v| !v.is_null() && v.as_str() != Some(""))
            .cloned()
            .unwrap_or(Value::Null);
        rows.push(vec![
            reference,
            title,
            Value::from(state),
            Value::from(verification),
            reason,
        ]);
    }
    rows
}

fn omission_rows(omissions: Option<&Value>) -> Vec<Vec<Value>> {
    omissions
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|value| {
            let omission = mapping(Some(value));
            vec![
                omission
                    .get("collection")
                    .or_else(|| omission.get("field"))
                    .cloned()
                    .unwrap_or(Value::Null),
                omission.get("count").cloned().unwrap_or(Value::Null),
                omission.get("reason").cloned().unwrap_or(Value::Null),
            ]
        })
        .collect()
}

fn overview_notice(model: &Map<String, Value>, lang: &str) -> String {
    let projection = mapping(model.get("projection"));
    if projection.get("policy").and_then(Value::as_str) != Some("overview-v1") {
        return String::new();
    }
    let title = if lang == "en" {
        "PDF overview — not an exhaustive listing"
    } else {
        "Обзор PDF — не полный перечень"
    };
    let explanation = if lang == "en" {
        "The PDF displays an ordered prefix, not a representative sample. Summary counts and charts describe the full saved audit. All check states and reasons are shown; nested metadata and prerequisites remain in complete audit.json. All records remain in the complete JSON and CSV companions."
    } else {
        "PDF показывает начало сохранённого списка, а не репрезентативную выборку. Счётчики и графики относятся ко всему сохранённому аудиту. Полные записи находятся в приложенных JSON и CSV."
    };
    let headers: Vec<String> = if lang == "en" {
        [
            "Collection",
            "Full source",
            "Displayed",
            "Omitted from PDF",
            "Display limits",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    } else {
        [
            "Коллекция",
            "Полный источник",
            "В PDF",
            "Не показано",
            "Лимиты обзора",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    };
    let (row_label, byte_label, page_label) = if lang == "en" {
        ("rows", "bytes", "pages")
    } else {
        ("строк", "байт", "страниц")
    };
    let mut rows = Vec::new();
    for (name, item) in mapping(projection.get("collections")) {
        let item_map = mapping(Some(&item));
        rows.push(vec![
            Value::from(if lang == "en" {
                if name == "findings" {
                    "Findings"
                } else {
                    "Pages"
                }
            } else if name == "findings" {
                "Находки"
            } else {
                "Страницы"
            }),
            item_map.get("source").cloned().unwrap_or(Value::Null),
            item_map.get("displayed").cloned().unwrap_or(Value::Null),
            item_map.get("omitted").cloned().unwrap_or(Value::Null),
            Value::from(format!(
                "{} {}; {} {}",
                str_value(item_map.get("max_rows").unwrap_or(&Value::Null)),
                row_label,
                str_value(item_map.get("max_bytes").unwrap_or(&Value::Null)),
                byte_label
            )),
        ]);
    }
    let links = model
        .get("artifacts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|name| {
            format!(
                "<a href=\"{}{}\">{}</a>",
                crate::pdf_stream::ARTIFACT_LINK_PREFIX,
                escape_value(name),
                escape_value(name)
            )
        })
        .collect::<Vec<_>>()
        .join(" · ");
    let limit_label = if lang == "en" {
        "PDF limit"
    } else {
        "Лимит PDF"
    };
    format!(
        "<aside class=\"notes\"><h3>{}</h3><p>{}</p>{}<p>{limit_label}: {} {} / {} {}.</p><p>{links}</p></aside>",
        escape_html(title),
        escape_html(explanation),
        table(&headers, &rows, lang),
        crate::pdf_validation::DEFAULT_PAGE_LIMIT,
        page_label,
        crate::pdf_validation::DEFAULT_BYTE_LIMIT,
        byte_label
    )
}

fn styles(brand: &Brand, lang: &str) -> String {
    let footer = l(lang, "report_footer")
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let ink = &brand.ink;
    let accent = &brand.accent;
    let font_stack = &brand.font_stack;
    let table_header = &brand.table_header;
    let card = &brand.card;
    let negative = &brand.negative;
    format!(
        r#"
@page {{
  size: 13.333in 7.5in;
  margin: 15mm 17mm 18mm;
  @bottom-left {{ content: "{footer}"; color: {ink}; font: 8pt {font_stack}; }}
  @bottom-right {{ content: counter(page); color: {accent}; font: 8pt {font_stack}; }}
}}
* {{ box-sizing: border-box; }}
html {{ color: {ink}; font-family: {font_stack}; font-size: 10pt; -webkit-print-color-adjust: exact; print-color-adjust: exact; }}
body {{ margin: 0; line-height: 1.45; }}
header {{ border-bottom: 1px solid {table_header}; display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 0 0 8px; margin: 0 0 18px; color: {ink}; }}
.brand {{ display: flex; align-items: center; gap: 10px; font-weight: 700; letter-spacing: .04em; }}
.brand-mark {{ display: inline-flex; align-items: end; gap: 2px; height: 16px; }}
.brand-mark i {{ display: block; width: 4px; border-radius: 2px; background: {accent}; }}
.brand-mark i:nth-child(1) {{ height: 7px; }} .brand-mark i:nth-child(2) {{ height: 11px; }} .brand-mark i:nth-child(3) {{ height: 16px; }}
.header-site {{ color: #64748B; font-size: 9pt; text-align: right; overflow-wrap: anywhere; }}
h1 {{ font-size: 26pt; line-height: 1.08; letter-spacing: -.02em; margin: 5px 0 4px; }}
h2 {{ font-size: 19pt; line-height: 1.15; margin: 0; }} h3 {{ font-size: 12pt; line-height: 1.25; margin: 2px 0 7px; }}
p {{ margin: 5px 0; }} .subtitle {{ color: #64748B; font-size: 11pt; margin: 0 0 16px; }}
.kicker {{ color: {accent}; font-size: 8pt; font-weight: 700; letter-spacing: .1em; margin: 0 0 4px; text-transform: uppercase; }}
.run-state {{ border-left: 4px solid {accent}; background: {card}; padding: 10px 13px; margin: 10px 0 14px; break-inside: avoid; }}
.state-partial,.state-failed,.state-unknown {{ border-left-color: {negative}; background: #FFF7F4; }}
.state-label {{ font-weight: 700; font-size: 10pt; }} .state-note {{ font-weight: 600; }}
.run-state ul {{ margin: 5px 0 0; padding-left: 18px; }}
.metric-grid {{ display: grid; grid-template-columns: repeat(4,1fr); gap: 9px; margin: 14px 0; }}
.metric-card {{ min-height: 72px; border: 1px solid {table_header}; border-top: 3px solid {accent}; background: {card}; padding: 9px 11px; break-inside: avoid; }}
.metric-card span,.metric-card small {{ display: block; color: #64748B; font-size: 8pt; }} .metric-card strong {{ display:block; font-size: 20pt; line-height: 1.15; margin: 4px 0; }} .metric-card small {{ min-height: 10px; }}
.chart-grid {{ display: grid; grid-template-columns: 1fr 1fr; gap: 12px; margin: 12px 0; }}
.chart-card {{ border: 1px solid {table_header}; border-radius: 5px; padding: 8px 10px; margin: 0; break-inside: avoid; }}
.chart-card figcaption {{ font-size: 9pt; font-weight: 700; margin-bottom: 4px; }} .chart-card svg {{ display:block; width:100%; height:auto; max-height: 155px; }}
.chart-legend {{ display:flex; flex-wrap:wrap; gap: 4px 12px; list-style:none; padding:0; margin:0; font-size:7pt; color:#475569; }} .chart-legend li {{ display:flex; gap:5px; align-items:center; }} .chart-legend i {{ display:inline-block; width:7px; height:7px; border-radius:50%; }}
.chapter {{ display:flex; align-items:center; gap:11px; border-bottom:1px solid {table_header}; padding:0 0 10px; margin:0 0 11px; break-after:avoid; }}
.chapter-number {{ display:grid; place-items:center; flex:none; width:34px; height:34px; border-radius:50%; background:{accent}; color:#fff; font-weight:700; font-size:10pt; }}
.chapter h2 {{ font-size:17pt; }}
.section {{ margin: 0 0 14px; }} .section-start {{ break-before: page; }}
.finding-card {{ border:1px solid {table_header}; border-left:4px solid {accent}; border-radius:4px; padding:9px 11px; margin:0 0 8px; break-inside:avoid; overflow-wrap:anywhere; }}
.finding-card.severity-critical {{ border-left-color:{negative}; }} .finding-card.severity-warning {{ border-left-color:#E58A13; }}
.finding-heading {{ display:flex; gap:10px; align-items:flex-start; }} .finding-number {{ font-size:16pt; line-height:1; color:{accent}; font-weight:700; }}
.finding-meta {{ color:#64748B; font-size:7.5pt; font-weight:700; letter-spacing:.04em; margin:0; text-transform:uppercase; }}
.url {{ color:#475569; overflow-wrap:anywhere; }}
.table-wrap {{ width:100%; overflow:visible; }} table {{ width:100%; border-collapse:collapse; table-layout:fixed; font-size:8pt; }}
thead {{ display:table-header-group; }} tr {{ break-inside:avoid; }} th {{ text-align:left; color:{ink}; background:{table_header}; font-size:7.5pt; padding:7px 8px; }}
td {{ border-bottom:1px solid #E8EDF4; padding:7px 8px; vertical-align:top; overflow-wrap:anywhere; }} tr:nth-child(even) td {{ background:#FAFBFD; }}
.empty {{ color:#64748B; font-style:italic; padding:8px 0; }} .notes {{ border-left:3px solid #E58A13; background:#FFF9ED; padding:8px 11px; break-inside:avoid; }}
.muted {{ color:#64748B; }} .identity {{ font-size:10pt; margin-bottom:10px; }}
"#
    )
}

/// Render one versioned audit PDF model into offline, print-ready localized HTML.
pub fn render_audit_pdf_html(
    model: &Value,
    lang: &str,
    brand: Option<&Value>,
) -> Result<String, String> {
    if !["en", "ru"].contains(&lang) {
        return Err(format!(
            "unsupported report language {lang:?}; expected one of (\"en\", \"ru\")"
        ));
    }
    if !model.is_object() || model.get("schema").and_then(Value::as_str) != Some(MODEL_SCHEMA) {
        return Err(format!(
            "audit PDF model must declare schema '{MODEL_SCHEMA}'"
        ));
    }

    let model_map = mapping(Some(model));
    let source = mapping(model.get("source"));
    let run = mapping(model.get("run"));
    let summary = mapping(model.get("summary"));
    let coverage = mapping(model.get("coverage"));
    let backlog = mapping(model.get("backlog"));
    let findings = model
        .get("findings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let pages = model
        .get("pages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let loaded_brand = load_brand(brand)?;
    let title = l(lang, "report");
    let domain = source
        .get("domain")
        .filter(|v| !v.is_null() && v.as_str() != Some(""))
        .or_else(|| source.get("url"))
        .cloned()
        .unwrap_or_else(|| Value::from(l(lang, "not_reported")));
    let generated = source.get("generated_at").cloned().unwrap_or(Value::Null);
    let source_kind = source.get("kind").cloned();
    let mut source_label = source_kind
        .as_ref()
        .and_then(Value::as_str)
        .and_then(|k| labels(lang).get(format!("source_{}", k.replace('-', "_")).as_str()))
        .map(|s| s.to_string());
    if source_kind.as_ref().and_then(Value::as_str) == Some("sf-audit") {
        let collector = source.get("collector").and_then(Value::as_str);
        if collector == Some("spiderhead.crawl") {
            source_label = Some(l(lang, "source_native").to_owned());
        } else if matches!(
            collector,
            Some("screaming-frog") | Some("screaming_frog") | Some("spiderhead.sf")
        ) || (collector.is_none()
            && source.get("input_mode").and_then(Value::as_str) == Some("exports"))
        {
            source_label = Some(l(lang, "source_screaming_frog").to_owned());
        }
    }
    let source_label = source_label
        .or_else(|| {
            source_kind
                .as_ref()
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .or_else(|| {
            source
                .get("schema")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| l(lang, "not_reported").to_owned());

    let head = format!(
        "<!doctype html><html lang=\"{lang}\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{} · {}</title><style>{}</style></head><body>",
        escape_html(title),
        escape_value(&domain),
        styles(&loaded_brand, lang)
    );
    let header = format!(
        "<header><div class=\"brand\"><span class=\"brand-mark\" aria-hidden=\"true\"><i></i><i></i><i></i></span><span>{}{}</span></div><div class=\"header-site\">{}</div></header>",
        escape_html(if loaded_brand.name.is_empty() { title } else { &loaded_brand.name }),
        if loaded_brand.logo_text.is_empty() {
            String::new()
        } else {
            format!(
                "<span class=\"muted\">{}</span>",
                escape_html(&loaded_brand.logo_text)
            )
        },
        escape_value(&domain)
    );

    let summary_source = mapping(summary.get("source"));
    let structured_summary_keys = [
        "findings_by_severity",
        "tools_run",
        "tools_failed",
        "page_tools_failed",
        "checks_disabled",
        "evidence_contract",
        "check_coverage",
        "project_coverage",
        "pages_checked",
        "urls_crawled",
        "findings_total",
        "issues_total",
    ];
    let mut summary_fields: Map<String, Value> = summary_source
        .iter()
        .filter(|(key, _)| !structured_summary_keys.contains(&key.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if model.get("projection").is_some_and(|v| !v.is_null()) {
        summary_fields = summary_fields
            .iter()
            .map(|(key, value)| {
                let value = if value.is_object() || value.is_array() {
                    Value::from(format!(
                        "Retained in full: audit.json#/summary/{}",
                        key.replace('~', "~0").replace('/', "~1")
                    ))
                } else {
                    value.clone()
                };
                (key.clone(), value)
            })
            .collect();
    }
    let mut summary_rows = field_rows(&Value::Object(summary_fields), lang);
    if let Some(diagnostics) = source
        .get("input_diagnostics")
        .filter(|v| crate::py::truthy(Some(v)))
    {
        summary_rows.push((
            l(lang, "input_diagnostics").to_owned(),
            readable_record(diagnostics, lang),
        ));
    }
    let summary_table = if summary_rows.is_empty() {
        String::new()
    } else {
        format!(
            "<section class=\"section\">{}</section>",
            table(
                &[
                    l(lang, "summary_field").to_owned(),
                    l(lang, "state_col").to_owned()
                ],
                &summary_rows
                    .iter()
                    .map(|(k, v)| vec![Value::from(k.clone()), Value::from(v.clone())])
                    .collect::<Vec<_>>(),
                lang
            )
        )
    };

    let generated_html = if generated.is_null() || generated.as_str() == Some("") {
        String::new()
    } else {
        format!(
            " · <b>{}:</b> {}",
            escape_html(l(lang, "prepared")),
            escape_value(&generated)
        )
    };
    let intro = format!(
        "<main><p class=\"kicker\">{}</p><h1>{}</h1><p class=\"subtitle\"><b>{}:</b> {}{}</p>{}{}{}{}{}</main>",
        escape_html(&source_label),
        escape_html(title),
        escape_html(l(lang, "audit_for")),
        escape_value(&domain),
        generated_html,
        overview_notice(&model_map, lang),
        status_block(&run, lang),
        summary_cards(&summary, &coverage, lang, model.get("projection").is_some_and(|v| !v.is_null())),
        format_args!(
            "<div class=\"chart-grid\">{}{}</div>",
            severity_chart(&summary, lang, &loaded_brand),
            checks_chart(&summary, &coverage, source.get("kind"), lang, &loaded_brand)
        ),
        summary_table
    );

    let coverage_state_label = if coverage.get("state").and_then(Value::as_str) == Some("reported")
    {
        l(lang, "coverage_reported").to_owned()
    } else if let Some(state) = coverage.get("state").filter(|v| !v.is_null()) {
        state_label(state, lang)
    } else {
        l(lang, "unavailable").to_owned()
    };
    let coverage_section = format!(
        "<section class=\"section section-start\"><div class=\"chapter\"><span class=\"chapter-number\">01</span><div><p class=\"kicker\">{}</p><h2>{}</h2></div></div><p class=\"muted\">{}</p>{}</section>",
        escape_html(l(lang, "source")),
        escape_html(l(lang, "coverage")),
        escape_html(&coverage_state_label),
        table(
            &[
                l(lang, "check").to_owned(),
                l(lang, "state_col").to_owned(),
                l(lang, "reason").to_owned()
            ],
            &coverage_rows(
                &coverage,
                lang,
                model.get("projection").is_some_and(|v| !v.is_null())
            ),
            lang
        )
    );

    let findings_count = count_value(&mapping(summary.get("counts")), "findings");
    let findings_section = format!(
        "<section class=\"section section-start\">{}<p class=\"muted\">{} · {}: {}</p>{}</section>",
        chapter("02", l(lang, "findings"), ""),
        escape_html(&count_text(&findings_count, lang)),
        escape_html(l(lang, "state_col")),
        escape_html(&state_label(run.get("state").unwrap_or(&Value::Null), lang)),
        finding_cards(&findings, lang)
    );

    let pages_section = format!(
        "<section class=\"section section-start\">{}{}</section>",
        chapter("03", l(lang, "pages"), ""),
        table(
            &[
                l(lang, "page_url").to_owned(),
                l(lang, "status_code").to_owned(),
                l(lang, "title").to_owned()
            ],
            &page_rows(&pages),
            lang
        )
    );

    let backlog_state = state_label(backlog.get("state").unwrap_or(&Value::Null), lang);
    let backlog_section = format!(
        "<section class=\"section section-start\">{}<p class=\"muted\">{}</p>{}</section>",
        chapter("04", l(lang, "backlog"), ""),
        escape_html(&backlog_state),
        table(
            &[
                l(lang, "reference").to_owned(),
                l(lang, "item").to_owned(),
                l(lang, "state_col").to_owned(),
                l(lang, "verification").to_owned(),
                l(lang, "reason").to_owned()
            ],
            &backlog_rows(&backlog, lang),
            lang
        )
    );

    let omissions = omission_rows(model.get("omissions"));
    let omissions_section = if omissions.is_empty() {
        String::new()
    } else {
        format!(
            "<section class=\"section\">{}{}</section>",
            chapter("05", l(lang, "omissions"), ""),
            table(
                &[
                    l(lang, "item").to_owned(),
                    l(lang, "count").to_owned(),
                    l(lang, "reason").to_owned()
                ],
                &omissions,
                lang
            )
        )
    };

    Ok(format!(
        "{head}{header}{intro}{coverage_section}{findings_section}{pages_section}{backlog_section}{omissions_section}</body></html>"
    ))
}

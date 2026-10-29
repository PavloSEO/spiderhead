//! Human labels for terminal presentation; persisted API identifiers stay unchanged.

use serde_json::Value;

use crate::localization::ui;
use crate::pyjson;

fn labels(key: &str) -> Option<&'static str> {
    Some(match key {
        "not_run" => "Not started",
        "not_agreed" => "Outside agreed scope",
        "pending_exclusion" => "Exclusion needs review",
        "not_applicable" => "Not applicable",
        "remaining" => "Ready to start",
        "review" => "Needs review",
        "deliverable" => "Deliverable needs review",
        "succeeded" => "Succeeded",
        "run" => "Evidence recorded",
        "running" => "In progress",
        "completed" => "Complete",
        "blocked" => "Blocked",
        "failed" => "Failed",
        "unavailable" => "Unavailable",
        "stale" => "Evidence needs refresh",
        "excluded" => "Excluded from scope",
        "proposed_goal" => "Proposed goal",
        "screaming_frog" => "Screaming Frog",
        "sf_exports" => "Supplied Screaming Frog exports",
        "sf_live" => "Live Screaming Frog crawl",
        "native" => "Native crawler",
        "schema" => "Structured data",
        "primary" => "Own site",
        "competitor" => "Competitor",
        "proposed_competitor" => "Suggested competitor",
        "task_ids" => "Linked tasks",
        "goal_id" => "Linked goal",
        "blocked_by" => "Blocked by",
        "attempt_status" => "Latest attempt",
        "execution_kind" => "Execution method",
        "definition_hash" => "Definition fingerprint",
        "config_fingerprint" => "Configuration fingerprint",
        _ => return None,
    })
}

/// `label`: `None`/empty → "Not recorded"; known key → UI label; else `s/_/ /`.capitalize().
pub fn label(value: &Value) -> String {
    if value.is_null() || value.as_str() == Some("") {
        return ui("Not recorded");
    }
    let text = match value {
        Value::String(s) => s.clone(),
        other => pyjson::py_str(other),
    };
    ui(&labels(&text)
        .map(str::to_string)
        .unwrap_or_else(|| capitalize(&text.replace('_', " "))))
}

/// `capitalize()`: first char upper, the rest lower.
fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
    }
}

/// `title`: slug-looking names expand to prose; strings with spaces stay verbatim.
pub fn title(value: &str) -> String {
    if value.contains(' ') {
        value.to_string()
    } else {
        capitalize(&value.replace(['-', '_'], " "))
    }
}

/// `value_lines`: present retained evidence without a second silent truncation.
pub fn value_lines(value: &Value, indent: &str) -> Vec<String> {
    match value {
        Value::Object(map) => {
            let mut lines = Vec::new();
            for (key, item) in map {
                if item.is_object() || item.is_array() {
                    lines.push(format!("{indent}{}:", label(&Value::String(key.clone()))));
                    lines.extend(value_lines(item, &format!("{indent}  ")));
                } else {
                    let shown = if matches!(
                        key.as_str(),
                        "state"
                            | "status"
                            | "attempt_status"
                            | "kind"
                            | "applicability"
                            | "execution_kind"
                    ) {
                        label(item)
                    } else {
                        match item {
                            Value::Null => "Not recorded".to_string(),
                            Value::String(s) => s.clone(),
                            other => pyjson::py_str(other),
                        }
                    };
                    lines.push(format!(
                        "{indent}{}: {shown}",
                        label(&Value::String(key.clone()))
                    ));
                }
            }
            if lines.is_empty() {
                vec![format!("{indent}No retained fields")]
            } else {
                lines
            }
        }
        Value::Array(items) => items
            .iter()
            .flat_map(|item| value_lines(item, &format!("{indent}• ")))
            .collect(),
        other => vec![format!(
            "{indent}{}",
            match other {
                Value::String(s) => s.clone(),
                v => pyjson::py_str(v),
            }
        )],
    }
}

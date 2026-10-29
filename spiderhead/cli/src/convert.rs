//! Value conversion for `type=` callables: what argparse's `_get_value` / `_check_value` do.
//!
//! Handlers receive typed values; this surface only needs the accept/reject decision and the
//! exact reference error text, so conversions that cannot fail (`_split_list`, the site lambdas)
//! keep the raw string.

use serde_json::Value;

use crate::pycompat::{is_py_float, is_py_int, repr};
use crate::pyjson;
use crate::tree::Action;

/// Convert one command-line string with the action's `type=`; `Err` is the message part of
/// argparse's `argument X: <message>`.
pub fn convert(action: &Action, text: &str) -> Result<Value, String> {
    match action.type_name.as_deref() {
        None => Ok(Value::String(text.to_owned())),
        Some("int") if is_py_int(text) => Ok(int_value(text)),
        Some("float") if is_py_float(text) => Ok(float_value(text)),
        Some(name @ ("int" | "float")) => Err(format!("invalid {name} value: {}", repr(text))),
        Some("_json_list") => json_list(text),
        Some("_mcp_http_port") => {
            if !is_py_int(text) {
                return Err(format!("invalid port: {}", repr(text)));
            }
            let n: i128 = text.trim().replace('_', "").parse().unwrap_or(i128::MAX);
            if (1..=65535).contains(&n) {
                Ok(Value::from(n as i64))
            } else {
                Err(format!("port must be 1-65535, got {n}"))
            }
        }
        Some(_) => Ok(Value::String(text.to_owned())),
    }
}

fn int_value(text: &str) -> Value {
    let clean = text.trim().replace('_', "");
    clean
        .strip_prefix('+')
        .unwrap_or(&clean)
        .parse::<i64>()
        .map_or_else(|_| Value::String(clean.clone()), Value::from)
}

fn float_value(text: &str) -> Value {
    text.trim()
        .replace('_', "")
        .parse::<f64>()
        .ok()
        .and_then(serde_json::Number::from_f64)
        .map_or(Value::Null, Value::Number)
}

fn json_list(text: &str) -> Result<Value, String> {
    if let Some(err) = pyjson::decode_error(text) {
        return Err(format!("not valid JSON: {err}"));
    }
    if !text.trim_start().starts_with('[') {
        return Err("expected a JSON list".to_owned());
    }
    Ok(serde_json::from_str(text).unwrap_or(Value::Array(Vec::new())))
}

/// `_check_value`: reject a converted value that is not among the action's choices.
pub fn check_choice(action: &Action, value: &Value) -> Result<(), String> {
    let (Some(choices), Some(strings)) = (&action.choices, action.choice_strings()) else {
        return Ok(());
    };
    if choices.contains(value) {
        return Ok(());
    }
    let shown = match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let list: Vec<String> = strings.iter().map(|c| repr(c)).collect();
    Err(format!(
        "invalid choice: {} (choose from {})",
        repr(&shown),
        list.join(", ")
    ))
}

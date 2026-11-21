//! Number parsing for command-line and environment values: the text is trimmed,
//! then handed to the standard integer and float parsers.

use serde_json::Value;

/// Parses an integer as a JSON number; `None` when the text is not an integer or does not fit 64 bits.
pub(super) fn parse_int(raw: &str) -> Option<Value> {
    let text = raw.trim();
    if let Ok(value) = text.parse::<i64>() {
        return Some(Value::from(value));
    }
    text.parse::<u64>().ok().map(Value::from)
}

/// Parses a float as a JSON number; `None` when the text is not a number, or for `inf`/`nan`, which a JSON number cannot represent.
pub(super) fn parse_float(raw: &str) -> Option<Value> {
    let parsed = raw.trim().parse::<f64>().ok()?;
    parsed.is_finite().then(|| serde_json::json!(parsed))
}

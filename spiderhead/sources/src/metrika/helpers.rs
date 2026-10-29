//! Value helpers that reproduce the Metrika client's JSON handling and error text.

use super::*;

/// `MissingCredential`/friends → `MetrikaFail`.
pub(super) fn credential_fail(err: CredentialError) -> MetrikaFail {
    match err {
        CredentialError::Missing(message) => MetrikaFail::MissingCredential(message),
        other => MetrikaFail::python(other.python_class(), other.to_string()),
    }
}

/// Retry pause: the service's `Retry-After` wins when it parses.
pub(super) fn backoff(attempt: i64, retry_after: Option<&str>) -> f64 {
    if let Some(raw) = retry_after {
        if !raw.is_empty() {
            if let Some(seconds) = pyjson::float_parse(raw) {
                if seconds > 0.0 {
                    return seconds.min(60.0);
                }
            }
        }
    }
    (2f64.powi((attempt - 1) as i32)).min(MAX_BACKOFF)
}

/// API error message: the `message` or `errors` field, else the truncated body text.
pub(super) fn api_message(text: &str) -> String {
    let body: Value = match serde_json::from_str(text) {
        Ok(body) => body,
        Err(_) => return char_slice(text, 300).unwrap_or_else(|| "empty response".to_owned()),
    };
    if let Value::Object(map) = &body {
        if pystr::truthy(map.get("message")) {
            return py_str(&map["message"]);
        }
        if let Some(errors) = map.get("errors").and_then(Value::as_array) {
            if let Some(first) = errors.first() {
                if let Value::Object(first) = first {
                    let message = first.get("message").cloned().unwrap_or(Value::Null);
                    if pystr::truthy(Some(&message)) {
                        return py_str(&message);
                    }
                    return pystr::repr_value(&Value::Object(first.clone()));
                }
                return py_str(first);
            }
        }
    }
    char_slice(text, 300).unwrap_or_default()
}

/// Detects the "query too complicated" refusal in either supported response language.
pub(crate) fn is_too_complicated(exc: &MetrikaError) -> bool {
    let message = exc.message.to_lowercase();
    COMPLEXITY_MARKERS.iter().any(|m| message.contains(m))
}

/// True for a JSON number that is not a boolean.
pub(crate) fn is_number(value: &Value) -> bool {
    value.is_number()
}

/// `_csv_ids` — a comma-separated id list, or a list that already arrived split.
pub(crate) fn csv_ids(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items.iter().map(py_str).collect(),
        _ => str_or_empty(value)
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect(),
    }
}

/// `_needs_today` — a relative date parameter needing a resolved "today".
pub(crate) fn needs_today(value: Option<&Value>) -> bool {
    match value.and_then(Value::as_str) {
        Some(text) => text == "today" || text == "yesterday" || days_ago(text).is_some(),
        None => false,
    }
}

/// `^(\d+)daysAgo$`; `\d` matches any Unicode decimal digit, and the count reads every decimal block.
/// The trailing `$` also matches before one final newline.
pub(super) fn days_ago(text: &str) -> Option<i64> {
    let stem = text.strip_suffix('\n').unwrap_or(text);
    let digits = stem.strip_suffix("daysAgo")?;
    if digits.is_empty() || !pystr::is_decimal(digits) {
        return None;
    }
    pystr::nd_int(digits)
}

/// `^([+-])(\d{2}):(\d{2})$`: the match only; bounds are not checked.
/// The trailing `$` also matches before one final newline.
pub(super) fn tz_offset_re(text: &str) -> Option<(i64, i64, i64)> {
    let text = text.strip_suffix('\n').unwrap_or(text);
    let chars: Vec<char> = text.chars().collect();
    if chars.len() != 6 || chars[3] != ':' {
        return None;
    }
    let sign = match chars[0] {
        '+' => 1i64,
        '-' => -1i64,
        _ => return None,
    };
    if ![1, 2, 4, 5]
        .iter()
        .all(|&i| pystr::is_decimal(&chars[i].to_string()))
    {
        return None;
    }
    let hours = pystr::nd_int(&chars[1..3].iter().collect::<String>())?;
    let minutes = pystr::nd_int(&chars[4..6].iter().collect::<String>())?;
    Some((sign, hours, minutes))
}

/// Object member lookup; a non-object receiver, including null, is an error.
pub(crate) fn py_get<'v>(value: &'v Value, key: &str) -> Result<Option<&'v Value>, MetrikaFail> {
    match value {
        Value::Object(map) => Ok(map.get(key)),
        other => Err(MetrikaFail::attribute_get(other)),
    }
}

/// Member lookup where a falsy receiver counts as an empty object; a truthy non-object is an error.
pub(crate) fn or_get<'v>(value: &'v Value, key: &str) -> Result<Option<&'v Value>, MetrikaFail> {
    match value {
        v if !pystr::truthy(Some(v)) => Ok(None),
        v => py_get(v, key),
    }
}

/// Iterate a value: array items, string characters, object keys; anything else is a type error.
pub(crate) fn py_iter(value: &Value) -> Result<Vec<Value>, MetrikaFail> {
    match value {
        Value::Array(items) => Ok(items.clone()),
        Value::String(text) => Ok(text.chars().map(|c| json!(c.to_string())).collect()),
        Value::Object(map) => Ok(map.keys().map(|k| json!(k)).collect()),
        other => Err(MetrikaFail::type_error(format!(
            "'{}' object is not iterable",
            pystr::type_name(other)
        ))),
    }
}

/// Index an array or string; an object key lookup or a non-indexable value is an error.
/// Callers check the index against the length first.
pub(crate) fn py_subscript(value: &Value, index: usize) -> Result<Value, MetrikaFail> {
    match value {
        Value::Array(items) => Ok(items[index].clone()),
        Value::String(text) => Ok(json!(text
            .chars()
            .nth(index)
            .unwrap_or_default()
            .to_string())),
        Value::Object(_) => Err(MetrikaFail::python("KeyError", index.to_string())),
        other => Err(MetrikaFail::type_error(format!(
            "'{}' object is not subscriptable",
            pystr::type_name(other)
        ))),
    }
}

/// Length of an array, string or object; anything else is a type error.
pub(crate) fn py_len(value: &Value) -> Result<i64, MetrikaFail> {
    match value {
        Value::Array(items) => Ok(items.len() as i64),
        Value::String(text) => Ok(text.chars().count() as i64),
        Value::Object(map) => Ok(map.len() as i64),
        other => Err(MetrikaFail::type_error(format!(
            "object of type '{}' has no len()",
            pystr::type_name(other)
        ))),
    }
}

/// Object conversion: an object copies, a falsy value becomes `{}`, pair sequences convert,
/// and anything else is a conversion error.
pub(crate) fn py_dict_from(value: Value) -> Result<Map<String, Value>, MetrikaFail> {
    if !pystr::truthy(Some(&value)) {
        return Ok(Map::new());
    }
    py_dict_from_pairs(value)
}

/// Object conversion of a truthy value, including pair sequences.
pub(crate) fn py_dict_from_pairs(value: Value) -> Result<Map<String, Value>, MetrikaFail> {
    match value {
        Value::Object(map) => Ok(map),
        Value::Array(items) => pairs_to_map(items),
        Value::String(text) => pairs_to_map(text.chars().map(|c| json!(c.to_string())).collect()),
        other => Err(MetrikaFail::type_error(format!(
            "'{}' object is not iterable",
            pystr::type_name(&other)
        ))),
    }
}

/// One conversion attempt over a pair sequence.
pub(super) fn pairs_to_map(items: Vec<Value>) -> Result<Map<String, Value>, MetrikaFail> {
    let mut out = Map::new();
    for (index, item) in items.iter().enumerate() {
        let pair: Vec<Value> = match item {
            Value::Array(pair) => pair.clone(),
            Value::String(text) => text.chars().map(|c| json!(c.to_string())).collect(),
            _ => {
                return Err(MetrikaFail::type_error(format!(
                    "cannot convert dictionary update sequence element #{index} to a sequence"
                )));
            }
        };
        if pair.len() != 2 {
            return Err(MetrikaFail::python(
                "ValueError",
                format!(
                    "dictionary update sequence element #{index} has length {}; 2 is required",
                    pair.len()
                ),
            ));
        }
        let key = match &pair[0] {
            Value::String(text) => text.clone(),
            Value::Array(_) | Value::Object(_) => {
                let name = pystr::type_name(&pair[0]);
                return Err(MetrikaFail::type_error(format!(
                    "cannot use '{name}' as a dict key (unhashable type: '{name}')"
                )));
            }
            other => py_str(other),
        };
        out.insert(key, pair[1].clone());
    }
    Ok(out)
}

/// Compare two numbers; a cross-type comparison is a type error.
pub(super) fn py_ge_i64(left: i64, right: &Value) -> Result<bool, MetrikaFail> {
    match right {
        Value::Bool(b) => Ok(left >= i64::from(*b)),
        Value::Number(n) => {
            let r = if let Some(i) = n.as_i64() {
                left >= i
            } else if let Some(u) = n.as_u64() {
                u64::try_from(left).map(|l| l >= u).unwrap_or(false)
            } else {
                (left as f64) >= n.as_f64().unwrap_or_default()
            };
            Ok(r)
        }
        other => Err(MetrikaFail::type_error(format!(
            "'>=' not supported between instances of 'int' and '{}'",
            pystr::type_name(other)
        ))),
    }
}

/// Equality for the scalar values that request-id and accuracy checks compare.
pub(crate) fn py_eq(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Number(a), Value::Number(b)) => {
            a.as_f64().unwrap_or_default() == b.as_f64().unwrap_or_default()
        }
        (Value::Bool(b), Value::Number(n)) | (Value::Number(n), Value::Bool(b)) => {
            n.as_f64().unwrap_or_default() == i64::from(*b) as f64
        }
        (Value::Array(a), Value::Array(b)) => a == b,
        (Value::Object(a), Value::Object(b)) => a == b,
        _ => false,
    }
}

/// Text form of a scalar: strings as-is, other values in their JSON-like text form.
pub(crate) fn py_str(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => pystr::repr_value(other),
    }
}

/// Text form of a value, where a falsy value becomes an empty string.
pub(super) fn str_or_empty(value: Option<&Value>) -> String {
    match value {
        Some(v) if pystr::truthy(Some(v)) => py_str(v),
        _ => String::new(),
    }
}

/// First 300 characters (code points) of the text; `None` when empty.
pub(super) fn char_slice(text: &str, limit: usize) -> Option<String> {
    let out: String = text.chars().take(limit).collect();
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Resolve a zone name against the system tz database.
pub(super) fn tz_offset_at(name: &str, unix_seconds: i64) -> Option<i64> {
    tz::tz_offset_at(name, unix_seconds)
}

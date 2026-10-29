//! Dynamic-typing helpers over `serde_json::Value`.
//!
//! The views index nested dicts freely (`snapshot["project"]["site"]`)
//! and rely on `None`/`{}` defaults. These helpers mirror that access pattern
//! without panicking; call sites that must not silently accept a missing key
//! use the plain getters and fall back to empty values as the views expect.

use serde_json::Value;

/// `value.get(key)` — `None` for non-objects, like a dict missing the key.
pub fn get<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.as_object().and_then(|m| m.get(key))
}

/// Nested `value[a][b][c]`; `None` if any level is absent or not an object.
pub fn path<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    let mut cur = value;
    for key in keys {
        cur = get(cur, key)?;
    }
    Some(cur)
}

/// Object lookup that yields the shared empty object instead of `None`.
pub fn obj<'a>(value: &'a Value, key: &str) -> &'a Value {
    static EMPTY: Value = Value::Null;
    get(value, key).unwrap_or(&EMPTY)
}

/// `obj[key]` as `&str` (`""` when absent or not a string).
pub fn str_of<'a>(value: &'a Value, key: &str) -> &'a str {
    get(value, key).and_then(Value::as_str).unwrap_or("")
}

/// `str(item)` for the TUI's one-level display values` of
/// str/int/float/bool/None; containers fall back to `""` — they never reach
/// the places that use this helper).
pub fn py_str(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(b) => (if *b { "True" } else { "False" }).into(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

/// `value[key]` as `i64`, accepting floats
/// not; non-numeric values yield `None`.
pub fn int_of(value: &Value, key: &str) -> Option<i64> {
    get(value, key).and_then(Value::as_i64)
}

/// `value[key]` as `f64` (ints coerce).
pub fn float_of(value: &Value, key: &str) -> Option<f64> {
    get(value, key).and_then(Value::as_f64)
}

/// `int(v)` under `isinstance(v, int)` — JSON booleans count as ints.
pub fn int_loose(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| value.as_bool().map(|b| b as i64))
}

/// `f"{value:,}"` — comma grouping, independent of the UI locale.
pub fn comma_number(value: i64) -> String {
    let negative = value < 0;
    let digits = value.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if negative {
        format!("-{out}")
    } else {
        out
    }
}

/// `repr(text)` — `!r`-style for the strings that appear inside log/status
/// fragments: single quotes preferred, double quotes when the text already
/// contains a single quote, the usual backslash escapes.
pub fn py_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `value[key]` as list (`&[]` when absent or not a list).
pub fn list_of<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    static EMPTY: [Value; 0] = [];
    get(value, key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&EMPTY)
}

/// `type(v) is int` — strict: JSON booleans do NOT count (use `int_loose`
/// where a plain integer test would be wrong).
pub fn is_int(value: &Value) -> bool {
    value.is_i64() || value.is_u64()
}

/// Truthiness of a JSON value the way a dynamic `if value:` reads it.
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `sorted(set(...))` of a list's items as strings where they are scalars.
pub fn str_set(items: &[Value]) -> Vec<String> {
    let mut out: Vec<String> = items
        .iter()
        .map(|v| match v {
            Value::String(s) => s.clone(),
            _ => py_str(v),
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The `a or b` rule over optional JSON lookups: a present-but-falsy value
/// (`null`, `""`, `0`, empty container) yields to the alternative.
pub trait OrPy<'a> {
    /// Keep `self` when truthy, else the alternative (or `self` when that is absent).
    fn or_py(self, alternative: impl FnOnce() -> Option<&'a Value>) -> Option<&'a Value>;
}

impl<'a> OrPy<'a> for Option<&'a Value> {
    fn or_py(self, alternative: impl FnOnce() -> Option<&'a Value>) -> Option<&'a Value> {
        match self {
            Some(v) if truthy(v) => Some(v),
            other => alternative().or(other),
        }
    }
}

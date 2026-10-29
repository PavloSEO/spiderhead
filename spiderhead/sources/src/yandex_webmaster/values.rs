//! Value helpers for the Webmaster adapter: JSON, user IDs, paging limits and query strings.

use super::*;

/// String form of a value: strings as-is; other values in their repr form.
pub(super) fn str_value(value: &Value) -> String {
    pystr::repr_value(value)
}

/// Parses JSON. Error messages keep the standard JSON wording.
pub(super) fn loads(text: &str) -> Result<Value, WmError> {
    serde_json::from_str(text).map_err(|_| {
        WmError::Value(pyjson::decode_error(text).unwrap_or_else(|| "JSON decode error".to_owned()))
    })
}

/// Resolves the user ID from `GET /user`, stored as a string.
pub(super) async fn resolve_user_id(
    call: &mut dyn WmCall,
    bearer: &Value,
) -> Result<String, WmError> {
    let text = call
        .send("GET", &format!("{HOST}/user"), None, bearer)
        .await?;
    let body = loads(&text)?;
    match &body {
        Value::Object(map) => match map.get("user_id") {
            Some(value) => Ok(str_value(value)),
            None => Err(WmError::Key(pystr::repr_str("user_id"))),
        },
        other => Err(WmError::subscript(other)),
    }
}

/// True only for integers; booleans and floats are excluded.
pub(super) fn is_strict_int(value: &Value) -> bool {
    matches!(value, Value::Number(n) if n.is_i64() || n.is_u64())
}

/// A numeric limit. Integers and floats are kept apart: floats compare fine but cannot index a slice.
#[derive(Debug, Clone, Copy)]
pub(super) enum PyNum {
    /// Integer value.
    Int(i64),
    /// Float value; valid in comparisons, invalid as a slice index.
    Float(f64),
}

impl PyNum {
    /// String form for query strings: `500`, `0.5`.
    pub(super) fn text(&self) -> String {
        match self {
            PyNum::Int(v) => v.to_string(),
            PyNum::Float(v) => pyjson::float_repr(*v),
        }
    }

    /// `self < other`.
    pub(super) fn lt(&self, other: i64) -> bool {
        match self {
            PyNum::Int(v) => *v < other,
            PyNum::Float(v) => *v < other as f64,
        }
    }

    /// Subtracts a length, keeping the integer or float type.
    pub(super) fn minus(&self, len: usize) -> PyNum {
        match self {
            PyNum::Int(v) => PyNum::Int(v - len as i64),
            PyNum::Float(v) => PyNum::Float(v - len as f64),
        }
    }

    /// `len < self`.
    pub(super) fn len_below(&self, len: usize) -> bool {
        match self {
            PyNum::Int(v) => (len as i64) < *v,
            PyNum::Float(v) => (len as f64) < *v,
        }
    }
}

/// Smaller of the page size and the remaining budget, keeping the smaller value's type.
pub(super) fn num_min(page_size: i64, budget: PyNum) -> PyNum {
    match budget {
        PyNum::Int(v) if v < page_size => PyNum::Int(v),
        PyNum::Float(v) if v < page_size as f64 => PyNum::Float(v),
        _ => PyNum::Int(page_size),
    }
}

/// `max_rows` as a raw operand: `None` applies the default and numbers keep their int/float type.
/// Other types fail with a `TypeError` from the subtraction in the page loop.
pub(super) fn rows_operand(value: Option<&Value>, default: i64) -> Result<PyNum, WmError> {
    match value {
        None => Ok(PyNum::Int(default)),
        Some(Value::Bool(b)) => Ok(PyNum::Int(i64::from(*b))),
        Some(Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                Ok(PyNum::Int(i))
            } else if let Some(u) = n.as_u64() {
                Ok(PyNum::Int(i64::try_from(u).unwrap_or(i64::MAX)))
            } else {
                Ok(PyNum::Float(n.as_f64().unwrap_or_default()))
            }
        }
        Some(other) => Err(WmError::type_error(format!(
            "unsupported operand type(s) for -: '{}' and 'int'",
            pystr::type_name(other)
        ))),
    }
}

/// Limit check for pagination; a non-comparable value is a type error.
pub(super) fn rows_below_one(value: Option<&Value>, default: i64) -> Result<bool, WmError> {
    match value {
        Some(other) if !matches!(other, Value::Bool(_) | Value::Number(_)) => {
            Err(WmError::type_error(format!(
                "'<' not supported between instances of '{}' and 'int'",
                pystr::type_name(other)
            )))
        }
        _ => Ok(rows_operand(value, default)?.lt(1)),
    }
}

/// Integer parse for a `count`-style string, saturating at the `i64` bounds. The value only feeds comparisons, where a saturated value gives the same result.
pub(super) fn int_str_saturate(text: &str) -> Option<i64> {
    pystr::int_str_saturating(text)
}

/// Validates one documented paged response and returns its rows and total.
pub(super) fn sample_page(page: &Value, list_key: &str) -> Result<(Vec<Value>, i64), WmError> {
    let map = match page {
        Value::Object(map) => map,
        _ => return Err(WmError::value_error("malformed Yandex Webmaster response")),
    };
    let chunk = match map.get(list_key) {
        Some(Value::Array(rows)) => rows.clone(),
        _ => {
            return Err(WmError::value_error(format!(
                "malformed Yandex Webmaster response: '{list_key}' must be a list"
            )))
        }
    };
    let count: Option<i64> = match map.get("count") {
        // A parse failure resets the count to `None`.
        Some(Value::String(text)) => int_str_saturate(text),
        Some(value) if is_strict_int(value) => value
            .as_i64()
            .or_else(|| value.as_u64().map(|u| u.min(i64::MAX as u64) as i64)),
        // Only strings are parsed; booleans, floats, null, lists and objects are rejected.
        _ => None,
    };
    match count {
        Some(v) if v >= 0 => Ok((chunk, v)),
        _ => Err(WmError::value_error(
            "malformed Yandex Webmaster response: 'count' must be a nonnegative integer",
        )),
    }
}

/// URL-encodes a query map in insertion order; array values repeat the key.
pub(super) fn wm_urlencode(query: &Map<String, Value>) -> String {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for (key, value) in query {
        match value {
            Value::Array(items) => {
                for item in items {
                    pairs.push((key.clone(), str_value(item)));
                }
            }
            other => pairs.push((key.clone(), str_value(other))),
        }
    }
    crate::http::urlencode(&pairs)
}

/// Returns a copy of `query` with `extra` values overwriting existing keys.
pub(super) fn merged(query: &Map<String, Value>, extra: &[(&str, Value)]) -> Map<String, Value> {
    let mut out = query.clone();
    for (key, value) in extra {
        out.insert(key.to_string(), value.clone());
    }
    out
}

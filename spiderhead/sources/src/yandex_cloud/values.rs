//! Value helpers for the Yandex Cloud client: normalization, JSON and base64.

use super::*;

/// String form of a value: strings as-is; other values in the repr form the output uses.
pub(super) fn str_value(value: &Value) -> String {
    pystr::repr_value(value)
}

/// Canonical form of a search phrase: trimmed, lowercased, `ё` folded to `е`, and whitespace runs collapsed to one space.
/// A truthy non-string is an error.
pub fn normalize(phrase: &Value) -> Result<String, CloudError> {
    let text = if pystr::truthy(Some(phrase)) {
        match phrase {
            Value::String(text) => text.as_str(),
            other => {
                return Err(CloudError::python(
                    "AttributeError",
                    format!(
                        "'{}' object has no attribute 'strip'",
                        pystr::type_name(other)
                    ),
                ))
            }
        }
    } else {
        ""
    };
    let folded = pystr::strip(text).to_lowercase().replace('ё', "е");
    // Every whitespace run becomes one space. The input is already trimmed, so no edge space can appear.
    let mut out = String::with_capacity(folded.len());
    let mut in_space = false;
    for ch in folded.chars() {
        if pystr::isspace(ch) {
            in_space = true;
        } else {
            if in_space && !out.is_empty() {
                out.push(' ');
            }
            in_space = false;
            out.push(ch);
        }
    }
    Ok(out)
}

/// Parse JSON; invalid JSON is kept as `{"raw": text}`.
pub(super) fn maybe_json(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| json!({"raw": raw}))
}

/// Short message (at most 500 chars) from a failed operation response.
pub(super) fn safe_operation_message(body: &Value) -> String {
    let rendered = match body {
        Value::Object(map) => {
            let candidate = map
                .get("message")
                .filter(|v| pystr::truthy(Some(v)))
                .or_else(|| map.get("error").filter(|v| pystr::truthy(Some(v))));
            match candidate {
                Some(value) => str_value(value),
                None => str_value(body),
            }
        }
        other => str_value(other),
    };
    rendered.chars().take(500).collect()
}

/// Items of a value as a list: array items, string characters or object keys; other types are a `TypeError`.
pub(super) fn py_list(value: &Value) -> Result<Vec<Value>, CloudError> {
    crate::metrika::py_iter(value).map_err(|err| CloudError::python("TypeError", err.to_string()))
}

/// Key for a result map: string keys as-is, other keys in their JSON spelling.
pub(super) fn dict_key(query: &Value) -> String {
    match query {
        Value::String(text) => text.clone(),
        other => pyjson::dumps(other),
    }
}

/// Lenient base64 decode. Characters outside the alphabet (including inner `=`) are skipped; trailing `=` is padding.
/// Malformed input returns the error class and message the output format expects.
pub(super) fn b64decode(text: &str) -> Result<Vec<u8>, CloudError> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    if !text.is_ascii() {
        return Err(CloudError::python(
            "ValueError",
            "string argument should contain only ASCII characters",
        ));
    }
    let bytes = text.as_bytes();
    let pads = bytes.iter().rev().take_while(|b| **b == b'=').count();
    let data: Vec<u8> = bytes[..bytes.len() - pads]
        .iter()
        .copied()
        .filter(|b| ALPHABET.contains(b))
        .collect();
    let remainder = data.len() % 4;
    if remainder == 1 {
        return Err(CloudError::python(
            "Error",
            format!(
                "Invalid base64-encoded string: number of data characters ({}) cannot be 1 more than a multiple of 4",
                data.len()
            ),
        ));
    }
    if remainder != 0 && pads < 4 - remainder {
        return Err(CloudError::python("Error", "Incorrect padding"));
    }
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    for chunk in data.chunks(4) {
        let bits: u32 = chunk
            .iter()
            .fold(0u32, |acc, b| (acc << 6) | sextet(*b) as u32)
            << (6 * (4 - chunk.len()) as u32);
        match chunk.len() {
            4 => out.extend_from_slice(&[(bits >> 16) as u8, (bits >> 8) as u8, bits as u8]),
            3 => out.extend_from_slice(&[(bits >> 16) as u8, (bits >> 8) as u8]),
            2 => out.push((bits >> 16) as u8),
            _ => {}
        }
    }
    Ok(out)
}

pub(super) fn sextet(byte: u8) -> u8 {
    match byte {
        b'A'..=b'Z' => byte - b'A',
        b'a'..=b'z' => byte - b'a' + 26,
        b'0'..=b'9' => byte - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => 0,
    }
}

/// Converts a number or boolean to `f64`; other types are a `TypeError`.
pub(super) fn as_float(value: &Value) -> Result<f64, CloudError> {
    match value {
        Value::Bool(b) => Ok(i64::from(*b) as f64),
        Value::Number(n) => Ok(n.as_f64().unwrap_or_default()),
        other => Err(CloudError::python(
            "TypeError",
            format!(
                "float() argument must be a string or a real number, not '{}'",
                pystr::type_name(other)
            ),
        )),
    }
}

/// Minimum seconds between requests, `1.0 / rps`. A zero rate is a division error.
pub(super) fn min_interval(rps: &Value) -> Result<f64, CloudError> {
    let divisor = as_float(rps).map_err(|_| {
        CloudError::python(
            "TypeError",
            format!(
                "unsupported operand type(s) for /: 'float' and '{}'",
                pystr::type_name(rps)
            ),
        )
    })?;
    if divisor == 0.0 {
        return Err(CloudError::python(
            "ZeroDivisionError",
            "float division by zero",
        ));
    }
    Ok(1.0 / divisor)
}

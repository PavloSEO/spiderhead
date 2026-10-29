//! String and value helpers for provider validation, matching the exact text rules providers use.

use serde_json::Value;

/// Whitespace per character: Unicode `White_Space` plus the separators `\x1c`–`\x1f`,
/// which count as whitespace but are not in Rust's `char::is_whitespace`.
pub fn isspace(ch: char) -> bool {
    ch.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&ch)
}

/// Non-empty and every character an ASCII digit. Non-ASCII decimal digits are excluded.
pub fn is_ascii_decimal(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

/// True when every character is ASCII.
pub fn is_ascii(text: &str) -> bool {
    text.is_ascii()
}

/// Truthiness of a JSON value: null, false, zero, the empty string, and empty arrays or objects are falsy.
pub fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                i != 0
            } else if let Some(u) = n.as_u64() {
                u != 0
            } else {
                n.as_f64().unwrap_or(0.0) != 0.0
            }
        }
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(m)) => !m.is_empty(),
    }
}

/// True for a JSON integer (either sign); floats and booleans are rejected.
pub fn is_py_int(value: &Value) -> bool {
    match value {
        Value::Number(n) => n.is_i64() || n.is_u64(),
        _ => false,
    }
}

/// Text of a value for error messages: `None`/`True`/`False` spelling, and single-quoted string
/// members inside containers.
pub fn repr_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => repr(other),
    }
}

/// Repr-style text of a JSON value: containers render their members with single-quoted strings,
/// for example `{'a': 1}`.
pub fn repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_owned(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_owned(),
        Value::Number(n) => crate::pyjson::float_repr_or_int(n),
        Value::String(s) => repr_str(s),
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(repr).collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Object(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", repr_str(k), repr(v)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

/// Sign-prefix helper used by numeric coercion checks.
pub fn as_strict_i64(value: Option<&Value>) -> Option<i64> {
    value.and_then(|v| {
        if is_py_int(v) {
            v.as_i64()
                .or_else(|| v.as_u64().and_then(|u| i64::try_from(u).ok()))
        } else {
            None
        }
    })
}

/// Split text into lines on every boundary that counts: `\n`, `\r`, `\r\n`, `\v`, `\f`, `\x1c`–`\x1e`,
/// `\x85`, `\u2028` and `\u2029`, which is more than Rust's `str::lines` splits on.
pub fn splitlines(text: &str) -> Vec<String> {
    const BREAKS: &[char] = &[
        '\n', '\r', '\u{b}', '\u{c}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{85}', '\u{2028}',
        '\u{2029}',
    ];
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if BREAKS.contains(&c) {
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            lines.push(std::mem::take(&mut current));
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// Strict UTF-8 decode. On failure returns the decoding error message text, which providers surface.
pub fn decode_utf8_strict(body: &[u8]) -> Result<String, String> {
    match std::str::from_utf8(body) {
        Ok(text) => Ok(text.to_owned()),
        Err(err) => {
            let position = err.valid_up_to();
            let byte = body.get(position).copied().unwrap_or_default();
            let reason = if err.error_len().is_some() {
                "invalid start byte"
            } else {
                "unexpected end of data"
            };
            Err(format!(
                "'utf-8' codec can't decode byte 0x{byte:02x} in position {position}: {reason}"
            ))
        }
    }
}

/// Trim whitespace (as defined above) from both ends.
pub fn strip(text: &str) -> &str {
    text.trim_matches(isspace)
}

/// Iterate a value: array items, string characters, object keys; any other value is a type error.
pub fn iterate(value: &Value) -> Result<Vec<Value>, (&'static str, String)> {
    match value {
        Value::Array(items) => Ok(items.clone()),
        Value::String(text) => Ok(text.chars().map(|c| Value::String(c.to_string())).collect()),
        Value::Object(map) => Ok(map.keys().map(|k| Value::String(k.clone())).collect()),
        other => Err((
            "TypeError",
            format!("'{}' object is not iterable", type_name(other)),
        )),
    }
}

/// Name of a JSON value's type, using the names providers use in error messages.
pub fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                "int"
            } else {
                "float"
            }
        }
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// Repr of a string: single quotes, or double quotes when the text contains a single quote and no double
/// quote; control characters are backslash-escaped.
pub fn repr_str(text: &str) -> String {
    let has_single = text.contains('\'');
    let has_double = text.contains('"');
    let quote = if has_single && !has_double { '"' } else { '\'' };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 32 || (c as u32) == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Integer coercion with the error class and message providers report.
/// Strings give `invalid literal for int() with base 10: {repr}`; unsupported types give the type error text;
/// NaN and infinity give a value error or an overflow error.
pub fn to_python_int(value: &Value) -> Result<i64, (&'static str, String)> {
    match value {
        Value::Bool(b) => Ok(if *b { 1 } else { 0 }),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i)
            } else if let Some(u) = n.as_u64() {
                i64::try_from(u).map_err(|_| {
                    (
                        "OverflowError",
                        "cannot convert float infinity to integer".to_owned(),
                    )
                })
            } else {
                let f = n.as_f64().unwrap_or(f64::NAN);
                if f.is_nan() {
                    Err((
                        "ValueError",
                        "cannot convert float NaN to integer".to_owned(),
                    ))
                } else if !f.is_finite() {
                    Err((
                        "OverflowError",
                        "cannot convert float infinity to integer".to_owned(),
                    ))
                } else {
                    Ok(f.trunc() as i64)
                }
            }
        }
        Value::String(s) => int_from_str(s).ok_or_else(|| {
            (
                "ValueError",
                format!("invalid literal for int() with base 10: {}", repr_str(s)),
            )
        }),
        other => Err((
            "TypeError",
            format!(
                "int() argument must be a string, a bytes-like object or a real number, not '{}'",
                type_name(other)
            ),
        )),
    }
}

/// The `int()` string grammar: `strip`, single underscores between `Nd` digits, an
/// optional sign, then nothing but `Nd` digits. Returns `(sign, digits)`.
fn int_grammar(text: &str) -> Option<(i64, String)> {
    let t = strip(text);
    if t.is_empty() {
        return None;
    }
    // Single underscores are allowed between digits, and "digit" means any Unicode `Nd` character, not only ASCII.
    let mut cleaned = String::with_capacity(t.len());
    let chars: Vec<char> = t.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if *c == '_' {
            let prev = i.checked_sub(1).map(|j| chars[j]);
            let next = chars.get(i + 1);
            let prev_ok = prev.and_then(nd_digit).is_some();
            let next_ok = next.and_then(|n| nd_digit(*n)).is_some();
            if !(prev_ok && next_ok) {
                return None;
            }
            continue;
        }
        cleaned.push(*c);
    }
    let (sign, digits) = match cleaned.strip_prefix('-') {
        Some(rest) => (-1i64, rest),
        None => (1i64, cleaned.strip_prefix('+').unwrap_or(&cleaned[..])),
    };
    if digits.is_empty() {
        return None;
    }
    Some((sign, digits.to_owned()))
}

fn int_from_str(text: &str) -> Option<i64> {
    let (sign, digits) = int_grammar(text)?;
    nd_int(&digits).map(|v| v * sign)
}

/// Integer value saturating at the `i64` bounds, for values that only feed comparisons; a saturated value
/// gives the same result for every `<` and `>=` check.
pub(crate) fn int_str_saturating(text: &str) -> Option<i64> {
    let (sign, digits) = int_grammar(text)?;
    let mut acc = 0i64;
    for ch in digits.chars() {
        acc = acc.saturating_mul(10).saturating_add(nd_digit(ch)? as i64);
    }
    Some(acc * sign)
}

/// Non-empty and every character in Unicode category `Nd`.
pub(crate) fn is_decimal(text: &str) -> bool {
    !text.is_empty()
        && text.chars().all(|c| {
            unicode_general_category::get_general_category(c)
                == unicode_general_category::GeneralCategory::DecimalNumber
        })
}

/// Every Unicode `Nd` block's zero — the value-0 members of each decimal block.
const ND_ZEROS: &[u32] = &[
    0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66,
    0xde6, 0xe50, 0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90,
    0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0, 0xff10,
    0x104a0, 0x10d30, 0x10d40, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450, 0x114d0,
    0x11650, 0x116c0, 0x116d0, 0x116da, 0x11730, 0x118e0, 0x11950, 0x11bf0, 0x11c50, 0x11d50,
    0x11da0, 0x11f50, 0x16130, 0x16a60, 0x16ac0, 0x16b50, 0x16d70, 0x1ccf0, 0x1d7ce, 0x1d7d8,
    0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0, 0x1e5f1, 0x1e950, 0x1fbf0,
];

/// One `Nd` char's digit value — every decimal block is a contiguous run of ten,
/// so `cp - block_zero` is the value; positions past `zero + 9` are not `Nd`.
pub(crate) fn nd_digit(c: char) -> Option<u32> {
    let cp = c as u32;
    let idx = ND_ZEROS.partition_point(|&zero| zero <= cp);
    if idx == 0 {
        return None;
    }
    let value = cp - ND_ZEROS[idx - 1];
    (value <= 9).then_some(value)
}

/// Integer value of a string already verified as decimal digits; values past `i64::MAX` give `None`.
pub(crate) fn nd_int(text: &str) -> Option<i64> {
    text.chars().try_fold(0i64, |acc, c| {
        let digit = nd_digit(c)? as i64;
        acc.checked_mul(10)?.checked_add(digit)
    })
}

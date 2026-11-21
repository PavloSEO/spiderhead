//! JSON serialization with a fixed, byte-stable layout for request bodies and journal lines.
//!
//! Provider request bodies and journal lines use a fixed layout that differs from
//! `serde_json::to_string` in three ways: `", "` / `": "` separators, ASCII-only output
//! (`\uXXXX` escapes, surrogate pairs beyond the BMP), and the shortest round-trip float layout
//! (`1e+16`, `1e-07`, `1.0`). Request bytes must match recorded provider requests exactly, so the
//! serializer reproduces that layout.

use serde_json::Value;

/// Serialize with insertion order, `", "`/`": "` separators and ASCII escaping.
pub fn dumps(value: &Value) -> String {
    let mut out = String::new();
    write_value(value, false, true, false, &mut out);
    out
}

/// Serialize with optional key sorting and optional ASCII escaping.
pub fn dumps_opts(value: &Value, sort_keys: bool, ensure_ascii: bool) -> String {
    let mut out = String::new();
    write_value(value, sort_keys, ensure_ascii, false, &mut out);
    out
}

/// Compact serialization (no spaces) with optional key sorting and ASCII escaping.
pub fn dumps_opts_compact(value: &Value, sort_keys: bool, ensure_ascii: bool) -> String {
    let mut out = String::new();
    write_value(value, sort_keys, ensure_ascii, true, &mut out);
    out
}

fn write_value(
    value: &Value,
    sort_keys: bool,
    ensure_ascii: bool,
    compact: bool,
    out: &mut String,
) {
    let (item_sep, key_sep) = if compact { (",", ":") } else { (", ", ": ") };
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Number(number) => out.push_str(&number_repr(number)),
        Value::String(text) => write_string(text, ensure_ascii, out),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(item_sep);
                }
                write_value(item, sort_keys, ensure_ascii, compact, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            let mut first = true;
            if sort_keys {
                let mut entries: Vec<(&String, &Value)> = map.iter().collect();
                entries.sort_by(|a, b| a.0.cmp(b.0));
                for (key, item) in entries {
                    if !first {
                        out.push_str(item_sep);
                    }
                    first = false;
                    write_string(key, ensure_ascii, out);
                    out.push_str(key_sep);
                    write_value(item, sort_keys, ensure_ascii, compact, out);
                }
            } else {
                for (key, item) in map.iter() {
                    if !first {
                        out.push_str(item_sep);
                    }
                    first = false;
                    write_string(key, ensure_ascii, out);
                    out.push_str(key_sep);
                    write_value(item, sort_keys, ensure_ascii, compact, out);
                }
            }
            out.push('}');
        }
    }
}

/// Encode one string as a JSON string literal, optionally ASCII-only.
fn write_string(text: &str, ensure_ascii: bool, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{09}' => out.push_str("\\t"),
            '\u{0a}' => out.push_str("\\n"),
            '\u{0c}' => out.push_str("\\f"),
            '\u{0d}' => out.push_str("\\r"),
            c if c < '\u{20}' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c if ensure_ascii && (c as u32) >= 0x7f => {
                let code = c as u32;
                if code > 0xffff {
                    // Characters beyond the BMP are written as UTF-16 surrogate pairs.
                    let v = code - 0x10000;
                    out.push_str(&format!(
                        "\\u{:04x}\\u{:04x}",
                        0xd800 + (v >> 10),
                        0xdc00 + (v & 0x3ff)
                    ));
                } else {
                    out.push_str(&format!("\\u{:04x}", code));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn number_repr(number: &serde_json::Number) -> String {
    if number.is_f64() {
        float_repr(number.as_f64().unwrap_or_default())
    } else {
        number.to_string()
    }
}

/// Shortest round-trip float text: positional notation for decimal exponents -4 to 15, and
/// `1e-07` / `1e+16` style beyond that. The same routine exists in the core canonical writer;
/// it is duplicated here because cross-crate edits are off-limits.
pub fn float_repr(value: f64) -> String {
    if !value.is_finite() {
        return if value.is_nan() {
            "NaN"
        } else if value > 0.0 {
            "Infinity"
        } else {
            "-Infinity"
        }
        .to_owned();
    }
    let sci = format!("{:e}", value.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let sign = if value.is_sign_negative() { "-" } else { "" };
    let body = if (-4..16).contains(&exp) {
        let point = exp + 1;
        if point <= 0 {
            format!("0.{}{digits}", "0".repeat(point.unsigned_abs() as usize))
        } else if digits.len() as i32 <= point {
            format!("{digits}{}.0", "0".repeat((point as usize) - digits.len()))
        } else {
            format!(
                "{}.{}",
                &digits[..point as usize],
                &digits[point as usize..]
            )
        }
    } else {
        let tail = if digits.len() > 1 {
            format!(".{}", &digits[1..])
        } else {
            String::new()
        };
        format!(
            "{}{tail}e{}{:02}",
            &digits[..1],
            if exp < 0 { '-' } else { '+' },
            exp.abs()
        )
    };
    format!("{sign}{body}")
}

/// Parse float text: trims ASCII whitespace, allows single underscores between digits, and accepts
/// inf, infinity and nan. Returns `None` where the text is not a number.
pub fn float_parse(text: &str) -> Option<f64> {
    let t = text.trim_matches(|c: char| {
        c.is_ascii_whitespace()
            || matches!(
                c,
                '\u{0b}' | '\u{85}' | '\u{a0}' | '\u{1680}' | '\u{2000}'
                    ..='\u{200a}'
                        | '\u{2028}'
                        | '\u{2029}'
                        | '\u{202f}'
                        | '\u{205f}'
                        | '\u{3000}'
                        | '\u{feff}'
            )
    });
    if t.is_empty() {
        return None;
    }
    if let Ok(v) = t.parse::<f64>() {
        return Some(v);
    }
    // Single underscores between digits are accepted: "1_000.5" is 1000.5.
    if !t.contains('_') {
        return None;
    }
    let cleaned: String = {
        let mut ok = true;
        let mut out = String::with_capacity(t.len());
        let chars: Vec<char> = t.chars().collect();
        for (i, c) in chars.iter().enumerate() {
            if *c == '_' {
                let prev = i.checked_sub(1).map(|j| chars[j]);
                let next = chars.get(i + 1);
                if !matches!(prev, Some(p) if p.is_ascii_digit())
                    || !matches!(next, Some(n) if n.is_ascii_digit())
                {
                    ok = false;
                    break;
                }
                continue;
            }
            out.push(*c);
        }
        if !ok {
            return None;
        }
        out
    };
    cleaned.parse::<f64>().ok()
}

/// Text of a JSON number: integers plain, floats via [`float_repr`].
pub fn float_repr_or_int(number: &serde_json::Number) -> String {
    if number.is_f64() {
        float_repr(number.as_f64().unwrap_or_default())
    } else {
        number.to_string()
    }
}

/// Text of a scalar value as providers pass it to form encoding and logs.
pub fn str_value(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(if *b { "True" } else { "False" }.to_owned()),
        Value::Null => Some("None".to_owned()),
        Value::Number(n) => Some(number_repr(n)),
        _ => None,
    }
}

/// Error text for a body that is not valid JSON.
///
/// Providers surface this text in error fields, so it names character (not byte) positions and uses the
/// current "Illegal trailing comma" wording. Returns `None` when the text is valid JSON.
pub fn decode_error(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    match scan_value(&chars, 0).and_then(|pos| {
        let pos = skip_ws(&chars, pos);
        if pos < chars.len() {
            Err(("Extra data", pos))
        } else {
            Ok(())
        }
    }) {
        Ok(()) => None,
        Err((msg, pos)) => Some(format!("{msg}: {}", line_col(&chars, pos))),
    }
}

fn line_col(chars: &[char], pos: usize) -> String {
    let line = chars[..pos.min(chars.len())]
        .iter()
        .filter(|c| **c == '\n')
        .count()
        + 1;
    let last_nl = chars[..pos.min(chars.len())]
        .iter()
        .rposition(|c| *c == '\n');
    // Column is the offset from the last newline before the error, or from the start when there is none.
    let col = match last_nl {
        Some(i) => pos - i,
        None => pos + 1,
    };
    format!("line {line} column {col} (char {pos})")
}

fn skip_ws(chars: &[char], mut pos: usize) -> usize {
    while pos < chars.len() && matches!(chars[pos], ' ' | '\t' | '\n' | '\r') {
        pos += 1;
    }
    pos
}

type Scan = Result<usize, (&'static str, usize)>;

fn scan_value(chars: &[char], pos: usize) -> Scan {
    let pos = skip_ws(chars, pos);
    let Some(&c) = chars.get(pos) else {
        return Err(("Expecting value", pos));
    };
    match c {
        '"' => scan_string(chars, pos),
        '{' => scan_object(chars, pos),
        '[' => scan_array(chars, pos),
        't' if chars[pos..].starts_with(&['t', 'r', 'u', 'e']) => Ok(pos + 4),
        'f' if chars[pos..].starts_with(&['f', 'a', 'l', 's', 'e']) => Ok(pos + 5),
        'n' if chars[pos..].starts_with(&['n', 'u', 'l', 'l']) => Ok(pos + 4),
        'N' if chars[pos..].starts_with(&['N', 'a', 'N']) => Ok(pos + 3),
        'I' if chars[pos..].starts_with(&['I', 'n', 'f', 'i', 'n', 'i', 't', 'y']) => Ok(pos + 8),
        '-' if chars[pos..].starts_with(&['-', 'I', 'n', 'f', 'i', 'n', 'i', 't', 'y']) => {
            Ok(pos + 9)
        }
        '-' | '0'..='9' => scan_number(chars, pos),
        _ => Err(("Expecting value", pos)),
    }
}

fn scan_number(chars: &[char], pos: usize) -> Scan {
    // Number grammar: optional minus, integer part without leading zeros, optional fraction and exponent.
    let mut i = pos;
    if chars.get(i) == Some(&'-') {
        i += 1;
    }
    let int_start = i;
    match chars.get(i) {
        Some('0') => i += 1,
        Some(c) if c.is_ascii_digit() => {
            while chars.get(i).is_some_and(|c| c.is_ascii_digit()) {
                i += 1;
            }
        }
        _ => return Err(("Expecting value", pos)),
    }
    if i == int_start {
        return Err(("Expecting value", pos));
    }
    if chars.get(i) == Some(&'.') && chars.get(i + 1).is_some_and(|c| c.is_ascii_digit()) {
        i += 2;
        while chars.get(i).is_some_and(|c| c.is_ascii_digit()) {
            i += 1;
        }
    }
    if matches!(chars.get(i), Some('e') | Some('E')) {
        let mut j = i + 1;
        if matches!(chars.get(j), Some('+') | Some('-')) {
            j += 1;
        }
        if chars.get(j).is_some_and(|c| c.is_ascii_digit()) {
            while chars.get(j).is_some_and(|c| c.is_ascii_digit()) {
                j += 1;
            }
            i = j;
        }
    }
    Ok(i)
}

fn scan_string(chars: &[char], pos: usize) -> Scan {
    let mut i = pos + 1;
    loop {
        let Some(&c) = chars.get(i) else {
            return Err(("Unterminated string starting at", pos));
        };
        match c {
            '"' => return Ok(i + 1),
            '\\' => {
                let Some(&e) = chars.get(i + 1) else {
                    return Err(("Unterminated string starting at", pos));
                };
                if e == 'u' {
                    let hex =
                        (2..6).all(|k| chars.get(i + k).is_some_and(|c| c.is_ascii_hexdigit()));
                    if !hex {
                        return Err(("Invalid \\uXXXX escape", i + 1));
                    }
                    i += 6;
                } else if matches!(e, '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't') {
                    i += 2;
                } else {
                    return Err(("Invalid \\escape", i));
                }
            }
            c if (c as u32) < 0x20 => return Err(("Invalid control character at", i)),
            _ => i += 1,
        }
    }
}

fn scan_object(chars: &[char], mut pos: usize) -> Scan {
    pos = skip_ws(chars, pos + 1);
    if chars.get(pos) == Some(&'}') {
        return Ok(pos + 1);
    }
    loop {
        if chars.get(pos) != Some(&'"') {
            return Err(("Expecting property name enclosed in double quotes", pos));
        }
        pos = skip_ws(chars, scan_string(chars, pos)?);
        if chars.get(pos) != Some(&':') {
            return Err(("Expecting ':' delimiter", pos));
        }
        pos = scan_value(chars, pos + 1)?;
        pos = skip_ws(chars, pos);
        match chars.get(pos) {
            Some('}') => return Ok(pos + 1),
            Some(',') => {
                let comma = pos;
                pos = skip_ws(chars, pos + 1);
                if chars.get(pos) == Some(&'}') {
                    return Err(("Illegal trailing comma before end of object", comma));
                }
            }
            _ => return Err(("Expecting ',' delimiter", pos)),
        }
    }
}

fn scan_array(chars: &[char], mut pos: usize) -> Scan {
    pos = skip_ws(chars, pos + 1);
    if chars.get(pos) == Some(&']') {
        return Ok(pos + 1);
    }
    loop {
        pos = scan_value(chars, pos)?;
        pos = skip_ws(chars, pos);
        match chars.get(pos) {
            Some(']') => return Ok(pos + 1),
            Some(',') => {
                let comma = pos;
                pos = skip_ws(chars, pos + 1);
                if chars.get(pos) == Some(&']') {
                    return Err(("Illegal trailing comma before end of array", comma));
                }
            }
            _ => return Err(("Expecting ',' delimiter", pos)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Defaults match the layout described at the top of this module.
    #[test]
    fn dumps_matches_reference_layout() {
        assert_eq!(
            dumps(&json!({"b": [1, true], "a": null, "f": 1.0, "s": "тест😀"})),
            r#"{"b": [1, true], "a": null, "f": 1.0, "s": "тест😀"}"#
                .replace("тест😀", "\\u0442\\u0435\\u0441\\u0442\\ud83d\\ude00")
        );
    }

    #[test]
    fn dumps_sorted_orders_keys() {
        assert_eq!(
            dumps_opts(&json!({"b": 1, "a": 2}), true, true),
            r#"{"a": 2, "b": 1}"#
        );
    }

    #[test]
    fn floats_follow_python_repr() {
        assert_eq!(float_repr(1.0), "1.0");
        assert_eq!(float_repr(1e16), "1e+16");
        assert_eq!(float_repr(1e-7), "1e-07");
        assert_eq!(float_repr(-0.5), "-0.5");
        assert_eq!(float_repr(0.0001), "0.0001");
        assert_eq!(float_repr(0.00001), "1e-05");
    }

    #[test]
    fn decode_error_matches_cpython_messages() {
        let cases: &[(&str, &str)] = &[
            ("", "Expecting value: line 1 column 1 (char 0)"),
            ("x", "Expecting value: line 1 column 1 (char 0)"),
            (
                "{",
                "Expecting property name enclosed in double quotes: line 1 column 2 (char 1)",
            ),
            ("[1,", "Expecting value: line 1 column 4 (char 3)"),
            (
                "{\"a\":1",
                "Expecting ',' delimiter: line 1 column 7 (char 6)",
            ),
            (
                "{\"a\" 1}",
                "Expecting ':' delimiter: line 1 column 6 (char 5)",
            ),
            (
                "\"abc",
                "Unterminated string starting at: line 1 column 1 (char 0)",
            ),
            ("\"a\\xzb\"", "Invalid \\escape: line 1 column 3 (char 2)"),
            ("{\"a\":1} x", "Extra data: line 1 column 9 (char 8)"),
            ("[1 2]", "Expecting ',' delimiter: line 1 column 4 (char 3)"),
            (
                "\"a\u{0008}b\"",
                "Invalid control character at: line 1 column 3 (char 2)",
            ),
            (
                "\"\\u12zz\"",
                "Invalid \\uXXXX escape: line 1 column 3 (char 2)",
            ),
            (
                "[1,]",
                "Illegal trailing comma before end of array: line 1 column 3 (char 2)",
            ),
            (
                "{\"a\":1,}",
                "Illegal trailing comma before end of object: line 1 column 7 (char 6)",
            ),
            (" \"x\" \n\n y", "Extra data: line 3 column 2 (char 8)"),
            ("01", "Extra data: line 1 column 2 (char 1)"),
            ("1.5e", "Extra data: line 1 column 4 (char 3)"),
            (
                "{\"a\"",
                "Expecting ':' delimiter: line 1 column 5 (char 4)",
            ),
            ("[", "Expecting value: line 1 column 2 (char 1)"),
            ("{\"a\":}", "Expecting value: line 1 column 6 (char 5)"),
        ];
        for (input, want) in cases {
            assert_eq!(
                decode_error(input).as_deref(),
                Some(*want),
                "input {input:?}"
            );
        }
        assert_eq!(decode_error("{\"a\": 1}"), None);
    }

    #[test]
    fn float_parse_accepts_python_forms() {
        assert_eq!(float_parse(" 1.5 "), Some(1.5));
        assert_eq!(float_parse("1e5"), Some(100000.0));
        assert_eq!(float_parse("1_000"), Some(1000.0));
        assert_eq!(float_parse(""), None);
        assert_eq!(float_parse("abc"), None);
        assert_eq!(float_parse("1__0"), None);
        assert_eq!(float_parse("_1"), None);
        assert_eq!(float_parse("Mon, 01 Jan 2024"), None);
    }
}

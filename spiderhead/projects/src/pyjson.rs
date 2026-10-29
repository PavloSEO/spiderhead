//! Byte-exact JSON writers for project documents.
//!
//! Project files are compared byte-for-byte, so the writers reproduce
//! the exact `json` layout: `indent=2` with sorted keys for manifests and
//! `(",", ":")` separators for JSONL rows. Floats use the shortest round-trip spelling.
//!
//! (`spiderhead_core::common` has a compact writer but its float spelling is private;
//! this module owns the indented layout as well.)

use serde_json::Value;

/// Compact JSON: sorted keys, no extra spaces, non-ASCII kept as UTF-8.
pub fn dumps_compact(value: &Value) -> String {
    let mut out = String::new();
    write_json(value, Mode::Compact, false, true, 0, &mut out);
    out
}

/// Canonical hashing spelling: sorted keys, compact separators, NaN and infinity rejected.
/// Non-ASCII text is `\uXXXX`-escaped.
pub fn dumps_compact_ascii(value: &Value) -> String {
    let mut out = String::new();
    write_json(value, Mode::Compact, true, true, 0, &mut out);
    out
}

/// Indented manifest layout: two-space indent, sorted keys, non-ASCII kept as UTF-8.
pub fn dumps_indent2(value: &Value) -> String {
    let mut out = String::new();
    write_json(value, Mode::Indent, false, true, 0, &mut out);
    out
}

/// Indented layout with keys kept in document order — `json.dumps(indent=2)` of a
/// caller-ordered document written verbatim (e.g. a collected result file).
pub fn dumps_indent2_ordered(value: &Value) -> String {
    let mut out = String::new();
    write_json(value, Mode::Indent, false, false, 0, &mut out);
    out
}

/// Sorted keys, non-ASCII kept as UTF-8, default separators.
pub fn dumps_default(value: &Value) -> String {
    let mut out = String::new();
    write_json(value, Mode::Default, false, true, 0, &mut out);
    out
}

#[derive(Clone, Copy)]
enum Mode {
    Compact,
    Default,
    Indent,
}

fn write_json(value: &Value, mode: Mode, ascii: bool, sort: bool, depth: usize, out: &mut String) {
    match value {
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            if sort {
                entries.sort_by(|a, b| a.0.cmp(b.0));
            }
            out.push('{');
            for (index, (key, item)) in entries.into_iter().enumerate() {
                if index > 0 {
                    match mode {
                        Mode::Compact => out.push(','),
                        Mode::Default => out.push_str(", "),
                        Mode::Indent => {
                            out.push(',');
                            out.push('\n');
                            out.push_str(&"  ".repeat(depth + 1));
                        }
                    }
                } else if matches!(mode, Mode::Indent) {
                    out.push('\n');
                    out.push_str(&"  ".repeat(depth + 1));
                }
                escape_string(key, ascii, out);
                out.push(':');
                if !matches!(mode, Mode::Compact) {
                    out.push(' ');
                }
                write_json(item, mode, ascii, sort, depth + 1, out);
            }
            if matches!(mode, Mode::Indent) {
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
            }
            out.push('}');
        }
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    match mode {
                        Mode::Compact => out.push(','),
                        Mode::Default => out.push_str(", "),
                        Mode::Indent => {
                            out.push(',');
                            out.push('\n');
                            out.push_str(&"  ".repeat(depth + 1));
                        }
                    }
                } else if matches!(mode, Mode::Indent) {
                    out.push('\n');
                    out.push_str(&"  ".repeat(depth + 1));
                }
                write_json(item, mode, ascii, sort, depth + 1, out);
            }
            if matches!(mode, Mode::Indent) {
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
            }
            out.push(']');
        }
        Value::Number(number) if number.is_f64() => {
            out.push_str(&python_float_repr(number.as_f64().unwrap_or_default()));
        }
        Value::String(text) => escape_string(text, ascii, out),
        other => out.push_str(&serde_json::to_string(other).unwrap_or_default()),
    }
}

/// JSON string escaping; `ascii` escapes all non-ASCII characters.
fn escape_string(text: &str, ascii: bool, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c if ascii && !c.is_ascii() => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Shortest round-trip float spelling for the values JSON output can contain.
///
/// Positional notation for decimal exponents -4..15,
/// `1e-07` / `1e+16` beyond. `serde_json` spells `1e-7` / `1e16`, which would change bytes.
/// (Non-finite values are rejected before this point, but the spelling is kept
/// for completeness.)
pub fn python_float_repr(value: f64) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn indent2_matches_reference_layout() {
        let value = json!({"b": [1, 2], "a": {"z": null, "y": "текст"}, "e": {}, "l": []});
        let text = dumps_indent2(&value);
        assert_eq!(
            text,
            "{\n  \"a\": {\n    \"y\": \"текст\",\n    \"z\": null\n  },\n  \"b\": [\n    1,\n    2\n  ],\n  \"e\": {},\n  \"l\": []\n}"
        );
    }

    #[test]
    fn compact_matches_reference_layout() {
        assert_eq!(
            dumps_compact(&json!({"b": 1.5, "a": null})),
            r#"{"a":null,"b":1.5}"#
        );
    }
}

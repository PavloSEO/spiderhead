//! Compact JSON text with the float and string rendering the audit format fixes.
//!
//! `serde_json` prints floats differently (`1e16` vs `1e+16`), and audit.v2 hashes the stored text,
//! so the float text must match the stored bytes exactly.

use serde_json::Value;
use std::fmt::Write;

/// Shortest round-trip float digits; exponent form outside `1e-4 <= x < 1e16`.
pub(crate) fn float_repr(value: f64) -> String {
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        }
        .into();
    }
    let sci = format!("{:e}", value.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let decpt = exp + 1;
    let mut out = String::new();
    if value < 0.0 {
        out.push('-');
    }
    if (-3..=16).contains(&decpt) {
        if decpt <= 0 {
            out.push_str("0.");
            out.push_str(&"0".repeat((-decpt) as usize));
            out.push_str(&digits);
        } else if digits.len() as i32 <= decpt {
            out.push_str(&digits);
            out.push_str(&"0".repeat((decpt - digits.len() as i32) as usize));
            out.push_str(".0");
        } else {
            out.push_str(&digits[..decpt as usize]);
            out.push('.');
            out.push_str(&digits[decpt as usize..]);
        }
    } else {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let _ = write!(out, "e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs());
    }
    out
}

fn string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write_value(out: &mut String, value: &Value, sort_keys: bool) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) => {
                let _ = write!(out, "{i}");
            }
            (_, Some(u), _) => {
                let _ = write!(out, "{u}");
            }
            (_, _, Some(f)) => out.push_str(&float_repr(f)),
            _ => out.push_str("null"),
        },
        Value::String(s) => string(out, s),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(out, item, sort_keys);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            if sort_keys {
                // Keys sort by code point; UTF-8 byte order is identical.
                entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
            }
            for (i, (key, item)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                string(out, key);
                out.push(':');
                write_value(out, item, sort_keys);
            }
            out.push('}');
        }
    }
}

/// Compact JSON text; keys are sorted when `sort_keys` is set.
pub(crate) fn dumps(value: &Value, sort_keys: bool) -> String {
    let mut out = String::new();
    write_value(&mut out, value, sort_keys);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_repr_matches_reference() {
        let table = [
            (1.0, "1.0"),
            (0.1, "0.1"),
            (1e16, "1e+16"),
            (1.5e16, "1.5e+16"),
            (123456789012345680.0, "1.2345678901234568e+17"),
            (1e15, "1000000000000000.0"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1.5e-7, "1.5e-07"),
            (-2.5, "-2.5"),
            (1e100, "1e+100"),
            (5e-324, "5e-324"),
        ];
        for (value, want) in table {
            assert_eq!(float_repr(value), want, "{value}");
        }
    }

    #[test]
    fn dumps_sorts_and_escapes_like_python() {
        let v: Value = serde_json::from_str(r#"{"b":[1,2.5],"a":"x\u0001\"é\u007f "}"#).unwrap();
        assert_eq!(
            dumps(&v, true),
            "{\"a\":\"x\\u0001\\\"é\u{7f}\u{2028}\",\"b\":[1,2.5]}"
        );
        assert_eq!(
            dumps(&v, false),
            "{\"b\":[1,2.5],\"a\":\"x\\u0001\\\"é\u{7f}\u{2028}\"}"
        );
    }
}

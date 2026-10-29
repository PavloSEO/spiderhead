//! Small shared helpers: whitespace collapsing and canonical JSON text.
//!
//! The timestamp helper lives in `timeutil`.

use serde_json::Value;

/// Collapse every whitespace run to one space and trim both ends.
pub fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Deterministic compact JSON (sorted keys, non-ASCII kept), for hashing.
///
/// Keys are sorted explicitly, because `serde_json` map order depends on enabled crate features.
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_json(value, ",", ":", &mut out);
    out
}

/// JSON text with sorted keys, `", "` and `": "` separators, and non-ASCII kept.
/// Used where a manifest hash must stay stable across releases.
pub fn python_json(value: &Value) -> String {
    let mut out = String::new();
    write_json(value, ", ", ": ", &mut out);
    out
}

/// JSON text with insertion-ordered keys and a two-space indent, non-ASCII kept.
/// This is the text form of tool results.
pub fn python_json_pretty(value: &Value) -> String {
    let mut out = String::new();
    write_pretty(value, 0, &mut out);
    out
}

/// JSON text with insertion-ordered keys and default separators.
/// This is the text of a tool error result.
pub fn python_json_insertion(value: &Value) -> String {
    let mut out = String::new();
    write_pretty(value, -1, &mut out);
    out
}

fn write_pretty(value: &Value, depth: i32, out: &mut String) {
    let pad = |levels: i32| {
        if levels < 0 {
            String::new()
        } else {
            "  ".repeat(levels as usize)
        }
    };
    let newline = |depth: i32| if depth < 0 { "" } else { "\n" };
    match value {
        Value::Object(map) if map.is_empty() => out.push_str("{}"),
        Value::Array(items) if items.is_empty() => out.push_str("[]"),
        Value::Object(map) => {
            out.push('{');
            if depth >= 0 {
                out.push('\n');
            }
            for (index, (key, item)) in map.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                    if depth >= 0 {
                        out.push('\n');
                    } else {
                        out.push(' ');
                    }
                }
                out.push_str(&pad(depth + 1));
                out.push_str(&serde_json::to_string(key).unwrap_or_default());
                out.push_str(": ");
                write_pretty(item, if depth < 0 { depth } else { depth + 1 }, out);
            }
            out.push_str(newline(depth));
            out.push_str(&pad(depth));
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            if depth >= 0 {
                out.push('\n');
            }
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                    if depth >= 0 {
                        out.push('\n');
                    } else {
                        out.push(' ');
                    }
                }
                out.push_str(&pad(depth + 1));
                write_pretty(item, if depth < 0 { depth } else { depth + 1 }, out);
            }
            out.push_str(newline(depth));
            out.push_str(&pad(depth));
            out.push(']');
        }
        Value::Number(number) if number.is_f64() => {
            out.push_str(&python_float_repr(number.as_f64().unwrap_or_default()));
        }
        other => out.push_str(&serde_json::to_string(other).unwrap_or_default()),
    }
}

fn write_json(value: &Value, item_sep: &str, key_sep: &str, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            out.push('{');
            for (index, (key, item)) in entries.into_iter().enumerate() {
                if index > 0 {
                    out.push_str(item_sep);
                }
                out.push_str(&serde_json::to_string(key).unwrap_or_default());
                out.push_str(key_sep);
                write_json(item, item_sep, key_sep, out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(item_sep);
                }
                write_json(item, item_sep, key_sep, out);
            }
            out.push(']');
        }
        Value::Number(number) if number.is_f64() => {
            out.push_str(&python_float_repr(number.as_f64().unwrap_or_default()));
        }
        other => out.push_str(&serde_json::to_string(other).unwrap_or_default()),
    }
}

/// reference `repr()` of a JSON value: `None`/`True`/`False`, quoted strings,
/// `{'k': v}`/`[x]` containers and shortest round-trip floats.
/// Error messages that embed caller input use this wording.
pub fn python_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_owned(),
        Value::Bool(flag) => if *flag { "True" } else { "False" }.to_owned(),
        Value::Number(number) if number.is_f64() => {
            python_float_repr(number.as_f64().unwrap_or_default())
        }
        Value::Number(number) => number.to_string(),
        Value::String(text) => python_str_repr(text),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(python_repr).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(key, item)| format!("{}: {}", python_str_repr(key), python_repr(item)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
    }
}

/// reference `repr()` of one string: single quotes by default, double quotes when
/// the text holds a single quote but no double quote; escapes the control
/// characters Creference escapes.
pub fn python_str_repr(text: &str) -> String {
    let single = !text.contains('\'') || text.contains('"');
    let quote = if single { '\'' } else { '"' };
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
            c if (c as u32) < 0x20 || (c as u32) == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// reference `str(exc)` of an `OSError`: `[Errno N] <strerror>` with
/// `: '<path>'` appended when the failing call knew its filename.
/// Falls back to the plain message when no OS code is attached.
pub fn python_os_error(error: &std::io::Error, path: Option<&std::path::Path>) -> String {
    let Some(code) = error.raw_os_error() else {
        return error.to_string();
    };
    let text = error.to_string();
    let text = text.trim_end_matches(&format!(" (os error {code})"));
    match path {
        Some(path) => format!(
            "[Errno {code}] {text}: {}",
            python_str_repr(&path.to_string_lossy())
        ),
        None => format!("[Errno {code}] {text}"),
    }
}

/// Float text in the shortest round-trip form. Exponents -4 to 15 are positional; others use exponent notation with a sign, e.g. `1e-07`, `1e+16`.
/// The default `serde_json` float text differs (`1e-7`), which would change existing hashes.
fn python_float_repr(value: f64) -> String {
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
    fn collapse_whitespace_joins_runs_and_trims() {
        assert_eq!(collapse_whitespace("  a \n\t b  "), "a b");
        assert_eq!(collapse_whitespace(""), "");
    }

    #[test]
    fn canonical_json_is_sorted_compact_and_keeps_unicode() {
        assert_eq!(
            canonical_json(&json!({"b": 1, "a": "тест"})),
            r#"{"a":"тест","b":1}"#
        );
    }

    // Default separators: the manifest fingerprint is hashed over this layout.
    #[test]
    fn python_json_uses_default_separators() {
        assert_eq!(
            python_json(&json!({"b": [1, true], "a": null})),
            r#"{"a": null, "b": [1, true]}"#
        );
    }
}

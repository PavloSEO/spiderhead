//! Flag value typing: what the reference `type=` callables accepted and what
//! they turned a command-line string into. Handlers receive typed values, so
//! an accepted `--limit 1_000` becomes a JSON number and a rejected one is a
//! usage error (exit code 2).

use serde_json::Value;

use crate::json_errors;

/// The conversion a flag's declaration declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Any string, kept as-is (also for the site lambdas and `_split_list`).
    Text,
    /// `int`: base-10 with digit separators, any size.
    Int,
    /// `float`: the reference float syntax including `inf`/`nan`.
    Float,
    /// `_json_list`: a JSON array literal.
    JsonList,
    /// `_mcp_http_port`: an int in 1..=65535.
    Port,
}

/// Convert one command-line string; `Err` carries the reference message part.
pub fn convert_kind(kind: Kind, text: &str) -> Result<Value, String> {
    match kind {
        Kind::Text => Ok(Value::String(text.to_owned())),
        Kind::Int if int_accepts(text) => Ok(int_value(text)),
        Kind::Float if float_accepts(text) => Ok(float_value(text)),
        Kind::Int | Kind::Float => Err(format!(
            "invalid {} value: {}",
            if kind == Kind::Int { "int" } else { "float" },
            repr(text)
        )),
        Kind::JsonList => json_list(text),
        Kind::Port => {
            if !int_accepts(text) {
                return Err(format!("invalid port: {}", repr(text)));
            }
            let n: i128 = text.trim().replace('_', "").parse().unwrap_or(i128::MAX);
            if (1..=65535).contains(&n) {
                Ok(Value::from(n as i64))
            } else {
                Err(format!("port must be 1-65535, got {n}"))
            }
        }
    }
}

/// `repr()` of a reference `str`, the quoting error messages show.
pub fn repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f || ((0x80..0xa0).contains(&(c as u32))) => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

fn digits_with_underscores(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('_')
        && !s.ends_with('_')
        && !s.contains("__")
        && s.chars().all(|c| c.is_ascii_digit() || c == '_')
}

/// Whether the reference `int(text)` accepts `text` (base 10, any size).
pub fn int_accepts(text: &str) -> bool {
    let t = text.trim();
    digits_with_underscores(t.strip_prefix(['+', '-']).unwrap_or(t))
}

/// Whether the reference `float(text)` accepts `text`.
pub fn float_accepts(text: &str) -> bool {
    let t = text.trim();
    let t = t.strip_prefix(['+', '-']).unwrap_or(t);
    let lower = t.to_ascii_lowercase();
    if matches!(lower.as_str(), "inf" | "infinity" | "nan") {
        return true;
    }
    let (mantissa, exponent) = match lower.split_once('e') {
        Some((m, e)) => (m, Some(e)),
        None => (lower.as_str(), None),
    };
    let (int, frac) = mantissa
        .split_once('.')
        .map_or((mantissa, None), |(i, f)| (i, Some(f)));
    let part_ok = |p: &str| p.is_empty() || digits_with_underscores(p);
    if int.is_empty() && frac.is_none_or(str::is_empty)
        || !part_ok(int)
        || !frac.is_none_or(part_ok)
    {
        return false;
    }
    exponent.is_none_or(|e| digits_with_underscores(e.strip_prefix(['+', '-']).unwrap_or(e)))
}

fn int_value(text: &str) -> Value {
    let clean = text.trim().replace('_', "");
    clean
        .strip_prefix('+')
        .unwrap_or(&clean)
        .parse::<i64>()
        .map_or_else(|_| Value::String(clean.clone()), Value::from)
}

fn float_value(text: &str) -> Value {
    text.trim()
        .replace('_', "")
        .parse::<f64>()
        .ok()
        .and_then(serde_json::Number::from_f64)
        .map_or(Value::Null, Value::Number)
}

fn json_list(text: &str) -> Result<Value, String> {
    if let Some(err) = json_errors::decode_error(text) {
        return Err(format!("not valid JSON: {err}"));
    }
    if !text.trim_start().starts_with('[') {
        return Err("expected a JSON list".to_owned());
    }
    Ok(serde_json::from_str(text).unwrap_or(Value::Array(Vec::new())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repr_picks_quotes_like_python() {
        assert_eq!(repr("a"), "'a'");
        assert_eq!(repr("it's"), "\"it's\"");
        assert_eq!(repr("a\"b'c"), "'a\"b\\'c'");
        assert_eq!(repr("t\tn\n\\"), "'t\\tn\\n\\\\'");
    }

    #[test]
    fn int_and_float_acceptance() {
        for ok in ["1", " -5 ", "+7", "1_000"] {
            assert!(int_accepts(ok), "{ok}");
        }
        for bad in ["", "1.5", "1__0", "_1", "abc", "0x10", "- 1"] {
            assert!(!int_accepts(bad), "{bad}");
        }
        for ok in ["1.5", ".5", "5.", "1e5", "-inf", "NaN", "1_0.5", " 2 "] {
            assert!(float_accepts(ok), "{ok}");
        }
        for bad in ["", ".", "e5", "1e", "1.5.2", "abc"] {
            assert!(!float_accepts(bad), "{bad}");
        }
    }

    #[test]
    fn typed_kinds() {
        assert_eq!(
            convert_kind(Kind::Int, "1_000").unwrap(),
            serde_json::json!(1000)
        );
        assert!(convert_kind(Kind::Int, "x").is_err());
        assert_eq!(
            convert_kind(Kind::JsonList, "[1, 2]").unwrap(),
            serde_json::json!([1, 2])
        );
        assert!(convert_kind(Kind::JsonList, "{}").is_err());
        assert_eq!(
            convert_kind(Kind::Port, "8321").unwrap(),
            serde_json::json!(8321)
        );
        assert!(convert_kind(Kind::Port, "70000").is_err());
    }
}

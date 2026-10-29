//! JSON serialisation and number formatting that match the byte-exact output.
//!
//! Dict keys keep insertion order, with `", "` and `": "` separators;
//! floats use shortest round-trip spelling; integer parsing accepts whitespace, signs,
//! underscores and Unicode digits; rounding to `n` digits is correctly rounded
//! half-even decimal rounding.

use serde_json::{Number, Value};

/// Render an f64 as shortest round-trip text, as used in JSON output
/// with a `.0` suffix for integral values,
/// and `e+NN` exponent style.
pub fn py_float_repr(v: f64) -> String {
    if v.is_nan() {
        return "nan".to_string();
    }
    if v.is_infinite() {
        return if v > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    let mut s = format!("{v:?}");
    // Rust's Debug for f64 prints the shortest round-trip form, including
    // `.0` for integral values, which matches the required float spelling.
    if let Some(e) = s.find('e') {
        // Exponents use at least two digits and an explicit sign:
        // `1e-05` -> `1e-05`, Rust prints `1e-5`.
        let (mant, exp) = s.split_at(e);
        let exp = &exp[1..];
        let (sign, digits) = match exp.strip_prefix('-') {
            Some(d) => ("-", d),
            None => ("+", exp.trim_start_matches('+')),
        };
        let digits = if digits.len() == 1 {
            format!("0{digits}")
        } else {
            digits.to_string()
        };
        // Examples: `1e+20` and `1.5e-07`.
        let mant = mant.trim_end_matches(".0");
        s = format!("{mant}e{sign}{digits}");
        // Exponent form is used only for exp >= 16 or <= -5;
        // Rust's Debug uses the same threshold, so no extra work is needed.
        return s;
    }
    s
}

/// Insertion-order keys, `", "` and
/// `": "` separators, no ASCII escaping of non-ASCII text.
pub fn dumps(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value);
    out
}

fn write_value(out: &mut String, v: &Value) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => write_number(out, n),
        Value::String(s) => write_string(out, s),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_value(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (k, val)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_string(out, k);
                out.push_str(": ");
                write_value(out, val);
            }
            out.push('}');
        }
    }
}

fn write_number(out: &mut String, n: &Number) {
    if let Some(i) = n.as_i64() {
        out.push_str(&i.to_string());
    } else if let Some(u) = n.as_u64() {
        out.push_str(&u.to_string());
    } else if let Some(f) = n.as_f64() {
        out.push_str(&py_float_repr(f));
    }
}

fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Pretty layout with
/// a single-space indent step (what `export` writes for `pool.json`).
pub fn dump_pretty(value: &Value, indent_step: usize) -> String {
    let mut out = String::new();
    write_pretty(&mut out, value, indent_step, 0);
    out
}

fn write_pretty(out: &mut String, v: &Value, step: usize, depth: usize) {
    let pad = |d: usize| " ".repeat(step * d);
    match v {
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                out.push_str(&pad(depth + 1));
                write_pretty(out, item, step, depth + 1);
                if i + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad(depth));
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            for (i, (k, val)) in map.iter().enumerate() {
                out.push_str(&pad(depth + 1));
                write_string(out, k);
                out.push_str(": ");
                write_pretty(out, val, step, depth + 1);
                if i + 1 < map.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad(depth));
            out.push('}');
        }
        _ => write_value(out, v),
    }
}

/// Correctly rounded decimal rounding to `ndigits`
/// (round-half-even on the decimal representation).
/// Rust's `{:.prec$}` formatting is also correctly rounded half-even.
pub fn py_round(v: f64, ndigits: u32) -> f64 {
    if !v.is_finite() {
        return v;
    }
    let s = format!("{v:.prec$}", prec = ndigits as usize);
    s.parse().unwrap_or(v)
}

/// Marker for an integer parse failure.
#[derive(Debug, thiserror::Error)]
#[error("invalid literal for int(): {0:?}")]
pub struct PyIntError(pub String);

/// Integer parse: accepts surrounding whitespace (incl. \t\n), a sign,
/// underscores between digits, and Unicode decimal digits.
/// Returns `Err` on anything else.
pub fn py_int(s: &str) -> Result<i64, PyIntError> {
    let bad = || PyIntError(s.to_string());
    let t = s.trim_matches(|c: char| c.is_whitespace());
    if t.is_empty() {
        return Err(bad());
    }
    let (sign, rest) = match t.strip_prefix('-') {
        Some(r) => (-1i64, r),
        None => (1i64, t.strip_prefix('+').unwrap_or(t)),
    };
    if rest.is_empty() {
        return Err(bad());
    }
    let mut value: i64 = 0;
    let mut prev_underscore = false;
    let mut saw_digit = false;
    for c in rest.chars() {
        if c == '_' {
            // Underscore must sit between two digits.
            if !saw_digit || prev_underscore {
                return Err(bad());
            }
            prev_underscore = true;
            continue;
        }
        let d = match py_unicode_digit(c) {
            Some(d) => d,
            None => return Err(bad()),
        };
        prev_underscore = false;
        saw_digit = true;
        value = value
            .checked_mul(10)
            .and_then(|v| v.checked_add(d))
            .ok_or_else(bad)?;
    }
    if prev_underscore || !saw_digit {
        return Err(bad());
    }
    Ok(sign * value)
}

/// Decimal digit value for any Unicode `Nd` character
/// accepts (ASCII plus the common digit blocks).
fn py_unicode_digit(c: char) -> Option<i64> {
    match c {
        '0'..='9' => Some(c as i64 - '0' as i64),
        '٠'..='٩' => Some(c as i64 - '٠' as i64), // Arabic-Indic
        '۰'..='۹' => Some(c as i64 - '۰' as i64), // Extended Arabic-Indic
        '߀'..='߉' => Some(c as i64 - '߀' as i64), // N'Ko
        '०'..='९' => Some(c as i64 - '०' as i64), // Devanagari
        '০'..='৯' => Some(c as i64 - '০' as i64), // Bengali
        '੦'..='੯' => Some(c as i64 - '੦' as i64), // Gurmukhi
        '૦'..='૯' => Some(c as i64 - '૦' as i64), // Gujarati
        '୦'..='୯' => Some(c as i64 - '୦' as i64), // Oriya
        '௦'..='௯' => Some(c as i64 - '௦' as i64), // Tamil
        '౦'..='౯' => Some(c as i64 - '౦' as i64), // Telugu
        '೦'..='೯' => Some(c as i64 - '೦' as i64), // Kannada
        '൦'..='൯' => Some(c as i64 - '൦' as i64), // Malayalam
        '๐'..='๙' => Some(c as i64 - '๐' as i64), // Thai
        '໐'..='໙' => Some(c as i64 - '໐' as i64), // Lao
        '༠'..='༩' => Some(c as i64 - '༠' as i64), // Tibetan
        '၀'..='၉' => Some(c as i64 - '၀' as i64), // Myanmar
        '႐'..='႙' => Some(c as i64 - '႐' as i64), // Myanmar Shan
        '០'..='៩' => Some(c as i64 - '០' as i64), // Khmer
        '᠐'..='᠙' => Some(c as i64 - '᠐' as i64), // Mongolian
        '᥆'..='᥏' => Some(c as i64 - '᥆' as i64), // Limbu
        '᧐'..='᧙' => Some(c as i64 - '᧐' as i64), // Tai Tham
        '᪀'..='᪉' => Some(c as i64 - '᪀' as i64), // Tai Tham Hora
        '᪐'..='᪙' => Some(c as i64 - '᪐' as i64), // Tai Tham Tham
        '᭐'..='᭙' => Some(c as i64 - '᭐' as i64), // Balinese
        '᮰'..='᮹' => Some(c as i64 - '᮰' as i64), // Sundanese
        '᱀'..='᱉' => Some(c as i64 - '᱀' as i64), // Lepcha
        '᱐'..='᱙' => Some(c as i64 - '᱐' as i64), // Ol Chiki
        '꘠'..='꘩' => Some(c as i64 - '꘠' as i64), // Vai
        '꣐'..='꣙' => Some(c as i64 - '꣐' as i64), // Saurashtra
        '꤀'..='꤉' => Some(c as i64 - '꤀' as i64), // Kayah Li
        '꩐'..='꩙' => Some(c as i64 - '꩐' as i64), // Cham
        '０'..='９' => Some(c as i64 - '０' as i64), // Fullwidth
        _ => c.to_digit(10).map(|d| d as i64),
    }
}

/// Text of a cell value imported from XLSX, where a float
/// keeps its `.0` (`str(240.0) == "240.0"`).
pub fn py_str_f64(v: f64) -> String {
    py_float_repr(v)
}

/// Text of a JSON scalar: `"True"/"False"/"None"`, ints as
/// digits, floats in shortest round-trip form, strings as-is.
pub fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(b) => (if *b { "True" } else { "False" }).to_string(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else {
                py_float_repr(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => s.clone(),
        other => dumps(other),
    }
}

/// Integer value of a JSON scalar: `"5"` gives 5, `3.7` gives 3,
/// `true` gives 1; `Err` for values that cannot convert.
pub fn py_int_value(v: &Value) -> Result<i64, PyIntError> {
    match v {
        Value::Bool(b) => Ok(if *b { 1 } else { 0 }),
        Value::Number(n) => Ok(match n.as_i64() {
            Some(i) => i,
            None => n.as_f64().unwrap_or(0.0).trunc() as i64,
        }),
        Value::String(s) => py_int(s),
        other => Err(PyIntError(dumps(other))),
    }
}

/// General-format float helper (kept for output comparisons).
pub fn py_str_i64(v: i64) -> String {
    v.to_string()
}

/// Quoted string text: single quotes, switching to double when the string
/// has `'` but no `"`; escapes `\\`, the quote, `\n\r\t` and non-printable
/// C0/C1/nbsp/soft-hyphen code points as `\xNN`.
pub fn py_repr_str(s: &str) -> String {
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
            c if (c as u32) < 0x20 || (0x7f..=0xa0).contains(&(c as u32)) || c as u32 == 0xad => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

//! reference text forms of JSON values used across the ported sf modules.
//!
//! Mirrors `spiderhead_parse::py_json` (a private module there): `repr(float)`, `str(value)`
//! for scalars, and `json.dumps(sort_keys=True, ensure_ascii=False)` for containers.

use serde_json::{Map, Value};
pub(crate) use spiderhead_parse::py_str::py_repr;
use spiderhead_parse::py_str::py_strip;

/// reference `repr(float)`: shortest round-trip digits, fixed notation for exponents in
/// `[-4, 16)`, otherwise `d.ddde+XX`.
pub fn float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".to_owned();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf" } else { "-inf" }.to_owned();
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0" } else { "0.0" }.to_owned();
    }
    let sci = format!("{:e}", x.abs());
    let (mantissa, exponent) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exponent.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let sign = if x < 0.0 { "-" } else { "" };
    if (-4..16).contains(&exp) {
        if exp >= 0 {
            let int_len = exp as usize + 1;
            let padded = format!("{digits:0<int_len$}");
            let (int_part, frac) = padded.split_at(int_len);
            let frac = if frac.is_empty() { "0" } else { frac };
            format!("{sign}{int_part}.{frac}")
        } else {
            format!("{sign}0.{}{digits}", "0".repeat((-exp - 1) as usize))
        }
    } else {
        let head = &digits[..1];
        let tail = &digits[1..];
        let dot = if tail.is_empty() {
            String::new()
        } else {
            format!(".{tail}")
        };
        let esign = if exp < 0 { '-' } else { '+' };
        format!("{sign}{head}{dot}e{esign}{:02}", exp.abs())
    }
}

/// reference `repr(int)`/`repr(float)` of a JSON number.
pub fn number_repr(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        i.to_string()
    } else if let Some(u) = n.as_u64() {
        u.to_string()
    } else {
        float_repr(n.as_f64().unwrap_or(0.0))
    }
}

/// reference `repr(value)` for a JSON value: `{'a': 1}`-style containers, `'x'`-quoted
/// strings, `True`/`False`/`None` and `repr` numbers. `str(container)` delegates here.
pub fn py_repr_value(value: &Value) -> String {
    match value {
        Value::Null => "None".to_owned(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_owned(),
        Value::Number(n) => number_repr(n),
        Value::String(s) => py_repr(s),
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(py_repr_value)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(k, v)| format!("{}: {}", py_repr(k), py_repr_value(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// reference `str(value)`: identity for strings, `repr` numbers/booleans/`None`, and the
/// `repr` container layout (`str(dict)` is `repr(dict)` in Creference).
pub fn str_or_dumps(value: &Value) -> String {
    match value {
        Value::Null => "None".to_owned(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_owned(),
        Value::Number(n) => number_repr(n),
        Value::String(s) => s.clone(),
        other => py_repr_value(other),
    }
}

/// reference `float(str)`: whitespace-tolerant, case-insensitive `nan`/`inf`/`infinity`,
/// PEP 515 digit separators (`1_000.5`), and scientific notation. Non-ASCII decimal
/// digits are not supported (Creference accepts them; JSON-sourced cells never carry
/// them). Returns `None` where Creference raises `ValueError`.
pub fn py_float(text: &str) -> Option<f64> {
    let s = py_strip(text);
    let bytes = s.as_bytes();
    let mut i = 0usize;
    if matches!(bytes.get(i), Some(b'+') | Some(b'-')) {
        i += 1;
    }
    let rest = &s[i..];
    let lower = rest.to_lowercase();
    match lower.as_str() {
        "nan" => return Some(f64::NAN),
        "inf" | "infinity" => {
            return Some(if s.starts_with('-') {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            });
        }
        _ => {}
    }
    // Grammar: number = (digitpart ["." [digitpart]] | "." digitpart) [exponent]
    //          exponent = [eE] [+-]? digitpart
    //          digitpart = digit ("_"? digit)*
    let b = rest.as_bytes();
    let mut j = 0usize;
    let int_part = digit_part(b, j);
    j = int_part;
    let mut has_digits = int_part > 0;
    if b.get(j) == Some(&b'.') {
        j += 1;
        let frac = digit_part(b, j);
        has_digits = has_digits || frac > j;
        j = frac;
    }
    if !has_digits {
        return None;
    }
    if matches!(b.get(j), Some(b'e') | Some(b'E')) {
        j += 1;
        if matches!(b.get(j), Some(b'+') | Some(b'-')) {
            j += 1;
        }
        let exp = digit_part(b, j);
        if exp == j {
            return None;
        }
        j = exp;
    }
    if j != b.len() {
        return None;
    }
    let cleaned: String = rest.chars().filter(|c| *c != '_').collect();
    let with_sign = if s.starts_with('-') {
        format!("-{cleaned}")
    } else {
        cleaned
    };
    with_sign.parse::<f64>().ok()
}

/// reference `type(value).__name__` for a JSON value (error-message building).
pub fn py_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        // JSON `5.0` arrives as f64 → reference `float`; integers → `int`.
        Value::Number(n) => {
            if n.is_f64() {
                "float"
            } else {
                "int"
            }
        }
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// reference truthiness of a JSON value: `None`/`False`/`0`/empty string/empty
/// containers are falsy; everything else is truthy.
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `truthy` over an optional reference (missing is falsy).
pub fn truthy_opt(value: Option<&Value>) -> bool {
    value.is_some_and(truthy)
}

/// reference `==` on JSON values: numbers compare across `int`/`float`/`bool`
/// (`1 == 1.0 == True`), containers recursively, anything else by type.
/// `NaN != NaN`, as in Creference.
pub fn py_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        _ if numeric(a) && numeric(b) => numeric_eq(a, b),
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(u, v)| py_eq(u, v))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| py_eq(v, w)))
        }
        _ => false,
    }
}

fn numeric(v: &Value) -> bool {
    matches!(v, Value::Bool(_) | Value::Number(_))
}

/// Exact numeric equality: bools count as 0/1; `int == float` only when the
/// float is integral and fits, so `2**53+1 != 2.0**53` stays false.
fn numeric_eq(a: &Value, b: &Value) -> bool {
    let int_of = |v: &Value| -> Option<i128> {
        match v {
            Value::Bool(x) => Some(i128::from(*x)),
            Value::Number(n) => n
                .as_i64()
                .map(i128::from)
                .or_else(|| n.as_u64().map(i128::from)),
            _ => None,
        }
    };
    match (int_of(a), int_of(b)) {
        (Some(x), Some(y)) => x == y,
        (Some(x), None) | (None, Some(x)) => {
            let f = if int_of(a).is_none() { a } else { b }
                .as_f64()
                .unwrap_or(f64::NAN);
            f.is_finite() && f.fract() == 0.0 && (f as i128) == x
        }
        (None, None) => a.as_f64() == b.as_f64(),
    }
}

/// `json.dumps(value, sort_keys=True, ensure_ascii=False)` — the reference text
/// form with sorted object keys and `", "` / `": "` separators.
pub fn dumps_sorted(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value);
    out
}

fn write_value(out: &mut String, value: &Value) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&number_repr(n)),
        Value::String(s) => dumps_string(out, s),
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
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dumps_string(out, key);
                out.push_str(": ");
                write_value(out, &map[key.as_str()]);
            }
            out.push('}');
        }
    }
}

fn dumps_string(out: &mut String, text: &str) {
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
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

// --- `json.loads` -----------------------------------------------------------
//
// Cthe reference `json.loads` grammar: the standard six literal/value forms plus
// `NaN`, `Infinity` and `-Infinity`. `serde_json::Value` cannot hold a
// non-finite float, so those parse to the same marker strings the oracle
// fixtures emit under `_finite` normalization ("NaN"/"Infinity"/"-Infinity").
// A literal `"NaN"` string in input is therefore indistinguishable from a
// parsed NaN — a documented transport limitation; numeric consumers treat the
// three markers as non-finite.
//
// Errors are `JSONDecodeError: <msg>: line <l> column <c> (char <p>)` with
// Cthe reference message texts and positions (char positions, not bytes).

/// `json.loads(text)`: the parsed value or the full `JSONDecodeError: …`
/// text Cthe reference `str(exc)` produces.
pub fn py_json_loads(text: &str) -> Result<Value, String> {
    let mut parser = PyJson {
        chars: text.chars().collect(),
        pos: 0,
    };
    parser.skip_ws();
    let value = parser.value()?;
    parser.skip_ws();
    if parser.pos != parser.chars.len() {
        return Err(parser.decode_error("Extra data"));
    }
    Ok(value)
}

struct PyJson {
    chars: Vec<char>,
    pos: usize,
}

impl PyJson {
    fn skip_ws(&mut self) {
        while matches!(self.chars.get(self.pos), Some(' ' | '\t' | '\n' | '\r')) {
            self.pos += 1;
        }
    }

    fn at(&self, c: char) -> bool {
        self.chars.get(self.pos) == Some(&c)
    }

    /// `JSONDecodeError: <msg>: line <l> column <c> (char <pos>)`.
    fn decode_error(&self, msg: &str) -> String {
        let (line, col) = self.line_col(self.pos);
        format!(
            "JSONDecodeError: {msg}: line {line} column {col} (char {})",
            self.pos
        )
    }

    fn error_at(&self, msg: &str, pos: usize) -> String {
        let (line, col) = self.line_col(pos);
        format!("JSONDecodeError: {msg}: line {line} column {col} (char {pos})")
    }

    /// Creference reports 1-based line/column from the char offset.
    fn line_col(&self, pos: usize) -> (usize, usize) {
        let mut line = 1usize;
        let mut after_last_nl = 0usize;
        for (i, c) in self.chars[..pos.min(self.chars.len())].iter().enumerate() {
            if *c == '\n' {
                line += 1;
                after_last_nl = i + 1;
            }
        }
        (line, pos - after_last_nl + 1)
    }

    fn value(&mut self) -> Result<Value, String> {
        match self.chars.get(self.pos) {
            Some('{') => self.object(),
            Some('[') => self.array(),
            Some('"') => self.string().map(Value::String),
            Some('t') if self.lit("true") => {
                self.pos += 4;
                Ok(Value::Bool(true))
            }
            Some('f') if self.lit("false") => {
                self.pos += 5;
                Ok(Value::Bool(false))
            }
            Some('n') if self.lit("null") => {
                self.pos += 4;
                Ok(Value::Null)
            }
            Some('N') if self.lit("NaN") => {
                self.pos += 3;
                Ok(Value::String("NaN".to_owned()))
            }
            Some('I') if self.lit("Infinity") => {
                self.pos += 8;
                Ok(Value::String("Infinity".to_owned()))
            }
            Some('-') if self.lit("-Infinity") => {
                self.pos += 9;
                Ok(Value::String("-Infinity".to_owned()))
            }
            Some('-' | '0'..='9') => self.number(),
            _ => Err(self.decode_error("Expecting value")),
        }
    }

    /// Prefix match for the JSON literals.
    fn lit(&self, word: &str) -> bool {
        word.chars()
            .enumerate()
            .all(|(i, c)| self.chars.get(self.pos + i) == Some(&c))
    }

    /// Cthe reference number grammar: `-?(0|[1-9]\d*)(\.\d+)?([eE][+-]?\d+)?`.
    /// A trailing incomplete fraction/exponent is simply not matched, so
    /// `1.` parses `1` and leaves `.` for the `Extra data`/`',' delimiter`
    /// error downstream.
    fn number(&mut self) -> Result<Value, String> {
        let start = self.pos;
        if self.at('-') {
            self.pos += 1;
        }
        match self.chars.get(self.pos) {
            Some('0') => self.pos += 1,
            Some('1'..='9') => {
                while matches!(self.chars.get(self.pos), Some('0'..='9')) {
                    self.pos += 1;
                }
            }
            _ => {
                self.pos = start;
                return Err(self.decode_error("Expecting value"));
            }
        }
        let mut is_float = false;
        if self.at('.') && matches!(self.chars.get(self.pos + 1), Some('0'..='9')) {
            is_float = true;
            self.pos += 1;
            while matches!(self.chars.get(self.pos), Some('0'..='9')) {
                self.pos += 1;
            }
        }
        if matches!(self.chars.get(self.pos), Some('e' | 'E')) {
            let save = self.pos;
            let mut p = self.pos + 1;
            if matches!(self.chars.get(p), Some('+' | '-')) {
                p += 1;
            }
            if matches!(self.chars.get(p), Some('0'..='9')) {
                is_float = true;
                p += 1;
                while matches!(self.chars.get(p), Some('0'..='9')) {
                    p += 1;
                }
                self.pos = p;
            } else {
                self.pos = save;
            }
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        if !is_float {
            if let Ok(i) = text.parse::<i64>() {
                return Ok(Value::from(i));
            }
            if let Ok(u) = text.parse::<u64>() {
                return Ok(Value::from(u));
            }
        }
        let f = text.parse::<f64>().unwrap_or(f64::NAN);
        Ok(serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number))
    }

    fn object(&mut self) -> Result<Value, String> {
        self.pos += 1; // '{'
        let mut map = Map::new();
        self.skip_ws();
        if self.at('}') {
            self.pos += 1;
            return Ok(Value::Object(map));
        }
        loop {
            self.skip_ws();
            if !self.at('"') {
                return Err(self.decode_error("Expecting property name enclosed in double quotes"));
            }
            let key = self.string()?;
            self.skip_ws();
            if !self.at(':') {
                return Err(self.decode_error("Expecting ':' delimiter"));
            }
            self.pos += 1;
            self.skip_ws();
            let value = self.value()?;
            map.insert(key, value);
            self.skip_ws();
            if self.at('}') {
                self.pos += 1;
                return Ok(Value::Object(map));
            }
            if self.at(',') {
                let comma = self.pos;
                self.pos += 1;
                self.skip_ws();
                if self.at('}') {
                    return Err(self.error_at("Illegal trailing comma before end of object", comma));
                }
                continue;
            }
            return Err(self.decode_error("Expecting ',' delimiter"));
        }
    }

    fn array(&mut self) -> Result<Value, String> {
        self.pos += 1; // '['
        let mut items = Vec::new();
        self.skip_ws();
        if self.at(']') {
            self.pos += 1;
            return Ok(Value::Array(items));
        }
        loop {
            self.skip_ws();
            let value = self.value()?;
            items.push(value);
            self.skip_ws();
            if self.at(']') {
                self.pos += 1;
                return Ok(Value::Array(items));
            }
            if self.at(',') {
                let comma = self.pos;
                self.pos += 1;
                self.skip_ws();
                if self.at(']') {
                    return Err(self.error_at("Illegal trailing comma before end of array", comma));
                }
                continue;
            }
            return Err(self.decode_error("Expecting ',' delimiter"));
        }
    }

    /// `"`-quoted string: the standard escapes plus `\uXXXX` with surrogate
    /// pairing. A lone surrogate cannot live in a Rust `String` and becomes
    /// U+FFFD — reference keeps it, which JSON fixtures never carry.
    fn string(&mut self) -> Result<String, String> {
        let start = self.pos;
        self.pos += 1; // '"'
        let mut out = String::new();
        loop {
            let Some(&c) = self.chars.get(self.pos) else {
                return Err(self.error_at("Unterminated string starting at", start));
            };
            match c {
                '"' => {
                    self.pos += 1;
                    return Ok(out);
                }
                '\\' => {
                    self.pos += 1;
                    let Some(&esc) = self.chars.get(self.pos) else {
                        return Err(self.error_at("Unterminated string starting at", start));
                    };
                    let decoded = match esc {
                        '"' => Some('"'),
                        '\\' => Some('\\'),
                        '/' => Some('/'),
                        'b' => Some('\u{8}'),
                        'f' => Some('\u{c}'),
                        'n' => Some('\n'),
                        'r' => Some('\r'),
                        't' => Some('\t'),
                        'u' => {
                            self.pos += 1;
                            let unit = self.hex4()?;
                            if (0xD800..0xDC00).contains(&unit) {
                                // High surrogate: consume the paired low half.
                                if self.chars.get(self.pos) == Some(&'\\')
                                    && self.chars.get(self.pos + 1) == Some(&'u')
                                {
                                    self.pos += 2;
                                    let low = self.hex4()?;
                                    let code = 0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00);
                                    char::from_u32(code).or(Some('\u{fffd}'))
                                } else {
                                    Some('\u{fffd}')
                                }
                            } else {
                                char::from_u32(unit).or(Some('\u{fffd}'))
                            }
                        }
                        _ => {
                            return Err(self.decode_error("Invalid \\escape"));
                        }
                    };
                    if let Some(c) = decoded {
                        out.push(c);
                    }
                    self.pos += 1;
                }
                c if (c as u32) < 0x20 => {
                    return Err(self.decode_error("Invalid control character"));
                }
                c => {
                    out.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    /// Four hex digits of a `\uXXXX` escape; pos stays on the first bad char.
    fn hex4(&mut self) -> Result<u32, String> {
        let mut value = 0u32;
        for _ in 0..4 {
            let Some(&c) = self.chars.get(self.pos) else {
                return Err(self.decode_error("Invalid \\uXXXX escape"));
            };
            let Some(digit) = c.to_digit(16) else {
                return Err(self.decode_error("Invalid \\uXXXX escape"));
            };
            value = value * 16 + digit;
            self.pos += 1;
        }
        Ok(value)
    }
}

/// Consume `digit ("_"? digit)*` starting at `i`; returns the end offset.
fn digit_part(b: &[u8], mut i: usize) -> usize {
    // First char must be a digit; "_" only sits between two digits.
    if !b.get(i).is_some_and(u8::is_ascii_digit) {
        return i;
    }
    i += 1;
    while let Some(&c) = b.get(i) {
        if c.is_ascii_digit() {
            i += 1;
        } else if c == b'_' && b.get(i + 1).is_some_and(u8::is_ascii_digit) {
            i += 2;
        } else {
            break;
        }
    }
    i
}

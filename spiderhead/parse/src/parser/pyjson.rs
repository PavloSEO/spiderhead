//! JSON reader over `serde_json::Value` with `json.loads` error texts and positions.
//! Error wording is kept identical because it is part of the reported `JsonldInvalid.error` output.
//!
//! Deviations (`Value` cannot hold them): `NaN` and infinities become the strings `"NaN"`, `"Infinity"`, `"-Infinity"`;
//! integers beyond 64 bits become `f64`; lone surrogate escapes become U+FFFD.

use serde_json::{Map, Number, Value};

/// A JSON decode error, rendered as its display text.
pub(crate) struct JsonError(pub String);

/// Maximum container nesting depth; deeper input is rejected with an error instead of overflowing the stack.
const MAX_DEPTH: usize = 1000;

struct Scanner<'a> {
    s: &'a [char],
}

type Res<T> = Result<T, JsonError>;

fn error(msg: &str, s: &[char], pos: usize) -> JsonError {
    let line = s[..pos.min(s.len())].iter().filter(|&&c| c == '\n').count() + 1;
    let last_newline = s[..pos.min(s.len())].iter().rposition(|&c| c == '\n');
    let col = match last_newline {
        Some(at) => pos - at,
        None => pos + 1,
    };
    JsonError(format!("{msg}: line {line} column {col} (char {pos})"))
}

fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

impl Scanner<'_> {
    fn at(&self, i: usize) -> Option<char> {
        self.s.get(i).copied()
    }

    fn skip_ws(&self, mut i: usize) -> usize {
        while self.at(i).is_some_and(is_ws) {
            i += 1;
        }
        i
    }

    fn err(&self, msg: &str, pos: usize) -> JsonError {
        error(msg, self.s, pos)
    }

    fn starts(&self, i: usize, word: &str) -> bool {
        word.chars()
            .enumerate()
            .all(|(k, c)| self.at(i + k) == Some(c))
    }

    fn scan_string(&self, mut end: usize) -> Res<(String, usize)> {
        let begin = end - 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.at(end) else {
                return Err(self.err("Unterminated string starting at", begin));
            };
            match c {
                '"' => return Ok((out, end + 1)),
                c if (c as u32) < 0x20 => {
                    return Err(self.err("Invalid control character at", end));
                }
                '\\' => {
                    let Some(esc) = self.at(end + 1) else {
                        return Err(self.err("Unterminated string starting at", begin));
                    };
                    if esc == 'u' {
                        // The four hex digits must be followed by at least one more character.
                        let hex = |at: usize| -> Option<u32> {
                            self.at(at + 4)?;
                            let digits: Option<String> = (0..4).map(|k| self.at(at + k)).collect();
                            u32::from_str_radix(&digits?, 16).ok().filter(|_| {
                                (0..4)
                                    .all(|k| self.at(at + k).is_some_and(|c| c.is_ascii_hexdigit()))
                            })
                        };
                        let Some(mut uni) = hex(end + 2) else {
                            return Err(self.err("Invalid \\uXXXX escape", end + 1));
                        };
                        end += 6;
                        if (0xd800..=0xdbff).contains(&uni) && self.starts(end, "\\u") {
                            let Some(low) = hex(end + 2) else {
                                return Err(self.err("Invalid \\uXXXX escape", end + 1));
                            };
                            if (0xdc00..=0xdfff).contains(&low) {
                                uni = 0x10000 + (((uni - 0xd800) << 10) | (low - 0xdc00));
                                end += 6;
                            }
                        }
                        out.push(char::from_u32(uni).unwrap_or('\u{fffd}'));
                    } else {
                        let decoded = match esc {
                            '"' => '"',
                            '\\' => '\\',
                            '/' => '/',
                            'b' => '\u{8}',
                            'f' => '\u{c}',
                            'n' => '\n',
                            'r' => '\r',
                            't' => '\t',
                            _ => return Err(self.err("Invalid \\escape", end)),
                        };
                        out.push(decoded);
                        end += 2;
                    }
                }
                c => {
                    out.push(c);
                    end += 1;
                }
            }
        }
    }

    /// Scan one JSON value; end of input where a value is expected is reported as "Expecting value".
    fn scan_value(&self, idx: usize, depth: usize) -> Res<(Value, usize)> {
        if depth > MAX_DEPTH {
            return Err(self.err("maximum recursion depth exceeded", idx));
        }
        let Some(c) = self.at(idx) else {
            return Err(self.err("Expecting value", idx));
        };
        match c {
            '"' => self
                .scan_string(idx + 1)
                .map(|(s, e)| (Value::String(s), e)),
            '{' => self.scan_object(idx + 1, depth + 1),
            '[' => self.scan_array(idx + 1, depth + 1),
            'n' if self.starts(idx, "null") => Ok((Value::Null, idx + 4)),
            't' if self.starts(idx, "true") => Ok((Value::Bool(true), idx + 4)),
            'f' if self.starts(idx, "false") => Ok((Value::Bool(false), idx + 5)),
            _ => {
                if let Some(found) = self.scan_number(idx) {
                    return Ok(found);
                }
                if c == 'N' && self.starts(idx, "NaN") {
                    Ok((Value::String("NaN".into()), idx + 3))
                } else if c == 'I' && self.starts(idx, "Infinity") {
                    Ok((Value::String("Infinity".into()), idx + 8))
                } else if c == '-' && self.starts(idx, "-Infinity") {
                    Ok((Value::String("-Infinity".into()), idx + 9))
                } else {
                    Err(self.err("Expecting value", idx))
                }
            }
        }
    }

    fn scan_number(&self, start: usize) -> Option<(Value, usize)> {
        let mut i = start;
        if self.at(i) == Some('-') {
            i += 1;
        }
        match self.at(i) {
            Some('0') => i += 1,
            Some('1'..='9') => {
                while self.at(i).is_some_and(|c| c.is_ascii_digit()) {
                    i += 1;
                }
            }
            _ => return None,
        }
        let int_end = i;
        let mut is_float = false;
        if self.at(i) == Some('.') && self.at(i + 1).is_some_and(|c| c.is_ascii_digit()) {
            i += 1;
            while self.at(i).is_some_and(|c| c.is_ascii_digit()) {
                i += 1;
            }
            is_float = true;
        }
        if matches!(self.at(i), Some('e' | 'E')) {
            let mut k = i + 1;
            if matches!(self.at(k), Some('+' | '-')) {
                k += 1;
            }
            if self.at(k).is_some_and(|c| c.is_ascii_digit()) {
                while self.at(k).is_some_and(|c| c.is_ascii_digit()) {
                    k += 1;
                }
                i = k;
                is_float = true;
            }
        }
        let text: String = self.s[start..i].iter().collect();
        let value = if is_float {
            float_value(text.parse::<f64>().unwrap_or(f64::NAN))
        } else {
            let _ = int_end;
            match text.parse::<i64>() {
                Ok(n) => Value::Number(n.into()),
                Err(_) => match text.parse::<u64>() {
                    Ok(n) => Value::Number(n.into()),
                    Err(_) => float_value(text.parse::<f64>().unwrap_or(f64::NAN)),
                },
            }
        };
        Some((value, i))
    }

    fn scan_object(&self, mut end: usize, depth: usize) -> Res<(Value, usize)> {
        let mut map = Map::new();
        let mut next = self.at(end);
        if next != Some('"') {
            if next.is_some_and(is_ws) {
                end = self.skip_ws(end);
                next = self.at(end);
            }
            if next == Some('}') {
                return Ok((Value::Object(map), end + 1));
            }
            if next != Some('"') {
                return Err(self.err("Expecting property name enclosed in double quotes", end));
            }
        }
        end += 1;
        loop {
            let (key, e) = self.scan_string(end)?;
            end = e;
            if self.at(end) != Some(':') {
                end = self.skip_ws(end);
                if self.at(end) != Some(':') {
                    return Err(self.err("Expecting ':' delimiter", end));
                }
            }
            end += 1;
            end = self.skip_ws(end);
            let (value, e) = self.scan_value(end, depth)?;
            end = e;
            map.insert(key, value);
            end = self.skip_ws(end);
            let next = self.at(end);
            end += 1;
            if next == Some('}') {
                break;
            }
            if next != Some(',') {
                return Err(self.err("Expecting ',' delimiter", end - 1));
            }
            let comma = end - 1;
            end = self.skip_ws(end);
            let next = self.at(end);
            end += 1;
            if next != Some('"') {
                if next == Some('}') {
                    return Err(self.err("Illegal trailing comma before end of object", comma));
                }
                return Err(self.err("Expecting property name enclosed in double quotes", end - 1));
            }
        }
        Ok((Value::Object(map), end))
    }

    fn scan_array(&self, mut end: usize, depth: usize) -> Res<(Value, usize)> {
        let mut values = Vec::new();
        end = self.skip_ws(end);
        if self.at(end) == Some(']') {
            return Ok((Value::Array(values), end + 1));
        }
        loop {
            let (value, e) = self.scan_value(end, depth)?;
            end = e;
            values.push(value);
            end = self.skip_ws(end);
            let next = self.at(end);
            end += 1;
            if next == Some(']') {
                break;
            }
            if next != Some(',') {
                return Err(self.err("Expecting ',' delimiter", end - 1));
            }
            let comma = end - 1;
            end = self.skip_ws(end);
            if self.at(end) == Some(']') {
                return Err(self.err("Illegal trailing comma before end of array", comma));
            }
        }
        Ok((Value::Array(values), end))
    }
}

fn float_value(f: f64) -> Value {
    match Number::from_f64(f) {
        Some(n) => Value::Number(n),
        None if f.is_nan() => Value::String("NaN".into()),
        None if f > 0.0 => Value::String("Infinity".into()),
        None => Value::String("-Infinity".into()),
    }
}

/// Parse `text` as JSON; error texts follow the shared wording of this reader.
pub(crate) fn loads(text: &str) -> Result<Value, JsonError> {
    let chars: Vec<char> = text.chars().collect();
    if chars.first() == Some(&'\u{feff}') {
        return Err(error(
            "Unexpected UTF-8 BOM (decode using utf-8-sig)",
            &chars,
            0,
        ));
    }
    let scanner = Scanner { s: &chars };
    let start = scanner.skip_ws(0);
    let (value, end) = scanner.scan_value(start, 0)?;
    let end = scanner.skip_ws(end);
    if end != chars.len() {
        return Err(scanner.err("Extra data", end));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: deeply nested JSON must return `Err`, not overflow the thread stack and abort.
    #[test]
    fn deeply_nested_arrays_error_instead_of_crashing() {
        let input = "[".repeat(100_000);
        let err = loads(&input).expect_err("nesting past the bound is an error");
        assert!(err.0.contains("maximum recursion depth exceeded"));
        let input = "[".repeat(100_000) + &"]".repeat(100_000);
        assert!(loads(&input).is_err());
        // Moderately nested JSON still parses.
        let input = "[".repeat(500) + &"]".repeat(500);
        assert!(loads(&input).is_ok());
    }
}

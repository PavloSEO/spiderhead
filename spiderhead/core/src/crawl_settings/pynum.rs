//! Number parsing for command-line and environment values. Accepted text: surrounding whitespace, a sign, `_` between digits, and any Unicode decimal digits.

use serde_json::Value;

use super::pynum_data::DECIMAL_ZEROS;
use crate::url_normalize::is_py_space;

/// The value of a Unicode decimal digit, or `None` for any other character.
fn decimal_value(c: char) -> Option<u32> {
    let cp = u32::from(c);
    let at = DECIMAL_ZEROS.partition_point(|&zero| zero <= cp);
    let zero = *DECIMAL_ZEROS.get(at.checked_sub(1)?)?;
    (cp - zero < 10).then(|| cp - zero)
}

/// Strip, fold Unicode digits to ASCII and drop `_` that sit between two digits. A stray `_`
/// stays in the text so the number parser rejects it.
fn plain_ascii(raw: &str) -> String {
    let chars: Vec<char> = raw.trim_matches(is_py_space).chars().collect();
    let digit = |c: char| decimal_value(c).and_then(|v| char::from_digit(v, 10));
    let folded: Vec<char> = chars.iter().map(|&c| digit(c).unwrap_or(c)).collect();
    let mut out = String::with_capacity(folded.len());
    for (i, &c) in folded.iter().enumerate() {
        let between_digits = i > 0
            && folded[i - 1].is_ascii_digit()
            && folded.get(i + 1).is_some_and(char::is_ascii_digit);
        if c != '_' || !between_digits {
            out.push(c);
        }
    }
    out
}

/// Parses an integer as a JSON number; `None` when the text is not an integer or does not fit 64 bits.
pub(super) fn py_int(raw: &str) -> Option<Value> {
    let text = plain_ascii(raw);
    let digits = text.strip_prefix(['+', '-']).unwrap_or(&text);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse::<i64>()
        .map(Value::from)
        .or_else(|_| text.parse::<u64>().map(Value::from))
        .ok()
}

/// Parses a float as a JSON number; `None` when the text is not a number, or for `inf`/`nan`, which a JSON number cannot represent.
pub(super) fn py_float(raw: &str) -> Option<Value> {
    let parsed = plain_ascii(raw).parse::<f64>().ok()?;
    parsed.is_finite().then(|| serde_json::json!(parsed))
}

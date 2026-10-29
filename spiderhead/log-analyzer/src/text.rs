//! Text and number rules that log fields share: which characters are whitespace, which are
//! digits, and how loosely a numeric token may be spelled.

use unicode_general_category::{get_general_category, GeneralCategory};

/// Whitespace in the wide sense log tokenisation uses: Unicode `White_Space` plus the
/// ASCII information separators U+001C..U+001F.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Trim [`is_space`] characters from both ends.
pub fn strip(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// Split on runs of [`is_space`] characters, dropping empty pieces.
pub fn split_whitespace(text: &str) -> impl Iterator<Item = &str> {
    text.split(is_space).filter(|piece| !piece.is_empty())
}

/// Value of a decimal digit in any Unicode script, or `None` for other characters.
///
/// Decimal digits of one script form contiguous blocks of ten starting at zero, so the
/// value is the distance from the start of the run of digit characters, modulo ten.
pub fn digit_value(c: char) -> Option<u32> {
    if c.is_ascii_digit() {
        return c.to_digit(10);
    }
    if c.is_ascii() || get_general_category(c) != GeneralCategory::DecimalNumber {
        return None;
    }
    let mut start = c as u32;
    while start > 0 {
        match char::from_u32(start - 1) {
            Some(prev) if get_general_category(prev) == GeneralCategory::DecimalNumber => {
                start -= 1;
            }
            _ => break,
        }
    }
    Some((c as u32 - start) % 10)
}

/// Rewrite every Unicode decimal digit as its ASCII counterpart.
pub fn ascii_digits(text: &str) -> String {
    text.chars()
        .map(|c| match digit_value(c) {
            Some(v) if !c.is_ascii() => char::from_digit(v, 10).unwrap_or(c),
            _ => c,
        })
        .collect()
}

/// Parse an integer token: optional sign, digits of any script, single underscores
/// between digits, surrounding whitespace ignored. Out-of-range values saturate.
pub fn parse_int(text: &str) -> Option<i64> {
    let text = strip(text);
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') {
        return None;
    }
    let mut value: i128 = 0;
    let mut previous_underscore = false;
    for c in digits.chars() {
        if c == '_' {
            if previous_underscore {
                return None;
            }
            previous_underscore = true;
            continue;
        }
        previous_underscore = false;
        value = value
            .saturating_mul(10)
            .saturating_add(i128::from(digit_value(c)?));
    }
    let value = if negative { -value } else { value };
    Some(value.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64)
}

/// Parse a number made of digits and dots (the only characters a time token may carry).
pub fn parse_float(text: &str) -> Option<f64> {
    ascii_digits(strip(text)).parse::<f64>().ok()
}

/// Single-quoted rendering of a plain string, for messages that quote offending input.
pub fn quoted(text: &str) -> String {
    if text.contains('\'') && !text.contains('"') {
        format!("\"{text}\"")
    } else {
        format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_follow_the_loose_token_rules() {
        assert_eq!(parse_int("1_000"), Some(1000));
        assert_eq!(parse_int(" +12 "), Some(12));
        assert_eq!(parse_int("-0"), Some(0));
        assert_eq!(parse_int("\u{0663}\u{0660}\u{0660}"), Some(300));
        assert_eq!(parse_int("1__0"), None);
        assert_eq!(parse_int("_1"), None);
        assert_eq!(parse_int("-"), None);
        assert_eq!(parse_int(""), None);
        assert_eq!(parse_int("1.5"), None);
    }

    #[test]
    fn floats_accept_unicode_digits_and_reject_double_dots() {
        assert_eq!(parse_float("0.123"), Some(0.123));
        assert_eq!(parse_float("1."), Some(1.0));
        assert_eq!(parse_float(".5"), Some(0.5));
        assert_eq!(parse_float("1.2.3"), None);
        assert_eq!(parse_float("."), None);
        assert_eq!(parse_float("\u{0661}.\u{0665}"), Some(1.5));
    }

    #[test]
    fn separators_count_as_space() {
        assert_eq!(strip("\u{1c} a \u{1f}"), "a");
        assert_eq!(split_whitespace("a\u{1e}b  c").count(), 3);
    }
}

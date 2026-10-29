//! HTML character reference decoding: the WHATWG named reference table plus the numeric
//! reference replacement rules.
//! The table is stored once in `src/data/html_entities.json`.

use std::collections::HashMap;
use std::sync::LazyLock;

/// Every standard named reference, keyed with and without `;`.
pub(super) static HTML5: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../data/html_entities.json"))
        .unwrap_or_else(|_| panic!("embedded html_entities.json is not a string map"))
});

/// NUL, CR and the cp1252 remaps for 0x80..=0x9F.
const INVALID_CHARREFS: &[(u64, char)] = &[
    (0x00, '\u{fffd}'),
    (0x0d, '\u{000d}'),
    (0x80, '\u{20ac}'),
    (0x81, '\u{0081}'),
    (0x82, '\u{201a}'),
    (0x83, '\u{0192}'),
    (0x84, '\u{201e}'),
    (0x85, '\u{2026}'),
    (0x86, '\u{2020}'),
    (0x87, '\u{2021}'),
    (0x88, '\u{02c6}'),
    (0x89, '\u{2030}'),
    (0x8a, '\u{0160}'),
    (0x8b, '\u{2039}'),
    (0x8c, '\u{0152}'),
    (0x8d, '\u{008d}'),
    (0x8e, '\u{017d}'),
    (0x8f, '\u{008f}'),
    (0x90, '\u{0090}'),
    (0x91, '\u{2018}'),
    (0x92, '\u{2019}'),
    (0x93, '\u{201c}'),
    (0x94, '\u{201d}'),
    (0x95, '\u{2022}'),
    (0x96, '\u{2013}'),
    (0x97, '\u{2014}'),
    (0x98, '\u{02dc}'),
    (0x99, '\u{2122}'),
    (0x9a, '\u{0161}'),
    (0x9b, '\u{203a}'),
    (0x9c, '\u{0153}'),
    (0x9d, '\u{009d}'),
    (0x9e, '\u{017e}'),
    (0x9f, '\u{0178}'),
];

/// Code points whose numeric reference is replaced by the empty string.
fn invalid_codepoint(num: u64) -> bool {
    matches!(
        num,
        0x01..=0x08 | 0x0b | 0x0e..=0x1f | 0x7f..=0x9f | 0xfdd0..=0xfdef | 0xfffe | 0xffff
    ) || ((0x1fffe..=0x10ffff).contains(&num) && num & 0xffff >= 0xfffe)
}

/// One scan attempt matching the character reference pattern
/// `&(#[0-9]+;?|#[xX][0-9a-fA-F]+;?|[^\t\n\x0c <&#;]{1,32};?)`.
/// Returns `(reference text, bytes consumed)` — the text without the leading `&`.
fn match_charref(tail: &str) -> Option<(&str, usize)> {
    let body = tail.strip_prefix('&')?;
    if let Some(num) = body.strip_prefix('#') {
        let (digits, radix) = match num.strip_prefix(['x', 'X']) {
            Some(hex) if hex.starts_with(|c: char| c.is_ascii_hexdigit()) => (hex, 16u32),
            _ if num.starts_with(|c: char| c.is_ascii_digit()) => (num, 10u32),
            _ => return None,
        };
        let digit_len = digits
            .find(|c: char| {
                !(radix == 16 && c.is_ascii_hexdigit() || radix == 10 && c.is_ascii_digit())
            })
            .unwrap_or(digits.len());
        let semi = usize::from(digits[digit_len..].starts_with(';'));
        let s_len = 1 + (num.len() - digits.len()) + digit_len + semi;
        return Some((&body[..s_len], s_len + 1));
    }
    let mut count = 0usize;
    let mut end = 0usize;
    for (i, c) in body.char_indices() {
        if count == 32 || matches!(c, '\t' | '\n' | '\x0c' | ' ' | '<' | '&' | '#' | ';') {
            break;
        }
        count += 1;
        end = i + c.len_utf8();
    }
    if count == 0 {
        return None;
    }
    if body[end..].starts_with(';') {
        end += 1;
    }
    Some((&body[..end], end + 1))
}

/// Resolve one matched reference, or echo it back with `&` on failure.
fn replace_charref(s: &str) -> String {
    if let Some(num) = s.strip_prefix('#') {
        let (digits, radix) = match num.strip_prefix(['x', 'X']) {
            Some(hex) => (hex, 16u32),
            None => (num, 10u32),
        };
        let mut value: u64 = 0;
        for c in digits.trim_end_matches(';').chars() {
            value = value
                .saturating_mul(u64::from(radix))
                .saturating_add(u64::from(c.to_digit(radix).unwrap_or(0)));
        }
        if let Some((_, ch)) = INVALID_CHARREFS.iter().find(|(n, _)| *n == value) {
            return ch.to_string();
        }
        if (0xd800..=0xdfff).contains(&value) || value > 0x10ffff {
            return '\u{fffd}'.to_string();
        }
        if invalid_codepoint(value) {
            return String::new();
        }
        return char::from_u32(value as u32).map_or('\u{fffd}'.to_string(), |c| c.to_string());
    }
    if let Some(found) = HTML5.get(s) {
        return found.clone();
    }
    // Longest matching prefix wins.
    for x in (2..s.chars().count()).rev() {
        let split = s.char_indices().nth(x).map_or(s.len(), |(i, _)| i);
        if let Some(found) = HTML5.get(&s[..split]) {
            return format!("{}{}", found, &s[split..]);
        }
    }
    format!("&{s}")
}

/// Replace every character reference a scanner can reach.
pub(super) fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        match match_charref(tail) {
            Some((s, consumed)) => {
                out.push_str(&replace_charref(s));
                rest = &tail[consumed..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

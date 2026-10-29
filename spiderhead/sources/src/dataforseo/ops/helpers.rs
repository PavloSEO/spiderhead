//! Argument types and small value helpers shared by the DataForSEO operation wrappers.
use super::*;

/// Estimate arguments; `None` applies the default.
#[derive(Debug, Default)]
pub struct EstimateArgs<'a> {
    /// `query_count` (`1`).
    pub query_count: Option<&'a Value>,
    /// `row_limit` (`0`).
    pub row_limit: Option<&'a Value>,
    /// `cache_hits` (`0`).
    pub cache_hits: Option<&'a Value>,
    /// `prices` — a supplied `appendix/user_data` price snapshot.
    pub prices: Option<&'a Value>,
    /// `env` — `sandbox`/`prod` selector.
    pub env: Option<&'a Value>,
    /// `query` — required for `serp` search-operator pricing.
    pub query: Option<&'a Value>,
}

/// A count argument: a non-negative integer, not a boolean.
pub(super) fn nonneg_int(name: &str, value: &Value) -> Result<i64, DfsError> {
    match pystr::as_strict_i64(Some(value)) {
        Some(v) if v >= 0 => Ok(v),
        _ => Err(value_err(&format!("{name} must be a non-negative integer"))),
    }
}

/// SERP billing pages: rows divided by ten, rounded up, at least 1.
pub(super) fn serp_pages(row_limit: i64) -> i64 {
    ((row_limit + 9) / 10).max(1)
}

pub(super) fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

/// Decode `+` as space and `%XX` escapes; invalid UTF-8 is replaced.
pub(super) fn unquote_plus(text: &str) -> String {
    let mut bytes = Vec::with_capacity(text.len());
    let raw = text.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        match raw[i] {
            b'+' => bytes.push(b' '),
            b'%' if i + 2 < raw.len()
                && hex_val(raw[i + 1]).is_some()
                && hex_val(raw[i + 2]).is_some() =>
            {
                let hi = hex_val(raw[i + 1]).unwrap_or_default();
                let lo = hex_val(raw[i + 2]).unwrap_or_default();
                bytes.push(hi * 16 + lo);
                i += 2;
            }
            b => bytes.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// A word character: a letter or number in any script, or `_`.
pub(super) fn is_word_char(c: char) -> bool {
    use unicode_general_category::{get_general_category, GeneralCategory::*};
    c == '_'
        || matches!(
            get_general_category(c),
            UppercaseLetter
                | LowercaseLetter
                | TitlecaseLetter
                | ModifierLetter
                | OtherLetter
                | DecimalNumber
                | LetterNumber
                | OtherNumber
        )
}

/// True when the query uses a priced search operator: a keyword at a word boundary followed by `:`.
pub(super) fn search_operator(query: &str) -> bool {
    const WORDS: &[&str] = &[
        "allinanchor",
        "allintext",
        "allintitle",
        "allinurl",
        "cache",
        "define",
        "definition",
        "filetype",
        "id",
        "inanchor",
        "info",
        "intext",
        "intitle",
        "inurl",
        "link",
        "site",
    ];
    let lower = query.to_lowercase();
    // `boundary` = "previous char was not a word char"; position 0 starts as a boundary
    // because a match there can only begin with a word char anyway.
    let mut boundary = true;
    for (index, c) in lower.char_indices() {
        if boundary {
            for word in WORDS {
                if lower[index..].starts_with(word) && lower[index + word.len()..].starts_with(':')
                {
                    return true;
                }
            }
        }
        boundary = !is_word_char(c);
    }
    false
}

/// Round to 10 decimal places, parsed back to `f64`.
pub(super) fn round10(value: f64) -> f64 {
    format!("{value:.10}").parse().unwrap_or(value)
}

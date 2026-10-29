//! IDNA 2003 ToASCII (RFC 3490) with nameprep (RFC 3491).
//!
//! Uses IDNA 2003, not IDNA 2008 or UTS 46, so `url::Host` differs on deviation characters,
//! prohibited code points and label limits. The nameprep tables come from Unicode 3.2 stringprep data.

use icu_normalizer::ComposingNormalizerBorrowed;
use thiserror::Error;

use super::nameprep_data::{B1, B2, D1, D2, NFKC32, PROHIBITED};

/// Why the codec refused a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum IdnaError {
    /// An empty label (not the trailing one), such as `a..b`.
    #[error("label empty")]
    LabelEmpty,
    /// A label of 64 or more bytes after encoding.
    #[error("label too long")]
    LabelTooLong,
    /// A prohibited code point survived nameprep.
    #[error("invalid character")]
    InvalidCharacter,
    /// A label with right-to-left characters breaks the bidirectional rules.
    #[error("violation of BIDI requirement")]
    Bidi,
    /// A non-ASCII label that already starts with `xn--`.
    #[error("label starts with ACE prefix")]
    AcePrefix,
}

fn in_table(table: &[(u32, u32)], c: char) -> bool {
    let cp = u32::from(c);
    table
        .binary_search_by(|&(lo, hi)| {
            if cp < lo {
                std::cmp::Ordering::Greater
            } else if cp > hi {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// RFC 3491 nameprep (stringprep profile): map, NFKC, prohibit, bidi check.
fn nameprep(label: &str) -> Result<String, IdnaError> {
    let mut mapped = String::with_capacity(label.len());
    for c in label.chars() {
        if in_table(B1, c) {
            continue;
        }
        match B2.binary_search_by_key(&u32::from(c), |&(cp, _)| cp) {
            Ok(i) => mapped.push_str(B2[i].1),
            Err(_) => mapped.push(c),
        }
    }
    let label = nfkc_unicode_3_2(&mapped);
    if label.chars().any(|c| in_table(PROHIBITED, c)) {
        return Err(IdnaError::InvalidCharacter);
    }
    let right_to_left: Vec<bool> = label.chars().map(|c| in_table(D1, c)).collect();
    if right_to_left.iter().any(|&r| r)
        && (label.chars().any(|c| in_table(D2, c))
            || !right_to_left.first().copied().unwrap_or(false)
            || !right_to_left.last().copied().unwrap_or(false))
    {
        return Err(IdnaError::Bidi);
    }
    Ok(label)
}

/// NFKC with Unicode 3.2 data, as nameprep requires.
/// Code points that did not exist then, or were re-mapped since, are starters that stay
/// as they are, so they split the string into runs normalized independently.
fn nfkc_unicode_3_2(text: &str) -> String {
    let normalizer = ComposingNormalizerBorrowed::new_nfkc();
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    for c in text.chars() {
        match NFKC32.binary_search_by_key(&u32::from(c), |&(cp, _)| cp) {
            Ok(i) => {
                out.push_str(&normalizer.normalize(&run));
                run.clear();
                out.push_str(NFKC32[i].1);
            }
            Err(_) => run.push(c),
        }
    }
    out.push_str(&normalizer.normalize(&run));
    out
}

/// RFC 3492 punycode encoding of a label.
/// The accumulator is 64-bit: a u32 `delta` overflowed on long labels.
fn punycode(label: &str) -> String {
    const BASE: u64 = 36;
    const T_MIN: u64 = 1;
    const T_MAX: u64 = 26;
    fn digit(d: u64) -> char {
        char::from(if d < 26 {
            b'a' + d as u8
        } else {
            b'0' + (d - 26) as u8
        })
    }
    fn adapt(mut delta: u64, points: u64, first: bool) -> u64 {
        delta = if first { delta / 700 } else { delta / 2 };
        delta += delta / points;
        let mut k = 0;
        while delta > ((BASE - T_MIN) * T_MAX) / 2 {
            delta /= BASE - T_MIN;
            k += BASE;
        }
        k + (BASE - T_MIN + 1) * delta / (delta + 38)
    }
    let chars: Vec<u64> = label.chars().map(u64::from).collect();
    let mut out: String = label.chars().filter(char::is_ascii).collect();
    let basic = out.len() as u64;
    let mut handled = basic;
    if basic > 0 {
        out.push('-');
    }
    let (mut n, mut delta, mut bias) = (128_u64, 0_u64, 72_u64);
    while (handled as usize) < chars.len() {
        let m = chars.iter().copied().filter(|&c| c >= n).min().unwrap_or(n);
        delta += (m - n) * (handled + 1);
        n = m;
        for &c in &chars {
            if c < n {
                delta += 1;
            }
            if c == n {
                let mut q = delta;
                let mut k = BASE;
                loop {
                    let t = k.saturating_sub(bias).clamp(T_MIN, T_MAX);
                    if q < t {
                        break;
                    }
                    out.push(digit(t + (q - t) % (BASE - t)));
                    q = (q - t) / (BASE - t);
                    k += BASE;
                }
                out.push(digit(q));
                bias = adapt(delta, handled + 1, handled == basic);
                delta = 0;
                handled += 1;
            }
        }
        delta += 1;
        n += 1;
    }
    out
}

/// RFC 3490 ToASCII for one label that contains non-ASCII characters or needs a length check.
fn to_ascii_label(label: &str) -> Result<String, IdnaError> {
    let check = |ascii: String| match ascii.len() {
        0 => Err(IdnaError::LabelEmpty),
        1..=63 => Ok(ascii),
        _ => Err(IdnaError::LabelTooLong),
    };
    if label.is_ascii() {
        return check(label.to_owned());
    }
    let label = nameprep(label)?;
    if label.is_ascii() {
        return check(label);
    }
    if label.to_lowercase().starts_with("xn--") {
        return Err(IdnaError::AcePrefix);
    }
    // The encoder emits at least one code unit per input char plus the "xn--" prefix, so a
    // label of 60+ chars can never fit in 63 bytes: reject before running the encoder.
    if label.chars().count() >= 60 {
        return Err(IdnaError::LabelTooLong);
    }
    let encoded = format!("xn--{}", punycode(&label));
    if encoded.len() < 64 {
        Ok(encoded)
    } else {
        Err(IdnaError::LabelTooLong)
    }
}

/// Encode a full domain name to ASCII.
///
/// # Errors
///
/// Returns an [`IdnaError`] when a label is rejected.
pub fn encode_idna(input: &str) -> Result<String, IdnaError> {
    if input.is_empty() {
        return Ok(String::new());
    }
    if input.is_ascii() {
        let labels: Vec<&str> = input.split('.').collect();
        let (last, init) = labels.split_last().unwrap_or((&"", &[]));
        if init.iter().any(|l| l.is_empty()) {
            return Err(IdnaError::LabelEmpty);
        }
        if labels.iter().any(|l| l.len() >= 64) || last.len() >= 64 {
            return Err(IdnaError::LabelTooLong);
        }
        return Ok(input.to_owned());
    }
    let mut labels: Vec<&str> = input
        .split(['.', '\u{3002}', '\u{ff0e}', '\u{ff61}'])
        .collect();
    let trailing_dot = labels.last().is_some_and(|l| l.is_empty());
    if trailing_dot {
        labels.pop();
    }
    let mut encoded = Vec::with_capacity(labels.len());
    for label in labels {
        encoded.push(to_ascii_label(label)?);
    }
    let mut out = encoded.join(".");
    if trailing_dot {
        out.push('.');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Long non-ASCII labels once overflowed a u32 accumulator (panic in debug, wrap in release).
    // Such labels are refused before encoding, and the encoder is 64-bit.
    #[test]
    fn a_long_non_ascii_label_is_too_long_not_a_crash() {
        let label = "あ".repeat(5_000);
        assert_eq!(encode_idna(&label), Err(IdnaError::LabelTooLong));
        assert_eq!(encode_idna(&"あ".repeat(64)), Err(IdnaError::LabelTooLong));
    }

    // A 60+ char label can never fit in 63 bytes once encoded; reject it first.
    #[test]
    fn the_length_bound_is_checked_before_encoding() {
        let mixed = format!("{}字", "a".repeat(59));
        assert_eq!(encode_idna(&mixed), Err(IdnaError::LabelTooLong));
        let fits = format!("{}字", "a".repeat(53));
        assert!(encode_idna(&fits).is_ok());
    }
}

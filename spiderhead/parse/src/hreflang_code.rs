//! Validity of one hreflang value as a language tag.
//!
//! A value is checked subtag by subtag against the ISO 639-1, ISO 15924 and ISO 3166-1
//! registers (UN M.49 three-digit regions are accepted as well). Deprecated aliases are not
//! in the registers, which is what rejects `en-UK`.

use crate::hreflang_tables::{ISO_15924, ISO_3166_1, ISO_639_1};
use crate::py_str::{py_capitalize, py_repr, py_strip};

/// Why a value fails.
const SHAPE_ERROR: &str = "more subtags than language-script-region";

/// UN M.49 region number: `^[0-9]{3}$`, where `$` also matches before one
/// trailing newline.
fn is_m49_region(region: &str) -> bool {
    let core = region.strip_suffix('\n').unwrap_or(region);
    core.len() == 3 && core.bytes().all(|byte| byte.is_ascii_digit())
}

/// Membership in a sorted register.
fn registered(register: &[&str], code: &str) -> bool {
    register.binary_search(&code).is_ok()
}

/// Why an hreflang value is not a valid language tag, or `None` when it is valid.
///
/// The `x-default` value is valid in any case. A value is split on `-` into at most three
/// subtags. The language must be ISO 639-1, an optional four-letter script must be ISO 15924, and
/// an optional region must be ISO 3166-1 or a UN M.49 number. A region that is a UN M.49 number
/// ends the check as valid, so later subtags are not inspected.
pub fn code_error(value: &str) -> Option<String> {
    let tag = py_strip(value);
    if tag.is_empty() {
        return Some("empty value".to_owned());
    }
    if tag.to_lowercase() == "x-default" {
        return None;
    }
    let parts: Vec<&str> = tag.split('-').collect();
    if parts.len() > 3 {
        return Some(SHAPE_ERROR.to_owned());
    }
    let language = parts[0].to_lowercase();
    if !registered(ISO_639_1, &language) {
        return Some(format!(
            "{} is not an ISO 639-1 language code",
            py_repr(parts[0])
        ));
    }
    let mut rest = &parts[1..];
    if let Some(&first) = rest.first() {
        if first.chars().count() == 4 {
            if !registered(ISO_15924, &py_capitalize(first)) {
                return Some(format!(
                    "{} is not an ISO 15924 script code",
                    py_repr(first)
                ));
            }
            rest = &rest[1..];
        }
    }
    if let Some(&region) = rest.first() {
        if is_m49_region(region) {
            return None;
        }
        if !registered(ISO_3166_1, &region.to_uppercase()) {
            return Some(format!(
                "{} is not an ISO 3166-1 alpha-2 or UN M.49 region code",
                py_repr(region)
            ));
        }
        rest = &rest[1..];
    }
    rest.first()
        .map(|extra| format!("unexpected subtag {}", py_repr(extra)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &[&str] = &[
        "en",
        "en-US",
        "EN-US",
        "en-us",
        "pt-BR",
        "zh-Hans",
        "zh-Hant-TW",
        "sr-Cyrl-RS",
        "en-419",
        "x-default",
        "X-Default",
    ];

    #[test]
    fn valid_codes_are_accepted() {
        for value in VALID {
            assert_eq!(code_error(value), None, "{value}");
        }
    }

    #[test]
    fn invalid_codes_are_rejected() {
        let invalid = [
            "xx",
            "eng",
            "en-usa",
            "en-UK",
            "en-XQ",
            "zh-Xxxx-TW",
            "en_US",
            "de-DE-1996",
            "",
        ];
        for value in invalid {
            assert!(code_error(value).is_some(), "{value:?} should be invalid");
        }
    }

    #[test]
    fn the_reason_is_reported_not_just_the_verdict() {
        assert!(code_error("en-UK")
            .unwrap_or_default()
            .contains("ISO 3166-1"));
        assert!(code_error("xx").unwrap_or_default().contains("ISO 639-1"));
    }

    #[test]
    fn messages_quote_the_subtag_as_python_repr_does() {
        assert_eq!(
            code_error("xx").as_deref(),
            Some("'xx' is not an ISO 639-1 language code")
        );
        assert_eq!(code_error(""), Some("empty value".to_owned()));
        assert_eq!(
            code_error("en-US-x").as_deref(),
            Some("unexpected subtag 'x'")
        );
    }

    #[test]
    fn m49_region_ends_the_check_as_valid_even_with_trailing_subtags() {
        assert_eq!(code_error("en-419-zz"), None);
        // "123\n" has four code points, so the script branch runs first and rejects it.
        assert_eq!(
            code_error("en-123\n-US").as_deref(),
            Some("'123\\n' is not an ISO 15924 script code")
        );
    }
}

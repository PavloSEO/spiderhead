//! Strict manifest JSON: duplicate keys, non-JSON numeric constants and
//! unpaired Unicode surrogates are refused instead of silently normalized.

use std::fmt;

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::Value;

use super::ExternalCrawlImportError;

/// A JSON value whose object keys are unique and whose numeric constants are
/// finite — the same guarantees the reference `object_pairs_hook`/`parse_constant`
/// give the manifest.
struct Strict(Value);

impl<'de> Deserialize<'de> for Strict {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(StrictVisitor)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = Strict;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Strict, E> {
        Ok(Strict(Value::Bool(v)))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Strict, E> {
        Ok(Strict(Value::from(v)))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Strict, E> {
        Ok(Strict(Value::from(v)))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Strict, E> {
        if v.is_finite() {
            Ok(Strict(Value::from(v)))
        } else {
            Err(E::custom("non-finite JSON number"))
        }
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Strict, E> {
        Ok(Strict(Value::String(v.to_owned())))
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<Strict, E> {
        Ok(Strict(Value::String(v)))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Strict, E> {
        Ok(Strict(Value::Null))
    }

    fn visit_none<E: de::Error>(self) -> Result<Strict, E> {
        Ok(Strict(Value::Null))
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Strict, D::Error> {
        Strict::deserialize(deserializer)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Strict, A::Error> {
        let mut items = Vec::new();
        while let Some(Strict(item)) = seq.next_element()? {
            items.push(item);
        }
        Ok(Strict(Value::Array(items)))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Strict, A::Error> {
        let mut out = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            // the reference object_pairs_hook quotes the offending key, so the
            // wording stays part of the contract here too.
            if out.contains_key(&key) {
                return Err(de::Error::custom(format!("duplicate JSON key: '{key}'")));
            }
            let Strict(value) = map.next_value()?;
            out.insert(key, value);
        }
        Ok(Strict(Value::Object(out)))
    }
}

/// Scan the decoded manifest text for the constructs reference refuses with a
/// dedicated message and serde cannot express: an unpaired surrogate escape
/// inside a string (serde replaces it with U+FFFD), and a bare `NaN`/`Infinity`
/// numeric constant outside strings.
fn scan_disallowed(text: &str) -> Result<(), ExternalCrawlImportError> {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut in_string = false;
    let mut escaped = false;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                if chars.get(i + 1) == Some(&'u') {
                    if let Some(first) = chars.get(i + 2..i + 6).and_then(hex4) {
                        if (0xD800..=0xDBFF).contains(&first) {
                            let paired = chars.get(i + 6) == Some(&'\\')
                                && chars.get(i + 7) == Some(&'u')
                                && chars
                                    .get(i + 8..i + 12)
                                    .and_then(hex4)
                                    .is_some_and(|low| (0xDC00..=0xDFFF).contains(&low));
                            if !paired {
                                return Err(ExternalCrawlImportError(
                                    "manifest contains an unpaired Unicode surrogate".into(),
                                ));
                            }
                        } else if (0xDC00..=0xDFFF).contains(&first) {
                            return Err(ExternalCrawlImportError(
                                "manifest contains an unpaired Unicode surrogate".into(),
                            ));
                        }
                    }
                }
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
        } else {
            let word: String = chars[i..].iter().take(9).collect();
            for constant in ["-Infinity", "Infinity", "NaN"] {
                if word.starts_with(constant) {
                    return Err(ExternalCrawlImportError(format!(
                        "non-JSON numeric constant: {constant}"
                    )));
                }
            }
        }
        i += 1;
    }
    Ok(())
}

/// Four hex digits at `slice[0..4]`, as their code point.
fn hex4(slice: &[char]) -> Option<u32> {
    if slice.len() < 4 {
        return None;
    }
    u32::from_str_radix(&slice[..4].iter().collect::<String>(), 16).ok()
}

/// Parse one strict manifest document, or name the violation.
pub(super) fn parse(text: &str) -> Result<Value, ExternalCrawlImportError> {
    scan_disallowed(text)?;
    serde_json::from_str::<Strict>(text)
        .map(|Strict(value)| value)
        .map_err(|e| ExternalCrawlImportError(format!("manifest is not valid UTF-8 JSON: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_duplicate_keys_constants_and_surrogates() {
        assert!(parse(r#"{"a": 1, "a": 2}"#)
            .unwrap_err()
            .0
            .starts_with("manifest is not valid UTF-8 JSON: duplicate JSON key: 'a'"));
        assert_eq!(
            parse(r#"{"a": NaN}"#).unwrap_err().0,
            "non-JSON numeric constant: NaN"
        );
        assert_eq!(
            parse(r#"[Infinity]"#).unwrap_err().0,
            "non-JSON numeric constant: Infinity"
        );
        assert_eq!(
            parse(r#"{"s": "\ud800"}"#).unwrap_err().0,
            "manifest contains an unpaired Unicode surrogate"
        );
        assert_eq!(
            parse(r#"{"s": "\udc00"}"#).unwrap_err().0,
            "manifest contains an unpaired Unicode surrogate"
        );
        assert_eq!(
            parse(r#"{"s": "\ud83d"}"#).unwrap_err().0,
            "manifest contains an unpaired Unicode surrogate"
        );
    }

    #[test]
    fn accepts_pairs_and_constants_inside_strings() {
        assert_eq!(
            parse(r#"{"s": "😀", "n": "NaN"}"#).unwrap(),
            serde_json::json!({"s": "\u{1F600}", "n": "NaN"})
        );
    }
}

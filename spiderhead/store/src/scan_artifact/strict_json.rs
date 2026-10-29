//! Strict JSON parsing (duplicate keys refused), byte-stable serialisation, audit and config checks.

use super::*;

// Duplicate object keys are refused rather than silently overwritten.
struct Strict(Value);

impl<'de> Deserialize<'de> for Strict {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(StrictVisitor).map(Strict)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
        Ok(Value::from(v))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
        Ok(Value::from(v))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Value, E> {
        if v.is_finite() {
            Ok(Value::from(v))
        } else {
            Err(E::custom("non-finite JSON number"))
        }
    }

    fn visit_str<E>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        Strict::deserialize(deserializer).map(|s| s.0)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(Strict(item)) = seq.next_element()? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut out = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if out.contains_key(&key) {
                return Err(de::Error::custom(format!("duplicate JSON key: {key}")));
            }
            let Strict(value) = map.next_value()?;
            out.insert(key, value);
        }
        Ok(Value::Object(out))
    }
}

/// Parse strict JSON; the error keeps serde's classification (`is_eof` = truncated input).
pub(super) fn parse_json(text: &str) -> Result<Value, serde_json::Error> {
    serde_json::from_str::<Strict>(text).map(|s| s.0)
}

/// Parse strict JSON (duplicate keys and non-finite numbers refused).
pub fn loads(text: &str, label: &str) -> Result<Value, StoreError> {
    parse_json(text).map_err(|e| bad(format!("{label}: invalid JSON ({e})")))
}

/// Canonical JSON text: sorted keys, non-ASCII kept as-is, with a space after commas and colons.
/// Floats use the shortest round-trip digits (see the float formatter below).
pub fn dump(value: &Value) -> String {
    let mut out = String::new();
    write_python_json(value, &mut out);
    out
}

pub(super) fn write_python_json(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                out.push_str(&i.to_string());
            } else if let Some(u) = n.as_u64() {
                out.push_str(&u.to_string());
            } else {
                out.push_str(&python_float(n.as_f64().unwrap_or(0.0)));
            }
        }
        Value::String(s) => out.push_str(&serde_json::to_string(s).unwrap_or_default()),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_python_json(item, out);
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
                out.push_str(&serde_json::to_string(key).unwrap_or_default());
                out.push_str(": ");
                write_python_json(&map[key.as_str()], out);
            }
            out.push('}');
        }
    }
}

/// Float text: shortest round-trip digits, fixed notation for exponents in [-4, 16).
pub(super) fn python_float(f: f64) -> String {
    if f == 0.0 {
        return if f.is_sign_negative() {
            "-0.0".into()
        } else {
            "0.0".into()
        };
    }
    let sci = format!("{f:e}");
    let (mantissa, exponent) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exponent.parse().unwrap_or(0);
    let negative = mantissa.starts_with('-');
    let digits: String = mantissa
        .trim_start_matches('-')
        .chars()
        .filter(|c| *c != '.')
        .collect();
    let body = if (-4..16).contains(&exp) {
        if exp >= 0 {
            let int_len = (exp + 1) as usize;
            if digits.len() <= int_len {
                format!("{digits}{}.0", "0".repeat(int_len - digits.len()))
            } else {
                format!("{}.{}", &digits[..int_len], &digits[int_len..])
            }
        } else {
            format!("0.{}{digits}", "0".repeat((-exp - 1) as usize))
        }
    } else {
        let mant = if digits.len() > 1 {
            format!("{}.{}", &digits[..1], &digits[1..])
        } else {
            digits
        };
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{mant}e{sign}{:02}", exp.abs())
    };
    if negative {
        format!("-{body}")
    } else {
        body
    }
}

/// Lowercase SHA-256 hex digest of UTF-8 text.
pub fn sha(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

/// Read a file as UTF-8 text under the 64 MiB JSON limit.
pub fn text(path: &Path) -> Result<String, StoreError> {
    let mut raw = Vec::new();
    File::open(path)?
        .take(MAX_JSON_BYTES as u64 + 1)
        .read_to_end(&mut raw)?;
    if raw.len() as i64 > MAX_JSON_BYTES {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        return Err(bad(format!("{name}: exceeds the 64 MiB JSON limit")));
    }
    String::from_utf8(raw).map_err(|e| StoreError::Invalid(format!("utf-8: {e}")))
}

/// Parse and validate an audit document (strict JSON, then the audit schema).
pub fn audit(text: &str) -> Result<Value, StoreError> {
    let document = loads(text, "audit")?;
    validate_audit_document(&document)?;
    Ok(document)
}

/// Validate an already parsed audit without making a second JSON document.
pub fn validate_audit_document(document: &Value) -> Result<(), StoreError> {
    spiderhead_formats::audit_schema::validate_audit_document(document).map_err(|e| bad(e.0))
}

/// Validate the effective configuration: a non-empty object of JSON values with no
/// credential values outside the `REDACTED` marker.
pub fn config(value: &Value) -> Result<Value, StoreError> {
    if !value.as_object().is_some_and(|o| !o.is_empty()) {
        return Err(bad(
            "original effective configuration is required (run.crawl_config or --config)",
        ));
    }
    fn inspect(item: &Value) -> Result<(), StoreError> {
        match item {
            Value::Object(map) => {
                for (key, child) in map {
                    let lowered = key.to_lowercase();
                    let credential = matches!(
                        lowered.as_str(),
                        "authorization"
                            | "cookie"
                            | "set-cookie"
                            | "password"
                            | "token"
                            | "api_key"
                    );
                    if credential && child.as_str() != Some("REDACTED") {
                        return Err(bad(
                            "configuration contains credential values; provide a redacted manifest",
                        ));
                    }
                    inspect(child)?;
                }
                Ok(())
            }
            Value::Array(items) => items.iter().try_for_each(inspect),
            _ => Ok(()),
        }
    }
    inspect(value)?;
    Ok(value.clone())
}

/// Truthiness of a stored SQL/JSON value: null, false, zero, empty text and empty containers are false.
pub(super) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Expected strings use the canonical form: sorted keys, non-ASCII kept, NaN refused.
    #[test]
    fn dump_matches_reference_json_dumps() {
        let nested =
            serde_json::json!({"b": [1, true, null, "x"], "a": {"z": 1.5, "y": "é\n\"q\""}});
        assert_eq!(
            dump(&nested),
            r#"{"a": {"y": "é\n\"q\"", "z": 1.5}, "b": [1, true, null, "x"]}"#
        );
        assert_eq!(
            dump(&serde_json::json!({"e": {}, "l": []})),
            r#"{"e": {}, "l": []}"#
        );
    }

    #[test]
    fn dump_floats_use_python_repr() {
        let floats = serde_json::json!([1e16, 1.5e-5, 0.0001, 123.456, -0.0, 1.0, 2.5e20, 0.1]);
        assert_eq!(
            dump(&floats),
            "[1e+16, 1.5e-05, 0.0001, 123.456, -0.0, 1.0, 2.5e+20, 0.1]"
        );
    }

    #[test]
    fn sha_is_sha256_hex_of_the_dump() {
        let text = dump(&serde_json::json!({"b": 1, "a": [2]}));
        assert_eq!(&sha(&text)[..16], "41ff4104bc489a1b");
    }

    #[test]
    fn loads_refuses_duplicate_keys_and_accepts_plain_json() {
        assert!(loads(r#"{"a": 1, "a": 2}"#, "t").is_err());
        assert_eq!(
            loads(r#"{"a": [1, 2.5]}"#, "t").ok(),
            Some(serde_json::json!({"a": [1, 2.5]}))
        );
    }

    #[test]
    fn utc_timestamp_needs_explicit_zero_offset_and_valid_date() {
        assert!(is_utc_timestamp("2026-10-27T12:34:56.123456Z"));
        assert!(is_utc_timestamp("2026-10-27T12:34:56+00:00"));
        assert!(!is_utc_timestamp("2026-10-27T12:34:56"));
        assert!(!is_utc_timestamp("2026-10-27T12:34:56+03:00"));
        assert!(!is_utc_timestamp("2026-02-30T00:00:00Z"));
    }

    #[test]
    fn config_refuses_credentials_outside_redaction() {
        let ok = serde_json::json!({"http": {"cookie": "REDACTED"}});
        assert!(config(&ok).is_ok());
        let bad_value = serde_json::json!({"http": {"authorization": "Basic abc"}});
        assert!(config(&bad_value).is_err());
        assert!(config(&serde_json::json!({})).is_err());
    }

    #[test]
    fn heading_outline_checks_level_range() {
        assert!(
            heading_outline(&serde_json::json!([{"level": 1, "text": "a", "region": "main"}]))
                .is_ok()
        );
        assert!(
            heading_outline(&serde_json::json!([{"level": 7, "text": "a", "region": "main"}]))
                .is_err()
        );
    }

    #[test]
    fn recovered_detects_the_truncation_note() {
        assert!(recovered(&[
            "pages.jsonl: recovered a truncated final line 3".to_owned()
        ]));
        assert!(!recovered(&[]));
    }

    #[test]
    fn audit_document_runs_the_bundled_schema_check() {
        let error = audit("{}").unwrap_err().to_string();
        assert!(error.starts_with("audit schema: "), "{error}");
    }
}

//! Per-node validation against the bundled vocabulary, with numeric literal checks.

use std::collections::HashSet;

use serde_json::{json, Value};

use super::dates;
use super::nodes::{strip_json, truthy, type_chain, types_of, Node};
use super::Json;
use crate::py_json::str_or_dumps;
use crate::py_str::py_strip;

fn strings(value: &Json) -> Vec<String> {
    value
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Parse a numeric literal the way `float()` does; `None` where it fails.
fn py_float(value: &Json) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        Value::String(s) => py_float_str(s),
        _ => None,
    }
}

/// State for a left-to-right `decimal_digit` scan: the previous char and the
/// start of its `Nd` run. Reusing the start keeps a sequential scan from
/// re-walking the whole run (up to ~50 codepoints) for every character.
#[derive(Default)]
struct NdScan {
    prev: Option<(u32, u32)>,
}

/// Value of a decimal digit (`Nd`) in any script, `None` for other characters.
fn decimal_digit(c: char, scan: &mut NdScan) -> Option<u32> {
    use unicode_general_category::{get_general_category, GeneralCategory::DecimalNumber};
    if c.is_ascii_digit() {
        scan.prev = Some((c as u32, 0x30));
        return c.to_digit(10);
    }
    if get_general_category(c) != DecimalNumber {
        scan.prev = None;
        return None;
    }
    let code = c as u32;
    let start = match scan.prev {
        Some((prev, start)) if prev + 1 == code => start,
        _ => {
            let mut start = code;
            while start > 0
                && char::from_u32(start - 1)
                    .is_some_and(|p| get_general_category(p) == DecimalNumber)
            {
                start -= 1;
            }
            start
        }
    };
    scan.prev = Some((code, start));
    Some((code - start) % 10)
}

fn py_float_str(text: &str) -> Option<f64> {
    let mut scan = NdScan::default();
    let ascii: String = py_strip(text)
        .chars()
        .map(|c| {
            decimal_digit(c, &mut scan)
                .and_then(|d| char::from_digit(d, 10))
                .unwrap_or(c)
        })
        .collect();
    let t = ascii.as_str();
    let (neg, body) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let lower = body.to_lowercase();
    let magnitude = match lower.as_str() {
        "inf" | "infinity" => f64::INFINITY,
        "nan" => f64::NAN,
        _ => {
            let cleaned = underscore_free(body)?;
            if cleaned.is_empty()
                || !cleaned
                    .bytes()
                    .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-'))
                || !cleaned.bytes().any(|b| b.is_ascii_digit())
            {
                return None;
            }
            let fixed = if cleaned.starts_with('.') {
                format!("0{cleaned}")
            } else {
                cleaned
            };
            fixed.parse::<f64>().ok()?
        }
    };
    Some(if neg { -magnitude } else { magnitude })
}

/// Single underscores between digits are allowed in numeric strings.
fn underscore_free(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'_' {
            let prev = i.checked_sub(1).and_then(|j| bytes.get(j));
            let next = bytes.get(i + 1);
            if !(prev.is_some_and(u8::is_ascii_digit) && next.is_some_and(u8::is_ascii_digit)) {
                return None;
            }
        } else {
            out.push(char::from(*b));
        }
    }
    Some(out)
}

fn py_str_of(value: &Json) -> String {
    match value {
        Value::Array(_) | Value::Object(_) => "<container>".to_owned(),
        other => str_or_dumps(other),
    }
}

fn is_datatype(vocab: &Json, name: &str) -> bool {
    vocab["types"]
        .get(name)
        .is_some_and(|t| t.get("datatype").is_some_and(truthy))
}

fn has_datatype_in_chain(vocab: &Json, range: &str) -> bool {
    type_chain(range, vocab)
        .iter()
        .any(|t| is_datatype(vocab, t))
}

/// Whether a literal is valid for the vocabulary's expected ranges.
fn literal_ok(value: &Json, ranges: &[String], vocab: &Json) -> bool {
    if ranges.is_empty() {
        return true;
    }
    for r in ranges {
        if has_datatype_in_chain(vocab, r) {
            match r.as_str() {
                "Number" | "Float" | "Integer" => {
                    if let Some(number) = py_float(value) {
                        if !value.is_boolean()
                            && number.is_finite()
                            && (r != "Integer" || number.fract() == 0.0)
                        {
                            return true;
                        }
                    }
                }
                "Boolean" => {
                    if value.is_boolean()
                        || matches!(value.as_str(), Some("true" | "false" | "True" | "False"))
                    {
                        return true;
                    }
                }
                "Date" => {
                    if dates::date_ok(&py_str_of(value)) {
                        return true;
                    }
                }
                "DateTime" => {
                    let text = py_str_of(value);
                    if (text.contains('T') || text.contains(' '))
                        && dates::datetime_ok(&text.replace('Z', "+00:00"))
                    {
                        return true;
                    }
                }
                "Time" => {
                    if dates::time_ok(&py_str_of(value)) {
                        return true;
                    }
                }
                _ => {
                    if !value.is_null() {
                        return true;
                    }
                }
            }
        }
        if type_chain(r, vocab).iter().any(|t| t == "Enumeration") {
            return true;
        }
    }
    false
}

fn join_limit(list: &[String], n: usize) -> String {
    list.iter().take(n).cloned().collect::<Vec<_>>().join(", ")
}

/// Validate one graph node against the bundled vocabulary.
pub(super) fn check_node(node: &Node, vocab: &Json, known_ids: &HashSet<String>) -> Json {
    let mut errors: Vec<String> = strings(node.get("_markup_errors").unwrap_or(&Value::Null));
    let mut warnings: Vec<String> = strings(node.get("_markup_warnings").unwrap_or(&Value::Null));
    let types = types_of(node);
    let path = node.get("_path").cloned().unwrap_or_else(|| json!("?"));
    let format = node
        .get("_format")
        .cloned()
        .unwrap_or_else(|| json!("jsonld"));
    let id = node.get("@id").cloned().unwrap_or(Value::Null);
    let report = |errors: Vec<String>, warnings: Vec<String>| {
        json!({
            "path": path, "types": types, "id": id, "format": format,
            "errors": errors, "warnings": warnings,
        })
    };

    let supported = node.get("_vocab_supported").is_none_or(truthy);
    if !supported {
        let label = match node.get("_vocab_context") {
            Some(c) if truthy(c) => str_or_dumps(c),
            _ => "unknown vocabulary".to_owned(),
        };
        warnings.push(format!(
            "Node uses unsupported vocabulary {label}; Schema.org validation was skipped"
        ));
        return report(errors, warnings);
    }

    if types.is_empty() {
        let attribute = match node.get("_format").and_then(Value::as_str) {
            Some("microdata") => "itemtype",
            Some("rdfa") => "typeof",
            _ => "@type",
        };
        errors.push(format!(
            "Missing {attribute}; the entity cannot be validated and may be ignored by search engines"
        ));
    }
    let mut chain: Vec<String> = Vec::new();
    for t in &types {
        let Some(entry) = vocab["types"].get(t) else {
            errors.push(format!(
                "Type {t} is not present in the Schema.org vocabulary"
            ));
            continue;
        };
        if let Some(dead) = entry.get("dead").filter(|d| truthy(d)) {
            warnings.push(format!(
                "Type {t} is superseded by {}",
                strings(dead).join(", ")
            ));
        }
        if entry.get("layer").and_then(Value::as_str) == Some("pending") {
            warnings.push(format!(
                "Type {t} is in Schema.org's pending layer and is not yet stable"
            ));
        }
        chain.extend(type_chain(t, vocab));
    }
    let chain_set: HashSet<&String> = chain.iter().collect();

    for (key, value) in node {
        if key.starts_with('@') || key.starts_with('_') {
            continue;
        }
        let Some(prop) = vocab["properties"].get(key) else {
            errors.push(format!(
                "Property {key} is not present in the Schema.org vocabulary"
            ));
            continue;
        };
        if let Some(dead) = prop.get("dead").filter(|d| truthy(d)) {
            warnings.push(format!(
                "Property {key} is superseded by {}",
                strings(dead).join(", ")
            ));
        }
        if prop.get("layer").and_then(Value::as_str) == Some("pending") {
            warnings.push(format!("Property {key} is in Schema.org's pending layer"));
        }
        let domains = strings(&prop["d"]);
        let ranges = strings(&prop["r"]);
        if !chain_set.is_empty()
            && !domains.is_empty()
            && !domains.iter().any(|d| chain_set.contains(d))
        {
            errors.push(format!(
                "Property {key} is not declared for {} (allowed domains include {})",
                types.join("/"),
                join_limit(&domains, 4)
            ));
            continue;
        }
        let list: Vec<&Json> = match value {
            Value::Array(l) => l.iter().collect(),
            other => vec![other],
        };
        for v in list {
            if let Value::Object(obj) = v {
                if obj.contains_key("@value") {
                    if !literal_ok(obj.get("@value").unwrap_or(&Value::Null), &ranges, vocab) {
                        warnings.push(format!(
                            "{key}: typed literal value does not match expected ranges ({})",
                            join_limit(&ranges, 3)
                        ));
                    }
                    continue;
                }
                let sub = types_of(obj);
                if !sub.is_empty() && !ranges.is_empty() {
                    let range_set: HashSet<&String> = ranges.iter().collect();
                    let ok = sub
                        .iter()
                        .any(|s| type_chain(s, vocab).iter().any(|c| range_set.contains(c)));
                    if !ok {
                        warnings.push(format!(
                            "{key}: nested type {} does not match expected ranges {}",
                            sub.join("/"),
                            join_limit(&ranges, 4)
                        ));
                    }
                }
                if sub.is_empty() {
                    if let Some(ref_id) = obj.get("@id") {
                        let known = ref_id.as_str().is_some_and(|s| known_ids.contains(s));
                        if !strip_json(ref_id).is_empty() && !known {
                            errors.push(format!(
                                "{key}: @id reference {} has no entity in the graph",
                                str_or_dumps(ref_id)
                            ));
                        }
                    }
                }
            } else if !literal_ok(v, &ranges, vocab) {
                if ranges.iter().any(|r| has_datatype_in_chain(vocab, r)) {
                    warnings.push(format!(
                        "{key}: literal value does not match expected ranges ({})",
                        join_limit(&ranges, 3)
                    ));
                } else {
                    warnings.push(format!(
                        "{key}: literal value supplied where an object is expected ({}); \
                         Google may accept it, but the entity relationship is lost",
                        join_limit(&ranges, 3)
                    ));
                }
            }
        }
    }
    report(errors, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Digits from several `Nd` runs in one string exercise the cached
    /// run-start path (`prev + 1 == code` inside a run, fresh walk at every
    /// boundary); the result must match `float()`.
    #[test]
    fn py_float_str_multi_script_digits() {
        assert_eq!(py_float_str("١٢٣"), Some(123.0)); // Arabic-Indic U+0660..
        assert_eq!(py_float_str("𝟙𝟚𝟛"), Some(123.0)); // math digits U+1D7CE..
        assert_eq!(py_float_str("۴۵۶"), Some(456.0)); // Extended Arabic-Indic U+06F0..
        assert_eq!(py_float_str("-١٢.٥"), Some(-12.5));
        assert_eq!(py_float_str("1a"), None);
    }

    /// The scan cache must reset across a non-digit so a later digit is not
    /// mistaken for the next codepoint of the previous run.
    #[test]
    fn decimal_digit_run_cache_resets_on_gap() {
        let mut scan = NdScan::default();
        assert_eq!(decimal_digit('١', &mut scan), Some(1));
        assert_eq!(decimal_digit('٢', &mut scan), Some(2)); // cached: same run
        assert_eq!(decimal_digit('x', &mut scan), None); // gap clears the cache
        assert_eq!(decimal_digit('٩', &mut scan), Some(9)); // fresh walk
        assert_eq!(decimal_digit('k', &mut scan), None);
    }
}

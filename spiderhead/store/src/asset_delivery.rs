//! CSS/JS weight and delivery analysis.
//!
//!
//! The pure helpers it calls (`check_cache_lifetime`, `looks_minified`, `find_missing_font_display`,
//! `looks_legacy_transpiled`) are defined in the `checks` crate, which the store may not depend on;
//! private copies live here until that code can be shared.

use crate::bodies::{decode_entity, py_isspace};
use crate::body_diff::splitlines;
use crate::error::StoreError;
use crate::resources::media_matches;
use fancy_regex::Regex as FancyRegex;
use regex::Regex;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::OnceLock;

/// `LONG_CACHE_SECONDS`: a static asset is long-lived from one week.
const LONG_CACHE_SECONDS: u64 = 7 * 24 * 3600;

/// Caps the retained delivery envelope when rendered as ASCII-escaped JSON.
const ENVELOPE_LIMIT: usize = 16 * 1024;

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("static regex"))
}

/// Trim whitespace from both ends; U+001C..U+001F count as whitespace.
fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

fn whitespace_ratio(text: &str) -> f64 {
    let total = text.chars().count();
    if total == 0 {
        return 0.0;
    }
    text.chars().filter(|c| py_isspace(*c)).count() as f64 / total as f64
}

/// Heuristic: minified CSS/JS reads as long lines with little whitespace.
fn looks_minified(text: &str) -> bool {
    let stripped = py_strip(text);
    let mut lines = splitlines(stripped);
    if lines.is_empty() {
        lines.push(stripped);
    }
    let length = stripped.chars().count();
    if length < 200 {
        return lines.len() <= 1 && whitespace_ratio(stripped) < 0.15;
    }
    let average = length as f64 / lines.len() as f64;
    whitespace_ratio(stripped) < 0.15 && average > 200.0
}

/// `@font-face` blocks without `font-display: swap` (or an equivalent), as `{"excerpt": ...}`.
fn find_missing_font_display(css: &str) -> Vec<String> {
    static FACE: OnceLock<Regex> = OnceLock::new();
    static OK: OnceLock<Regex> = OnceLock::new();
    let face = re(&FACE, r"(?i)@font-face[\s\x1c-\x1f]*\{([^}]*)\}");
    let ok = re(
        &OK,
        r"(?i)font-display[\s\x1c-\x1f]*:[\s\x1c-\x1f]*(?:swap|fallback|optional)",
    );
    face.captures_iter(css)
        .filter_map(|c| c.get(1))
        .filter(|block| !ok.is_match(block.as_str()))
        .map(|block| py_strip(block.as_str()).chars().take(200).collect())
        .collect()
}

/// Value of one decimal digit (Unicode category Nd).
fn decimal_digit(c: char) -> Option<u64> {
    use unicode_general_category::{get_general_category, GeneralCategory as G};
    if get_general_category(c) != G::DecimalNumber {
        return None;
    }
    let mut start = c as u32;
    while start > 0 {
        match char::from_u32(start - 1) {
            Some(prev) if get_general_category(prev) == G::DecimalNumber => start -= 1,
            _ => break,
        }
    }
    Some(u64::from(c as u32 - start) % 10)
}

/// Value of a run of Unicode decimal digits; `None` when a character is not one
/// or the value does not fit in `u64`.
fn decimal_value(digits: &str) -> Option<u64> {
    digits.chars().try_fold(0u64, |acc, c| {
        acc.checked_mul(10)?.checked_add(decimal_digit(c)?)
    })
}

fn looks_legacy_transpiled(js: &str) -> bool {
    static LEGACY: OnceLock<Regex> = OnceLock::new();
    re(&LEGACY, r"core-js|regeneratorRuntime|_babelPolyfill|@babel/runtime|Object\.assign[\s\x1c-\x1f]*=[\s\x1c-\x1f]*function").is_match(js)
}

/// Whether a static asset's `Cache-Control` is long-lived.
fn check_cache_lifetime(cache_control: Option<&str>) -> Value {
    static MAX_AGE: OnceLock<FancyRegex> = OnceLock::new();
    let max_age = MAX_AGE.get_or_init(|| {
        FancyRegex::new(r"(?i)(?:^|,)[\s\x1c-\x1f]*max-age[\s\x1c-\x1f]*=[\s\x1c-\x1f]*(\d+)(?=[\s\x1c-\x1f]*(?:,|$))")
            .expect("static regex")
    });
    let value = cache_control.unwrap_or("");
    let lowered = value.to_lowercase();
    let verdict =
        |ok: bool, age: Value, reason: Value| json!({"ok": ok, "max_age": age, "reason": reason});
    if lowered.contains("no-store") || lowered.contains("no-cache") {
        return verdict(
            false,
            json!(0),
            json!("no-store/no-cache on a static asset"),
        );
    }
    let digits = max_age
        .captures(value)
        .ok()
        .flatten()
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_owned());
    let Some(digits) = digits else {
        return verdict(false, Value::Null, json!("no Cache-Control max-age"));
    };
    if digits.chars().count() > 19 {
        return verdict(
            false,
            Value::Null,
            json!("Cache-Control max-age exceeds the supported integer bound"),
        );
    }
    // Digits in any Unicode Nd script count, not just ASCII.
    let Some(age) = decimal_value(&digits) else {
        return verdict(false, Value::Null, json!("no Cache-Control max-age"));
    };
    if lowered.contains("immutable") || age >= LONG_CACHE_SECONDS {
        verdict(true, json!(age), Value::Null)
    } else {
        verdict(
            false,
            json!(age),
            json!("max-age is short for a static asset"),
        )
    }
}

/// Text form of a header cell.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        other => other.to_string(),
    }
}

/// Length of the value's ASCII-escaped JSON text, with ", " and ": " separators.
fn ascii_json_len(value: &Value) -> usize {
    fn go(out: &mut String, v: &Value) {
        match v {
            Value::String(s) => {
                out.push('"');
                for c in s.chars() {
                    match c {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\t' => out.push_str("\\t"),
                        '\u{8}' => out.push_str("\\b"),
                        '\u{c}' => out.push_str("\\f"),
                        c if (' '..='~').contains(&c) => out.push(c),
                        c => {
                            let mut buf = [0u16; 2];
                            for unit in c.encode_utf16(&mut buf) {
                                let _ = write!(out, "\\u{unit:04x}");
                            }
                        }
                    }
                }
                out.push('"');
            }
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    go(out, item);
                }
                out.push(']');
            }
            Value::Object(map) => {
                out.push('{');
                for (i, (k, item)) in map.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    go(out, &Value::String(k.clone()));
                    out.push_str(": ");
                    go(out, item);
                }
                out.push('}');
            }
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Number(n) => match (n.as_i64(), n.as_u64()) {
                (Some(i), _) => out.push_str(&i.to_string()),
                (_, Some(u)) => out.push_str(&u.to_string()),
                _ => out.push_str(&crate::audit_v2::float_repr(n.as_f64().unwrap_or(0.0))),
            },
        }
    }
    let mut out = String::new();
    go(&mut out, value);
    out.len()
}

/// Summarize a completed native resource fetch without retaining source code.
/// `headers` is a JSON array of `[name, value]` pairs.
pub fn resource_delivery_evidence(
    body: &[u8],
    kind: &str,
    headers: &Value,
) -> Result<Map<String, Value>, StoreError> {
    let kind = match kind {
        "module" => "script",
        "css_import" => "stylesheet",
        other => other,
    };
    let mut values: BTreeMap<String, String> = BTreeMap::new();
    let pairs = headers
        .as_array()
        .ok_or_else(|| StoreError::Invalid("headers must be (name, value) pairs".into()))?;
    for pair in pairs {
        let (Some(name), Some(value)) = (pair.get(0), pair.get(1)) else {
            return Err(StoreError::Invalid(
                "headers must be (name, value) pairs".into(),
            ));
        };
        let name = py_str(name).to_lowercase();
        let value = py_str(value);
        let joined: Vec<String> = [values.get(&name).cloned(), Some(value)]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .collect();
        values.insert(name, joined.join(","));
    }
    let content_type = values.get("content-type").map_or("", String::as_str);
    let is_script_or_style = kind == "script" || kind == "stylesheet";
    let is_source = is_script_or_style && media_matches(kind, content_type);
    let text = if is_source {
        decode_entity(body, content_type)?.0
    } else {
        String::new()
    };
    let fonts = if is_source && kind == "stylesheet" {
        find_missing_font_display(&text)
    } else {
        Vec::new()
    };
    let mut sample: Vec<Value> = fonts
        .iter()
        .take(20)
        .map(|e| json!({"excerpt": e}))
        .collect();
    let (shown, count, omitted) = (is_source, fonts.len(), fonts.len().saturating_sub(20));

    let mut evidence = Map::new();
    let build = |sample: &[Value], omitted: usize| -> Map<String, Value> {
        let mut m = Map::new();
        m.insert(
            "cache".into(),
            check_cache_lifetime(values.get("cache-control").map(String::as_str)),
        );
        m.insert(
            "source_state".into(),
            json!(if is_source {
                "complete"
            } else if is_script_or_style {
                "unavailable"
            } else {
                "not_applicable"
            }),
        );
        m.insert(
            "font_display".into(),
            if shown { json!(sample) } else { Value::Null },
        );
        m.insert(
            "font_display_count".into(),
            if shown { json!(count) } else { Value::Null },
        );
        m.insert(
            "font_display_omitted".into(),
            if shown { json!(omitted) } else { Value::Null },
        );
        m.insert(
            "unminified".into(),
            if is_source {
                json!(!text.is_empty() && !looks_minified(&text))
            } else {
                Value::Null
            },
        );
        m.insert(
            "legacy_js".into(),
            if is_source && kind == "script" {
                json!(looks_legacy_transpiled(&text))
            } else {
                Value::Null
            },
        );
        // Exact bytes: stripping whitespace inside string literals can conflate different programs.
        let sha = (is_source && kind == "script" && !body.is_empty()).then(|| {
            Sha256::digest(body)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        });
        m.insert("sha256".into(), json!(sha));
        m
    };
    let mut omitted_now = omitted;
    evidence.extend(build(&sample, omitted_now));
    // The retained validator caps the whole ASCII-escaped envelope, not just the excerpt count.
    if shown {
        while ascii_json_len(&Value::Object(evidence.clone())) > ENVELOPE_LIMIT {
            if sample.pop().is_none() {
                return Err(StoreError::Invalid("pop from empty list".into()));
            }
            omitted_now = count - sample.len();
            evidence = build(&sample, omitted_now);
        }
    }
    Ok(evidence)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> Value {
        Value::Array(pairs.iter().map(|(k, v)| json!([k, v])).collect())
    }

    fn evidence(body: &[u8], kind: &str, pairs: &[(&str, &str)]) -> Map<String, Value> {
        resource_delivery_evidence(body, kind, &headers(pairs)).unwrap()
    }

    #[test]
    fn script_evidence_hashes_exact_bytes_and_flags_legacy_markers() {
        let body = b"var a = 1;\nvar b = require('core-js');\n";
        let e = evidence(
            body,
            "script",
            &[
                ("Content-Type", "application/javascript"),
                ("Cache-Control", "max-age=31536000"),
            ],
        );
        assert_eq!(e["source_state"], "complete");
        assert_eq!(e["legacy_js"], true);
        assert_eq!(e["unminified"], true);
        assert_eq!(e["sha256"], format!("{:x}", Sha256::digest(body)));
        assert_eq!(
            e["cache"],
            json!({"ok": true, "max_age": 31536000, "reason": null})
        );
        assert_eq!(e["font_display"], json!([]));
        assert_eq!(e["font_display_count"], 0);
    }

    #[test]
    fn stylesheet_reports_font_faces_without_font_display() {
        let css = "@font-face { font-family: A; src: url(a.woff2); }\n@font-face { font-family: B; font-display: swap; }\n";
        let e = evidence(
            css.as_bytes(),
            "css_import",
            &[("content-type", "text/css; charset=utf-8")],
        );
        assert_eq!(e["font_display_count"], 1);
        assert_eq!(
            e["font_display"][0]["excerpt"],
            "font-family: A; src: url(a.woff2);"
        );
        assert_eq!(e["font_display_omitted"], 0);
        assert_eq!(e["legacy_js"], Value::Null);
        assert_eq!(e["sha256"], Value::Null);
    }

    #[test]
    fn undeclared_media_is_unavailable_and_other_kinds_not_applicable() {
        let e = evidence(b"x", "script", &[("content-type", "text/plain")]);
        assert_eq!(e["source_state"], "unavailable");
        assert_eq!(e["font_display"], Value::Null);
        assert_eq!(e["unminified"], Value::Null);
        let image = evidence(b"x", "image", &[]);
        assert_eq!(image["source_state"], "not_applicable");
        assert_eq!(image["cache"]["reason"], "no Cache-Control max-age");
        assert_eq!(
            image.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "cache",
                "source_state",
                "font_display",
                "font_display_count",
                "font_display_omitted",
                "unminified",
                "legacy_js",
                "sha256"
            ]
        );
    }

    #[test]
    fn repeated_headers_join_with_commas_and_empty_values_drop() {
        let e = evidence(
            b"a",
            "module",
            &[
                ("content-type", "text/javascript"),
                ("Cache-Control", "public"),
                ("cache-control", ""),
                ("CACHE-CONTROL", "max-age=10"),
            ],
        );
        assert_eq!(
            e["cache"],
            json!({"ok": false, "max_age": 10, "reason": "max-age is short for a static asset"})
        );
        assert_eq!(e["source_state"], "complete");
    }

    #[test]
    fn cache_lifetime_rules() {
        let c = |v: Option<&str>| check_cache_lifetime(v);
        assert_eq!(c(Some("no-store"))["max_age"], 0);
        assert_eq!(c(Some("public, immutable, max-age=5"))["ok"], true);
        assert_eq!(c(Some("max-age=604800"))["ok"], true);
        assert_eq!(c(Some("max-age=604799"))["ok"], false);
        assert_eq!(c(Some("s-maxage=999999, max-age = 7"))["max_age"], 7);
        assert_eq!(
            c(Some("max-age=99999999999999999999"))["reason"],
            "Cache-Control max-age exceeds the supported integer bound"
        );
        assert_eq!(c(Some("max-age=1x"))["max_age"], Value::Null);
        assert_eq!(c(None)["reason"], "no Cache-Control max-age");
    }

    #[test]
    fn minified_heuristic() {
        assert!(looks_minified("a{b:c}"));
        assert!(!looks_minified("a {\n  b: c;\n}\n"));
        assert!(looks_minified(&"x".repeat(300)));
        assert!(!looks_minified(&"line of code\n".repeat(40)));
    }

    #[test]
    fn envelope_is_capped_by_dropping_excerpts() {
        let faces: String = (0..30)
            .map(|i| {
                format!(
                    "@font-face {{ font-family: F{i}; src: url({}) }}\n",
                    "\u{4e2d}".repeat(190)
                )
            })
            .collect();
        let e = evidence(
            faces.as_bytes(),
            "stylesheet",
            &[("content-type", "text/css")],
        );
        let kept = e["font_display"].as_array().unwrap().len();
        assert_eq!(kept, 14);
        assert_eq!(e["font_display_count"], 30);
        assert_eq!(e["font_display_omitted"], 30 - kept);
        assert!(ascii_json_len(&Value::Object(e)) <= ENVELOPE_LIMIT);
    }
}

//! Whole-corpus sitemap parity: every document in
//! `tests/fixtures/sitemap_oracle.json` (recorded by the reference suite
//! `parse_sitemap` and structurally diffed, plus the URL-helper table.

use serde_json::Value;
use spiderhead_crawl::sitemap;

fn fixture() -> Value {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sitemap_oracle.json");
    // the reference json module emits NaN/Infinity literals for float('nan') etc.;
    // swap them for strings on both sides of the comparison.
    let raw = std::fs::read_to_string(path).expect("fixture file");
    let raw = raw
        .replace(": NaN", ": \"NaN\"")
        .replace(": Infinity", ": \"Infinity\"")
        .replace(": -Infinity", ": \"-Infinity\"");
    serde_json::from_str(&raw).expect("fixture json")
}

/// Normalise one answer the way the comparison needs: special float values to
/// their string names, everything else as parsed.
fn normalize(value: &Value) -> Value {
    match value {
        Value::Number(number) => {
            let f = number.as_f64().unwrap_or_default();
            if f.is_nan() {
                Value::String("NaN".into())
            } else if f.is_infinite() {
                Value::String(if f > 0.0 {
                    "Infinity".into()
                } else {
                    "-Infinity".into()
                })
            } else {
                value.clone()
            }
        }
        Value::Object(map) => {
            let out: serde_json::Map<String, Value> =
                map.iter().map(|(k, v)| (k.clone(), normalize(v))).collect();
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(normalize).collect()),
        other => other.clone(),
    }
}

fn diffs(path: &str, expected: &Value, actual: &Value, out: &mut Vec<String>) {
    match (expected, actual) {
        (Value::Object(want), Value::Object(got)) => {
            for (key, value) in want {
                match got.get(key) {
                    Some(actual) => diffs(&format!("{path}/{key}"), value, actual, out),
                    None => out.push(format!("{path}/{key}: missing in rust")),
                }
            }
            for key in got.keys() {
                if !want.contains_key(key) {
                    out.push(format!("{path}/{key}: extra in rust"));
                }
            }
        }
        (Value::Array(want), Value::Array(got)) => {
            if want.len() != got.len() {
                out.push(format!(
                    "{path}: {} items vs rust {}",
                    want.len(),
                    got.len()
                ));
            }
            for (index, (w, g)) in want.iter().zip(got.iter()).enumerate() {
                diffs(&format!("{path}[{index}]"), w, g, out);
            }
        }
        (Value::Number(want), Value::Number(got)) => {
            let (w, g) = (
                want.as_f64().unwrap_or_default(),
                got.as_f64().unwrap_or_default(),
            );
            if w != g {
                out.push(format!("{path}: python {w} vs rust {g}"));
            }
        }
        (want, got) => {
            if want != got {
                out.push(format!(
                    "{path}: python {} vs rust {}",
                    serde_json::to_string(want).unwrap_or_default(),
                    serde_json::to_string(got).unwrap_or_default()
                ));
            }
        }
    }
}

#[test]
fn sitemap_corpus_matches_the_reference_oracle() {
    let fixture = fixture();
    let cases = fixture["cases"].as_array().expect("cases");
    let mut failures: Vec<String> = Vec::new();
    for case in cases {
        let name = case["name"].as_str().expect("case name");
        let body = decode_hex(case["body"].as_str().expect("body hex"));
        let base = case["base"].as_str().expect("base");
        let expected = normalize(&case["answer"]);
        let actual = normalize(&sitemap::parse_sitemap(&body, base));
        let mut problems = Vec::new();
        diffs(name, &expected, &actual, &mut problems);
        if !problems.is_empty() {
            failures.push(format!("{}:\n  {}", name, problems.join("\n  ")));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} sitemap cases diverge:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}

#[test]
fn url_helpers_match_the_reference_oracle() {
    let fixture = fixture();
    let helpers = &fixture["helpers"];

    for (index, expected) in helpers["normalize"]
        .as_array()
        .expect("normalize")
        .iter()
        .enumerate()
    {
        let input = NORMALIZE_INPUTS[index];
        let expected = expected.as_str().expect("string");
        let actual = match sitemap::normalize_url(input) {
            Ok(url) => url,
            Err(message) => format!("<ValueError: {message}>"),
        };
        assert_eq!(actual, expected, "normalize_url({input:?})");
    }
    for (index, expected) in helpers["classify"]
        .as_array()
        .expect("classify")
        .iter()
        .enumerate()
    {
        let actual = sitemap::classify_url_type(CLASSIFY_INPUTS[index]);
        assert_eq!(
            actual,
            expected.as_str().expect("string"),
            "classify_url_type({:?})",
            CLASSIFY_INPUTS[index]
        );
    }
    for (index, expected) in helpers["policy"]
        .as_array()
        .expect("policy")
        .iter()
        .enumerate()
    {
        let locs: Vec<Value> = POLICY_INPUTS[index]
            .iter()
            .map(|loc| serde_json::json!({"loc": loc}))
            .collect();
        let actual = sitemap::detect_host_policy(&locs);
        assert_eq!(
            normalize(&actual),
            normalize(expected),
            "detect_host_policy(#{index})"
        );
    }
    for (index, expected) in helpers["strip"]
        .as_array()
        .expect("strip")
        .iter()
        .enumerate()
    {
        let actual = sitemap::strip_www(STRIP_INPUTS[index]);
        assert_eq!(
            actual,
            expected.as_str().expect("string"),
            "strip_www({:?})",
            STRIP_INPUTS[index]
        );
    }
}

fn decode_hex(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).expect("hex byte"))
        .collect()
}

/// The helper inputs, mirrored from the reference suite (the fixture stores
/// only the answers).
const NORMALIZE_INPUTS: [&str; 24] = [
    "https://Example.com:443/path/",
    "http://example.com:80/a",
    "https://example.com:8443/a",
    "HTTPS://EXAMPLE.COM/UP",
    "https://user:pw@example.com/x",
    "https://example.com",
    "https://example.com/",
    "https://example.com/a//b/",
    "https://example.com/дуб",
    "https://example.com/a?b=1&c=2",
    "https://example.com/a#frag",
    "https://example.com/a?q=1#frag",
    "  https://example.com/spaced  ",
    "https://[2001:db8::1]:443/x",
    "https://[2001:db8::1]:8080/x",
    "not a url",
    "/relative/only",
    "ftp://example.com/file",
    "https://example.com:99999/x",
    "https://example.com:0/x",
    "https://xn--80ak6aa92e.com/путь",
    "https://example.com/%2f/a",
    "https://example.com/./dot",
    "https://example.com/../up",
];

const CLASSIFY_INPUTS: [&str; 14] = [
    "https://example.com/",
    "https://example.com/page.html",
    "https://example.com/blog",
    "https://example.com/blog/",
    "https://example.com/a.b/c",
    "https://example.com/no-ext",
    "https://example.com/.hidden",
    "https://example.com/x.",
    "not a url",
    "/relative",
    "https://",
    "",
    "https://example.com/deep/path/file.PDF",
    "https://example.com/trailing.dot.",
];

const POLICY_INPUTS: [&[&str]; 7] = [
    &["https://www.example.com/a", "https://example.com/b"],
    &["https://www.example.com/a", "https://www.example.com/b"],
    &["https://example.com/a"],
    &[],
    &[
        "https://www.a.com/x",
        "https://a.com/y",
        "https://www.a.com/z",
        "https://WWW.A.COM/w",
    ],
    &["https://portal.a.com/x", "https://a.com/y"],
    &["garbage"],
];

const STRIP_INPUTS: [&str; 5] = [
    "www.example.com",
    "example.com",
    "www.www.example.com",
    "",
    "www.",
];

//! Differential test: `parse_html` against the reference parser, field by field.
//!
//! Fixture `fixtures/parse_oracle.json` (generator the reference suite) holds the full
//! `ParsedPage` dict the oracle produced for every document under several option sets; a case
//! that raised in reference must be an error here. `PARSE_FIXTURE` overrides the fixture path and
//! `PARSE_SHOW` the number of reported divergences.

use serde_json::Value;
use spiderhead_parse::parse_html;
use spiderhead_parse::types::ParseOptions;

fn walk(expected: &Value, actual: &Value, path: &str, out: &mut Vec<String>) {
    match (expected, actual) {
        (Value::Object(x), Value::Object(y)) => {
            for (key, ev) in x {
                match y.get(key) {
                    Some(av) => walk(ev, av, &format!("{path}/{key}"), out),
                    None => out.push(format!("{path}/{key}: missing, want {}", short(ev))),
                }
            }
            for (key, av) in y {
                if !x.contains_key(key) {
                    out.push(format!("{path}/{key}: extra {}", short(av)));
                }
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                out.push(format!(
                    "{path}: length want {} got {}\n    want {}\n    got  {}",
                    x.len(),
                    y.len(),
                    short(expected),
                    short(actual)
                ));
                return;
            }
            for (i, (ev, av)) in x.iter().zip(y).enumerate() {
                walk(ev, av, &format!("{path}/{i}"), out);
            }
        }
        _ if expected == actual => {}
        _ => out.push(format!(
            "{path}: want {} got {}",
            short(expected),
            short(actual)
        )),
    }
}

fn short(value: &Value) -> String {
    let text = value.to_string();
    if text.chars().count() > 240 {
        format!("{}...", text.chars().take(240).collect::<String>())
    } else {
        text
    }
}

#[test]
fn parse_html_matches_the_reference_parser() {
    let text = match std::env::var("PARSE_FIXTURE") {
        Ok(path) => std::fs::read_to_string(path).unwrap_or_default(),
        Err(_) => include_str!("fixtures/parse_oracle.json").to_owned(),
    };
    let rows: Vec<Value> = serde_json::from_str(&text).unwrap_or_default();
    let synthetic = rows
        .iter()
        .filter(|r| !r["name"].as_str().unwrap_or_default().starts_with("site:"))
        .count();
    assert!(
        std::env::var("PARSE_FIXTURE").is_ok() || synthetic >= 150,
        "fixture too small: {synthetic} synthetic documents"
    );
    let mut bad: Vec<String> = Vec::new();
    let mut cases = 0;
    for row in &rows {
        let name = row["name"].as_str().unwrap_or_default();
        let html = row["html"].as_str().unwrap_or_default();
        let Some(pages) = row["pages"].as_object() else {
            continue;
        };
        for (set, case) in pages {
            cases += 1;
            let options: ParseOptions =
                serde_json::from_value(case["options"].clone()).unwrap_or_default();
            let url = case["url"].as_str().unwrap_or_default();
            let got = parse_html(html, url, &options);
            if case.get("raised").is_some() {
                if got.is_ok() {
                    bad.push(format!(
                        "{name} [{set}]: reference raised {}, Rust returned a page",
                        case["raised"]
                    ));
                }
                continue;
            }
            let page = match got {
                Ok(page) => serde_json::to_value(page).unwrap_or(Value::Null),
                Err(err) => {
                    bad.push(format!("{name} [{set}]: Rust error {err}"));
                    continue;
                }
            };
            let mut diffs = Vec::new();
            walk(&case["page"], &page, "", &mut diffs);
            for d in diffs {
                bad.push(format!("{name} [{set}] {d}"));
            }
        }
    }
    let limit = std::env::var("PARSE_SHOW")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(25);
    assert!(
        bad.is_empty(),
        "{} divergences in {cases} cases:\n{}",
        bad.len(),
        bad.iter()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

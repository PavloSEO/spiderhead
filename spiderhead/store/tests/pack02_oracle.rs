//! Differential test of the retained content search (`storage-pack-02`): the reference baseline's
//! records over the raw scans, captured by the reference suite. Cases that reach a stub of another
//! slice (scan validation, body reader) are reported as blocked, not passed.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;

use serde_json::{json, Map, Value};
use spiderhead_store::content_search::{
    line_starts, match_window, occurrence_spans, redaction_spans, search_scan, snippet,
    SearchOptions, Text,
};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../")
}

fn fixture() -> Value {
    let text =
        std::fs::read_to_string(root().join("spiderhead/store/tests/fixtures/pack02-search.json"))
            .unwrap();
    serde_json::from_str(&text).unwrap()
}

fn normalise(value: &Value) -> Value {
    match value {
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        if k == "search_id" {
                            json!("<ID>")
                        } else {
                            normalise(v)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(normalise).collect()),
        other => other.clone(),
    }
}

fn blocked(payload: &(dyn std::any::Any + Send)) -> bool {
    let text = payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default();
    text.contains("slice ")
}

#[test]
fn searches_match_the_reference_oracle() {
    let mut blocked_cases = 0;
    let mut checked = 0;
    for case in fixture()["cases"].as_array().unwrap() {
        let call = case["call"].as_object().unwrap();
        let query = call["query"].as_str().unwrap();
        let mut options = SearchOptions::default();
        if let Some(v) = call.get("scope").and_then(Value::as_str) {
            options.scope = v.into();
        }
        if let Some(v) = call.get("mode").and_then(Value::as_str) {
            options.mode = v.into();
        }
        if let Some(v) = call.get("kind").and_then(Value::as_str) {
            options.kind = v.into();
        }
        if let Some(v) = call.get("representations").and_then(Value::as_array) {
            options.representations = v.iter().map(|x| x.as_str().unwrap().to_string()).collect();
        }
        options.selector = call
            .get("selector")
            .and_then(Value::as_str)
            .map(str::to_string);
        options.url = call.get("url").and_then(Value::as_str).map(str::to_string);
        options.case_sensitive = call.get("case_sensitive") == Some(&json!(true));
        options.include_snippets = call.get("include_snippets") == Some(&json!(true));
        let scan = root()
            .join("oracle/raw-scans")
            .join(case["site"].as_str().unwrap())
            .join("scan.sqlite");
        let mut records: Vec<Value> = Vec::new();
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let mut collect = |r: &Map<String, Value>| -> Option<bool> {
                records.push(Value::Object(r.clone()));
                None
            };
            search_scan(&scan, query, &options, Some(&mut collect))
        }));
        match (outcome, case.get("error")) {
            (Ok(Ok(result)), None) => {
                assert_eq!(
                    normalise(&Value::Object(result)),
                    case["result"],
                    "{}",
                    case["call"]
                );
                assert_eq!(
                    normalise(&Value::Array(records)),
                    case["records"],
                    "records of {}",
                    case["call"]
                );
                checked += 1;
            }
            (Ok(Err(e)), Some(message)) => {
                assert_eq!(
                    &Value::String(e.to_string().replace("invalid artifact: ", "")),
                    message,
                    "{}",
                    case["call"]
                );
                checked += 1;
            }
            (Err(p), _) if blocked(p.as_ref()) => blocked_cases += 1,
            (Err(p), _) => std::panic::resume_unwind(p),
            (other, error) => panic!("{} -> {other:?} vs {error:?}", case["call"]),
        }
    }
    eprintln!("compared {checked} case(s); blocked by other slices: {blocked_cases}");
    assert_eq!(blocked_cases, 0, "every case must reach real code");
}

#[test]
fn helpers_match_the_reference_oracle() {
    let helpers = &fixture()["helpers"];
    for case in helpers["line_starts"].as_array().unwrap() {
        let starts = line_starts(&Text::new(case["text"].as_str().unwrap()));
        assert_eq!(json!(starts), case["starts"], "{}", case["text"]);
    }
    for case in helpers["snippets"].as_array().unwrap() {
        let text = case["text"].as_str().unwrap();
        let spans = redaction_spans(text);
        assert_eq!(json!([spans.0, spans.1]), case["redaction"], "{text}");
        assert_eq!(
            json!(snippet(
                &Text::new(text),
                "token",
                false,
                None,
                Some(&spans)
            )),
            case["snippet"],
            "{text}"
        );
    }
    for case in helpers["windows"].as_array().unwrap() {
        let text = case["text"].as_str().unwrap();
        let chars = Text::new(text);
        let starts = line_starts(&chars);
        let span = (
            case["span"][0].as_u64().unwrap() as usize,
            case["span"][1].as_u64().unwrap() as usize,
        );
        let (line, window, truncated) = match_window(&chars, &starts, span, &redaction_spans(text));
        assert_eq!(
            json!([line, window, truncated]),
            case["result"],
            "{text} {span:?}"
        );
    }
    for case in helpers["spans"].as_array().unwrap() {
        let found = occurrence_spans(
            case["text"].as_str().unwrap(),
            case["query"].as_str().unwrap(),
            case["case_sensitive"] == json!(true),
            None,
        );
        assert_eq!(json!(found.spans), case["spans"], "{}", case["text"]);
        assert_eq!(json!(found.total), case["total"]);
    }
}

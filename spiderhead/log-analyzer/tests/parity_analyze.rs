//! One-shot analysis parity over the synthetic log corpus, including offline DNS.

mod common;

use std::sync::Arc;

use serde_json::{json, Value};
use spiderhead_log_analyzer::analyze::{analyze_log, AnalyzeOptions};

use common::{assert_same, load, log_path, verifier, FakeResolver};

fn options(map: &Value) -> AnalyzeOptions {
    let mut o = AnalyzeOptions::default();
    let int = |k: &str| map.get(k).and_then(Value::as_i64);
    o.max_lines = int("max_lines").unwrap_or(o.max_lines);
    o.sample_size = int("sample_size").unwrap_or(o.sample_size);
    o.limit = int("limit").unwrap_or(o.limit);
    o.offset = int("offset").unwrap_or(o.offset);
    o.max_dns = int("max_dns").unwrap_or(o.max_dns);
    o.verify_bots = map
        .get("verify_bots")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    o.bot = map.get("bot").and_then(Value::as_str).map(String::from);
    o.ip = map.get("ip").and_then(Value::as_str).map(String::from);
    o
}

fn run(case: &Value, resolver: Arc<FakeResolver>) -> Value {
    let v = verifier(resolver);
    match analyze_log(
        &log_path(case["file"].as_str().unwrap()),
        &options(&case["options"]),
        &v,
    ) {
        Ok(value) => value,
        Err(e) => json!({"exception": "ValueError", "message": e.to_string()}),
    }
}

/// Failures that name an error from the decompressor differ in wording only.
const WORDING: [&str; 2] = ["truncated.gz", "corrupt.gz"];

#[test]
fn offline_analysis_matches() {
    for case in load("analyze_cases.json").as_array().unwrap() {
        let name = case["file"].as_str().unwrap();
        let expected = &case["expected"];
        let actual = run(case, Arc::new(FakeResolver::standard()));
        let label = format!("{name} {}", case["options"]);
        if let Some(value) = expected.get("value") {
            let mut want = value.clone();
            let mut got = actual.clone();
            if WORDING.contains(&name) {
                assert_eq!(want["ok"], got["ok"], "{label}");
                continue;
            }
            for v in [&mut want, &mut got] {
                if let Some(o) = v.as_object_mut() {
                    o.remove("path");
                }
            }
            assert_same(&label, &want, &got, &[]);
        } else {
            assert_eq!(expected["message"], actual["message"], "{label}");
        }
    }
}

#[test]
fn verified_analysis_matches_with_offline_dns() {
    for case in load("verify_cases.json").as_array().unwrap() {
        let mut resolver = FakeResolver::standard();
        let blocked = case["lookups"].is_null();
        resolver.blocked = blocked;
        let resolver = Arc::new(resolver);
        let actual = run(case, Arc::clone(&resolver));
        let label = format!("{} {}", case["file"], case["options"]);
        let mut want = case["expected"].clone();
        let mut got = actual;
        for v in [&mut want, &mut got] {
            v.as_object_mut().unwrap().remove("path");
        }
        assert_same(
            &label,
            &want,
            &got,
            &[
                "/bot_ips/rows/*/checked_at",
                "/verification/checks/*/checked_at",
            ],
        );
        if !blocked {
            assert_eq!(json!(resolver.lookups()), case["lookups"], "{label}");
        }
    }
}

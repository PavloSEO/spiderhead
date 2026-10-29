//! Oracle parity for `segments`: every case of `tests/fixtures/segments.json`
//! results, or the exact error text.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use spiderhead_sf_import::segments as seg;

fn cases() -> Vec<Value> {
    serde_json::from_str(include_str!("fixtures/segments.json")).unwrap()
}

/// `{ok}` / `{error: "SegmentError: text"}` like the generator's capture.
fn capture(r: Result<Value, seg::SegmentError>) -> Value {
    match r {
        Ok(v) => json!({"ok": v}),
        Err(e) => json!({"error": format!("SegmentError: {}", e.0)}),
    }
}

fn same(name: &str, what: &str, got: &Value, want: &Value) {
    // A regex dialect error carries engine-specific text; only its presence is shared.
    if name == "bad_regex" {
        assert!(
            got.get("error").is_some() && want.get("error").is_some(),
            "{name} {what}"
        );
        return;
    }
    assert_eq!(got, want, "{name}: {what}");
}

#[test]
fn segments_match_oracle() {
    for case in cases() {
        let name = case["name"].as_str().unwrap();
        let segments = case["segments"].as_array().unwrap();
        let pages = case["pages"].as_array().unwrap();
        let issues = case["issues"].as_array().unwrap();
        same(
            name,
            "assign",
            &capture(seg::assign_segments(pages, segments)),
            &case["assign"],
        );
        same(
            name,
            "report",
            &capture(seg::segment_report(pages, issues, segments)),
            &case["report"],
        );
        let required = seg::required_fields(segments).map(|m| {
            json!(m
                .into_iter()
                .map(|(k, v)| (k, v.into_iter().collect::<Vec<_>>()))
                .collect::<BTreeMap<_, _>>())
        });
        same(name, "required", &capture(required), &case["required"]);
    }
}

#[test]
fn report_sums_to_ungrouped_totals() {
    let segments = vec![json!({"name": "blog", "rules": [{"op": "prefix", "value": "/blog"}]})];
    let pages = vec![
        json!({"url": "/blog/a"}),
        json!({"url": "/x"}),
        json!({"url": "/y"}),
    ];
    let issues = vec![
        json!({"check": "A", "target_url": "/blog/a"}),
        json!({"check": "A"}),
    ];
    let report = seg::segment_report(&pages, &issues, &segments).unwrap();
    let total: i64 = report["pages_by_segment"]
        .as_object()
        .unwrap()
        .values()
        .map(|v| v.as_i64().unwrap())
        .sum();
    assert_eq!(total, 3);
    assert_eq!(report["totals"], json!({"pages": 3, "issues": 2}));
}

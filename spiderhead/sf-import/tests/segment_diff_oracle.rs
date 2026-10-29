//! Oracle parity for `segment_diff`: each case of `tests/fixtures/segment_diff.json`
//! recorded answers of the crawl scope's `segment_for` / `rejection`, so the replay
//! answers from the same table the oracle saw.

use serde_json::{json, Value};
use spiderhead_sf_import::segment_diff as sd;

#[test]
fn segment_diff_matches_oracle() {
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/segment_diff.json")).unwrap();
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let table = |key: &str, url: &str| -> String {
            case[key][url]
                .as_str()
                .unwrap_or_else(|| panic!("{name}: no recorded {key} answer for {url:?}"))
                .to_owned()
        };
        let only: Vec<String> = case["segments_only"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect();
        let got = sd::diff_segments(
            case["pages"].as_array().unwrap(),
            case["source"].as_str().unwrap(),
            case["target"].as_str().unwrap(),
            case["segments"].as_array().unwrap(),
            &|u| table("segment_for", u),
            Some(&|u| table("rejection", u)),
            &only,
            case["partial"].as_bool().unwrap(),
        );
        let got = match got {
            Ok(v) => json!({"ok": v}),
            Err(e) => json!({"error": format!("SegmentDiffError: {}", e.0)}),
        };
        assert_eq!(got, case["result"], "{name}");
    }
}

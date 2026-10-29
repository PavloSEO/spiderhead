//! `mine` stage parity against the reference baseline fixture
//! (the reference suite → `tests/fixtures/mine_oracle.json`).

mod oracle;

use serde_json::json;
use spiderhead_semantics::stages;

#[test]
fn mine_matches_reference_oracle() {
    let fx = oracle::fixture("mine");
    for scenario in fx["scenarios"].as_array().unwrap() {
        let name = scenario["name"].as_str().unwrap();
        let (store, dir) = oracle::setup(scenario);
        let cfg = oracle::cfg_of(scenario, &dir);
        let args = &scenario["args"];
        let out_dir = dir.join(args["out_dir"].as_str().unwrap_or("report"));
        let mut pages = oracle::pages_of(scenario);
        let result = stages::mine::run(
            &store,
            &cfg,
            &out_dir,
            args["top_urls"].as_i64().unwrap_or(60),
            &mut pages,
        )
        .unwrap();
        let mut diffs = Vec::new();
        oracle::check_result(scenario, &result, &mut diffs);
        oracle::check_tables(&store, scenario, &mut diffs);
        oracle::check_files(scenario, &dir, &mut diffs);
        let calls = json!(pages.calls);
        let want_calls = &scenario["expect"]["calls"]["pages"];
        if !want_calls.is_null() && &calls != want_calls {
            diffs.push(format!("pages calls: rust={calls} oracle={want_calls}"));
        }
        assert!(diffs.is_empty(), "scenario {name}:\n{}", diffs.join("\n"));
    }
}

//! `mine` stage golden check against the reference baseline fixture
//! (the reference suite → `tests/fixtures/mine_oracle.json`).

mod golden;

use serde_json::json;
use spiderhead_semantics::stages;

#[test]
fn golden_mine() {
    let fx = golden::fixture("mine");
    for scenario in fx["scenarios"].as_array().unwrap() {
        let name = scenario["name"].as_str().unwrap();
        let (store, dir) = golden::setup(scenario);
        let cfg = golden::cfg_of(scenario, &dir);
        let args = &scenario["args"];
        let out_dir = dir.join(args["out_dir"].as_str().unwrap_or("report"));
        let mut pages = golden::pages_of(scenario);
        let result = stages::mine::run(
            &store,
            &cfg,
            &out_dir,
            args["top_urls"].as_i64().unwrap_or(60),
            &mut pages,
        )
        .unwrap();
        let mut diffs = Vec::new();
        golden::check_result(scenario, &result, &mut diffs);
        golden::check_tables(&store, scenario, &mut diffs);
        golden::check_files(scenario, &dir, &mut diffs);
        let calls = json!(pages.calls);
        let want_calls = &scenario["expect"]["calls"]["pages"];
        if !want_calls.is_null() && &calls != want_calls {
            diffs.push(format!("pages calls: rust={calls} golden={want_calls}"));
        }
        assert!(diffs.is_empty(), "scenario {name}:\n{}", diffs.join("\n"));
    }
}

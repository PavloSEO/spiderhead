//! `collect` stage golden check against the reference baseline fixture
//! (the reference suite → `tests/fixtures/collect_oracle.json`).

mod golden;

use spiderhead_semantics::stages;

#[test]
fn golden_collect() {
    let fx = golden::fixture("collect");
    for scenario in fx["scenarios"].as_array().unwrap() {
        let name = scenario["name"].as_str().unwrap();
        let (store, dir) = golden::setup(scenario);
        let cfg = golden::cfg_of(scenario, &dir);
        let args = &scenario["args"];
        let seeds: Vec<String> = args["seeds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_string())
            .collect();
        let mut demand = golden::demand_of(scenario);
        let result = stages::collect::run(
            &store,
            &cfg,
            &seeds,
            args["max_seeds"].as_i64().unwrap_or(0),
            args["max_phrases"].as_i64().unwrap_or(100000),
            args["resume"].as_bool().unwrap_or(false),
            &mut demand,
        )
        .unwrap();
        let mut diffs = Vec::new();
        golden::check_result(scenario, &result, &mut diffs);
        let calls = golden::demand_calls(&demand);
        let want_calls = &scenario["expect"]["calls"]["demand"];
        if &calls != want_calls {
            diffs.push(format!("demand calls: rust={calls} golden={want_calls}"));
        }
        golden::check_tables(&store, scenario, &mut diffs);
        assert!(diffs.is_empty(), "scenario {name}:\n{}", diffs.join("\n"));
    }
}

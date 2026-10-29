//! `collect` stage parity against the reference baseline fixture
//! (the reference suite → `tests/fixtures/collect_oracle.json`).

mod oracle;

use spiderhead_semantics::stages;

#[test]
fn collect_matches_reference_oracle() {
    let fx = oracle::fixture("collect");
    for scenario in fx["scenarios"].as_array().unwrap() {
        let name = scenario["name"].as_str().unwrap();
        let (store, dir) = oracle::setup(scenario);
        let cfg = oracle::cfg_of(scenario, &dir);
        let args = &scenario["args"];
        let seeds: Vec<String> = args["seeds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_string())
            .collect();
        let mut demand = oracle::demand_of(scenario);
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
        oracle::check_result(scenario, &result, &mut diffs);
        let calls = oracle::demand_calls(&demand);
        let want_calls = &scenario["expect"]["calls"]["demand"];
        if &calls != want_calls {
            diffs.push(format!("demand calls: rust={calls} oracle={want_calls}"));
        }
        oracle::check_tables(&store, scenario, &mut diffs);
        assert!(diffs.is_empty(), "scenario {name}:\n{}", diffs.join("\n"));
    }
}

//! `exact` stage parity against the reference baseline fixture
//! (the reference suite → `tests/fixtures/exact_oracle.json`).

mod oracle;

use serde_json::json;
use spiderhead_semantics::stages;

#[test]
fn exact_matches_reference_oracle() {
    let fx = oracle::fixture("exact");
    for scenario in fx["scenarios"].as_array().unwrap() {
        let name = scenario["name"].as_str().unwrap();
        let (store, dir) = oracle::setup(scenario);
        let cfg = oracle::cfg_of(scenario, &dir);
        let args = &scenario["args"];
        let mut arsenkin = oracle::arsenkin_of(scenario);
        let result = stages::exact::run(
            &store,
            &cfg,
            args["yes"].as_bool().unwrap_or(false),
            args["limit"].as_i64().unwrap_or(0),
            &mut arsenkin,
        )
        .unwrap();
        let mut diffs = Vec::new();
        oracle::check_result(scenario, &result, &mut diffs);
        oracle::check_tables(&store, scenario, &mut diffs);
        let submits = oracle::arsenkin_submits(&arsenkin);
        let want_submits = &scenario["expect"]["calls"]["arsenkin"];
        if !want_submits.is_null() && &submits != want_submits {
            diffs.push(format!(
                "arsenkin submits: rust={submits} oracle={want_submits}"
            ));
        }
        let waits = json!(arsenkin.calls);
        let want_waits = &scenario["expect"]["calls"]["waits"];
        if !want_waits.is_null() && &waits != want_waits {
            diffs.push(format!("arsenkin waits: rust={waits} oracle={want_waits}"));
        }
        assert!(diffs.is_empty(), "scenario {name}:\n{}", diffs.join("\n"));
    }
}

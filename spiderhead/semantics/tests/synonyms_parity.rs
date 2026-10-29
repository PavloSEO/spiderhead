//! `synonyms` stage parity (the reference suite → `synonyms_oracle.json`).

mod oracle;

use spiderhead_semantics::stages;

#[test]
fn synonyms_matches_reference_oracle() {
    let fx = oracle::fixture("synonyms");
    for scenario in fx["scenarios"].as_array().unwrap() {
        let name = scenario["name"].as_str().unwrap();
        let (store, dir) = oracle::setup(scenario);
        let cfg = oracle::cfg_of(scenario, &dir);
        let mut demand = oracle::demand_of(scenario);
        let result = stages::synonyms::run(&store, &cfg, &mut demand).unwrap();
        let mut diffs = Vec::new();
        oracle::check_result(scenario, &result, &mut diffs);
        let calls = oracle::demand_calls(&demand);
        let want_calls = &scenario["expect"]["calls"]["demand"];
        if &calls != want_calls {
            diffs.push(format!("demand calls: rust={calls} oracle={want_calls}"));
        }
        oracle::check_tables(&store, scenario, &mut diffs);
        oracle::check_files(scenario, &dir, &mut diffs);
        assert!(diffs.is_empty(), "scenario {name}:\n{}", diffs.join("\n"));
    }
}

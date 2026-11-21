//! `synonyms` stage golden check (the reference suite → `synonyms_golden.json`).

mod golden;

use spiderhead_semantics::stages;

#[test]
fn golden_synonyms() {
    let fx = golden::fixture("synonyms");
    for scenario in fx["scenarios"].as_array().unwrap() {
        let name = scenario["name"].as_str().unwrap();
        let (store, dir) = golden::setup(scenario);
        let cfg = golden::cfg_of(scenario, &dir);
        let mut demand = golden::demand_of(scenario);
        let result = stages::synonyms::run(&store, &cfg, &mut demand).unwrap();
        let mut diffs = Vec::new();
        golden::check_result(scenario, &result, &mut diffs);
        let calls = golden::demand_calls(&demand);
        let want_calls = &scenario["expect"]["calls"]["demand"];
        if &calls != want_calls {
            diffs.push(format!("demand calls: rust={calls} golden={want_calls}"));
        }
        golden::check_tables(&store, scenario, &mut diffs);
        golden::check_files(scenario, &dir, &mut diffs);
        assert!(diffs.is_empty(), "scenario {name}:\n{}", diffs.join("\n"));
    }
}

//! `competitors` stage parity against the reference baseline fixture
//! (the reference suite → `tests/fixtures/competitors_oracle.json`).

mod oracle;

use spiderhead_semantics::stages;

#[test]
fn competitors_matches_reference_oracle() {
    let fx = oracle::fixture("competitors");
    for scenario in fx["scenarios"].as_array().unwrap() {
        let name = scenario["name"].as_str().unwrap();
        let (store, dir) = oracle::setup(scenario);
        let cfg = oracle::cfg_of(scenario, &dir);
        let out_dir = dir.join(scenario["args"]["out_dir"].as_str().unwrap_or("report"));
        let result = stages::competitors::run(&store, &cfg, &out_dir).unwrap();
        let mut diffs = Vec::new();
        oracle::check_result(scenario, &result, &mut diffs);
        oracle::check_tables(&store, scenario, &mut diffs);
        oracle::check_files(scenario, &dir, &mut diffs);
        assert!(diffs.is_empty(), "scenario {name}:\n{}", diffs.join("\n"));
    }
}

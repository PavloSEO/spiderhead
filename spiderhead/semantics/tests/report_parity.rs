//! `report` stage parity against the reference baseline fixture
//! (the reference suite → `tests/fixtures/report_oracle.json`).

mod oracle;

use spiderhead_semantics::stages;

#[test]
fn report_matches_reference_oracle() {
    let fx = oracle::fixture("report");
    for scenario in fx["scenarios"].as_array().unwrap() {
        let name = scenario["name"].as_str().unwrap();
        let (store, dir) = oracle::setup(scenario);
        let cfg = oracle::cfg_of(scenario, &dir);
        let result = stages::report::run(&store, &cfg, &dir).unwrap();
        let mut diffs = Vec::new();
        oracle::check_result(scenario, &result, &mut diffs);
        oracle::check_tables(&store, scenario, &mut diffs);
        oracle::check_files(scenario, &dir, &mut diffs);
        assert!(diffs.is_empty(), "scenario {name}:\n{}", diffs.join("\n"));
    }
}

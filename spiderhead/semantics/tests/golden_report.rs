//! `report` stage golden check against the reference baseline fixture
//! (the reference suite → `tests/fixtures/report_oracle.json`).

mod golden;

use spiderhead_semantics::stages;

#[test]
fn golden_report() {
    let fx = golden::fixture("report");
    for scenario in fx["scenarios"].as_array().unwrap() {
        let name = scenario["name"].as_str().unwrap();
        let (store, dir) = golden::setup(scenario);
        let cfg = golden::cfg_of(scenario, &dir);
        let result = stages::report::run(&store, &cfg, &dir).unwrap();
        let mut diffs = Vec::new();
        golden::check_result(scenario, &result, &mut diffs);
        golden::check_tables(&store, scenario, &mut diffs);
        golden::check_files(scenario, &dir, &mut diffs);
        assert!(diffs.is_empty(), "scenario {name}:\n{}", diffs.join("\n"));
    }
}

//! `competitors` stage golden check against the reference baseline fixture
//! (the reference suite → `tests/fixtures/competitors_golden.json`).

mod golden;

use spiderhead_semantics::stages;

#[test]
fn golden_competitors() {
    let fx = golden::fixture("competitors");
    for scenario in fx["scenarios"].as_array().unwrap() {
        let name = scenario["name"].as_str().unwrap();
        let (store, dir) = golden::setup(scenario);
        let cfg = golden::cfg_of(scenario, &dir);
        let out_dir = dir.join(scenario["args"]["out_dir"].as_str().unwrap_or("report"));
        let result = stages::competitors::run(&store, &cfg, &out_dir).unwrap();
        let mut diffs = Vec::new();
        golden::check_result(scenario, &result, &mut diffs);
        golden::check_tables(&store, scenario, &mut diffs);
        golden::check_files(scenario, &dir, &mut diffs);
        assert!(diffs.is_empty(), "scenario {name}:\n{}", diffs.join("\n"));
    }
}

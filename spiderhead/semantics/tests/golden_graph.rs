//! `graph` stage golden check (the reference suite → `graph_golden.json`).

mod golden;

use spiderhead_semantics::stages;

#[test]
fn golden_graph() {
    let fx = golden::fixture("graph");
    for scenario in fx["scenarios"].as_array().unwrap() {
        let name = scenario["name"].as_str().unwrap();
        let (store, dir) = golden::setup(scenario);
        let cfg = golden::cfg_of(scenario, &dir);
        let result = stages::graph::run(&store, &cfg, &dir).unwrap();
        let mut diffs = Vec::new();
        golden::check_result(scenario, &result, &mut diffs);
        golden::check_tables(&store, scenario, &mut diffs);
        golden::check_files(scenario, &dir, &mut diffs);
        assert!(diffs.is_empty(), "scenario {name}:\n{}", diffs.join("\n"));
    }
}

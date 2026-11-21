//! `cluster` stage golden check against the reference baseline fixture
//! (the reference suite → `tests/fixtures/cluster_oracle.json`).

mod golden;

use spiderhead_semantics::stages;

#[test]
fn golden_cluster() {
    let fx = golden::fixture("cluster");
    for scenario in fx["scenarios"].as_array().unwrap() {
        let name = scenario["name"].as_str().unwrap();
        let (store, dir) = golden::setup(scenario);
        let cfg = golden::cfg_of(scenario, &dir);
        let args = &scenario["args"];
        let mut serp = golden::serp_of(scenario);
        let result = stages::cluster::run(
            &store,
            &cfg,
            args["limit"].as_i64().unwrap_or(0),
            args["top"].as_i64().unwrap_or(10),
            args["method"].as_str().unwrap_or("serp"),
            &mut serp,
        );
        let mut diffs = Vec::new();
        let expected_error = scenario["expect"].get("error");
        match (result, expected_error) {
            (Ok(v), None) | (Ok(v), Some(serde_json::Value::Null)) => {
                golden::check_result(scenario, &v, &mut diffs);
            }
            (Err(_), Some(e)) if e.is_string() => {}
            (Err(e), _) => diffs.push(format!("unexpected stage error: {e}")),
            (Ok(v), Some(e)) => diffs.push(format!("expected error {e} but stage returned {v}")),
        }
        let calls = golden::serp_calls(&serp);
        let want_calls = &scenario["expect"]["calls"]["serp"];
        if !want_calls.is_null() && &calls != want_calls {
            diffs.push(format!("serp calls: rust={calls} golden={want_calls}"));
        }
        golden::check_tables(&store, scenario, &mut diffs);
        assert!(diffs.is_empty(), "scenario {name}:\n{}", diffs.join("\n"));
    }
}

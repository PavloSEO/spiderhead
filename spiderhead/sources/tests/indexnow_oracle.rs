//! Oracle-diff test for the IndexNow client against `indexnow_oracle.json`.

mod common;

use common::{assert_scenario, load_fixture, scenario_host, StubTransport};
use serde_json::Value;
use spiderhead_sources::indexnow;

#[tokio::test(flavor = "current_thread")]
async fn indexnow_oracle_parity() {
    for scenario in load_fixture("indexnow_oracle.json") {
        let responses = scenario
            .get("responses")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let transport = StubTransport::new(&responses);
        let (host, root) = scenario_host(&scenario);
        let args = scenario.get("args").cloned().unwrap_or_default();
        let urls: Vec<String> = args
            .get("urls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
        let actual = indexnow::submit(
            &transport,
            &host,
            &root,
            &urls,
            args.get("host").and_then(Value::as_str).unwrap_or_default(),
            args.get("key").and_then(Value::as_str),
            args.get("key_location").and_then(Value::as_str),
        )
        .await
        .map_err(|e| (e.python_class().to_owned(), e.to_string()));
        assert_scenario(&scenario, &transport, &[], actual);
    }
}

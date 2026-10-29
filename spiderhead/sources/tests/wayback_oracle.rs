//! Oracle-diff test for the Wayback CDX client against `wayback_oracle.json`.

mod common;

use common::{assert_scenario, load_fixture, StubTransport};
use serde_json::Value;
use spiderhead_sources::wayback;

#[tokio::test(flavor = "current_thread")]
async fn wayback_oracle_parity() {
    for scenario in load_fixture("wayback_oracle.json") {
        let responses = scenario
            .get("responses")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let transport = StubTransport::new(&responses);
        let args = scenario.get("args").cloned().unwrap_or_default();
        let url = args.get("url").and_then(Value::as_str).unwrap_or_default();
        let actual = wayback::history(
            &transport,
            url,
            args.get("limit"),
            args.get("from_date").and_then(Value::as_str),
            args.get("to_date").and_then(Value::as_str),
        )
        .await
        .map_err(|e| (e.python_class().to_owned(), e.to_string()));
        assert_scenario(&scenario, &transport, &[], actual);
    }
}

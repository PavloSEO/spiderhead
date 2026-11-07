//! Oracle-diff test for the Clarity Data Export client against `clarity_oracle.json`.

mod common;

use common::{assert_scenario, load_fixture, StubTransport};
use serde_json::Value;
use spiderhead_sources::clarity;

#[tokio::test(flavor = "current_thread")]
async fn clarity_oracle_parity() {
    for scenario in load_fixture("clarity_oracle.json") {
        let args = scenario.get("args").cloned().unwrap_or_default();
        let transport = StubTransport::new(
            scenario
                .get("responses")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .as_slice(),
        );
        let actual = clarity::live_insights(
            &transport,
            args.get("api_key").and_then(Value::as_str).unwrap_or(""),
            args.get("project_id").and_then(Value::as_str).unwrap_or(""),
            args.get("start_date").and_then(Value::as_str).unwrap_or(""),
            args.get("end_date").and_then(Value::as_str).unwrap_or(""),
        )
        .await
        .map_err(|e| (e.error_class().to_owned(), e.to_string()));
        assert_scenario(&scenario, &transport, &[], actual);
    }
}

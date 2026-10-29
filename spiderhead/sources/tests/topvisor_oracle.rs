//! Oracle-diff test for the Topvisor client: every row of `topvisor_oracle.json` was
//! produced by the reference suite running the reference implementation against the stub
//! server. The Rust client must send the same requests and produce the same page or error.

mod common;

use common::{assert_scenario, load_fixture, scenario_host, StubTransport};
use spiderhead_sources::topvisor;

// test_fetch_defaults_to_projects_with_central_credentials,
// test_fetch_*_metadata tests, test_fetch_follows_provider_next_offset_signal,
// test_fetch_rejects_errors_envelope_without_leaking_credentials,
// test_http_errors_do_not_echo_credentials, plus the validation-before-auth cases.
#[tokio::test(flavor = "current_thread")]
async fn topvisor_oracle_parity() {
    for scenario in load_fixture("topvisor_oracle.json") {
        let (host, root) = scenario_host(&scenario);
        let responses = scenario
            .get("responses")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        let transport = StubTransport::new(&responses);
        let args = scenario.get("args").cloned().unwrap_or_default();
        let operation = args
            .get("operation")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("projects");
        let params = args.get("params").and_then(|v| v.as_object());
        let actual = topvisor::fetch(&host, &root, &transport, operation, params)
            .await
            .map_err(|e| (e.python_class().to_owned(), e.to_string()));
        assert_scenario(&scenario, &transport, &[], actual);
    }
}

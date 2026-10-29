//! Oracle-diff test for the crt.sh client against `crtsh_oracle.json`.

mod common;

use common::{assert_scenario, load_fixture, StubTransport};
use serde_json::Value;
use spiderhead_sources::crtsh;

#[tokio::test(flavor = "current_thread")]
async fn crtsh_oracle_parity() {
    for scenario in load_fixture("crtsh_oracle.json") {
        let responses = scenario
            .get("responses")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let transport = StubTransport::new(&responses);
        let domain = scenario
            .get("args")
            .and_then(|a| a.get("domain"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let actual = crtsh::subdomains(&transport, domain)
            .await
            .map_err(|e| (e.python_class().to_owned(), e.to_string()));
        assert_scenario(&scenario, &transport, &[], actual);
    }
}

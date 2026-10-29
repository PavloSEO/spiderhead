//! Oracle-diff test for the Bing Webmaster client against `bing_webmaster_oracle.json`,
//! recorded by the reference suite running the reference implementation.

mod common;

use common::{assert_scenario, load_fixture, scenario_host, StubTransport};
use serde_json::Value;
use spiderhead_sources::bing_webmaster;

#[tokio::test(flavor = "current_thread")]
async fn bing_webmaster_oracle_parity() {
    for scenario in load_fixture("bing_webmaster_oracle.json") {
        let (host, root) = scenario_host(&scenario);
        let responses = scenario
            .get("responses")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let transport = StubTransport::new(&responses);
        let args = scenario.get("args").cloned().unwrap_or_default();
        let operation = args.get("operation").and_then(Value::as_str).unwrap_or("");
        let site_url = args.get("site_url").and_then(Value::as_str).unwrap_or("");
        let link = args.get("link").and_then(Value::as_str);
        let api_key = args.get("api_key").and_then(Value::as_str);
        let actual = bing_webmaster::collect(
            &host,
            &root,
            &transport,
            operation,
            bing_webmaster::Call {
                site_url,
                page: args.get("page"),
                link,
                api_key,
            },
        )
        .await
        .map_err(|e| (e.python_class().to_owned(), e.to_string()));
        assert_scenario(&scenario, &transport, &[], actual);
    }
}

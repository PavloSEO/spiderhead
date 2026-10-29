//! Oracle-diff test for the Miratext client against `miratext_oracle.json`.

mod common;

use common::{assert_scenario, load_fixture, scenario_host, StubTransport};
use serde_json::{json, Value};
use spiderhead_sources::miratext::{self, Call};

/// `spend.record` stamps wall-clock time; the ledger write is verified by the rest of the
/// entry, so the stamp is scrubbed on both sides before comparing.
fn scrub_stamp(v: &mut Value) {
    if let Some(at) = v.pointer_mut("/spend/at") {
        *at = json!("__stamp__");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn miratext_oracle_parity() {
    for scenario in load_fixture("miratext_oracle.json") {
        let mut scenario = scenario.clone();
        if let Some(expect) = scenario.pointer_mut("/expect/result") {
            let mut e = expect.take();
            scrub_stamp(&mut e);
            *expect = e;
        }
        let responses = scenario
            .get("responses")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let transport = StubTransport::new(&responses);
        let (host, root) = scenario_host(&scenario);
        let args = scenario.get("args").cloned().unwrap_or_default();
        let call = Call {
            urls: args
                .get("urls")
                .and_then(Value::as_array)
                .map(Vec::as_slice),
            texts: args
                .get("texts")
                .and_then(Value::as_array)
                .map(Vec::as_slice),
            my: args.get("my").and_then(Value::as_str),
            hash: args.get("hash").filter(|v| !v.is_null()),
            check_type: args
                .get("check_type")
                .and_then(Value::as_str)
                .unwrap_or("url"),
            keywords: args.get("keywords").and_then(Value::as_str),
            paid: args.get("paid").and_then(Value::as_bool).unwrap_or(false),
            confirm_paid: args
                .get("confirm_paid")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            timeout: args.get("timeout"),
            top: args.get("top"),
            api_key: args.get("api_key").and_then(Value::as_str),
        };
        let mut actual = miratext::analyze(&transport, &host, &root, &call)
            .await
            .map_err(|e| (e.python_class().to_owned(), e.to_string()));
        if let Ok(v) = &mut actual {
            scrub_stamp(v);
        }
        assert_scenario(&scenario, &transport, &[], actual);
    }
}

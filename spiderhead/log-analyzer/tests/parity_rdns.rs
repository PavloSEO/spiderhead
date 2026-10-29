//! Forward-confirmed reverse DNS outcomes against a scripted resolver.

mod common;

use std::sync::Arc;

use serde_json::Value;
use spiderhead_log_analyzer::dns::DnsFailure;

use common::{assert_same, load, verifier, FakeResolver};

#[test]
fn outcomes_match_for_every_suffix_table() {
    let doc = load("rdns_cases.json");
    let script = &doc["script"];
    for case in doc["cases"].as_array().unwrap() {
        let mut r = FakeResolver::default();
        for (ip, host) in script["ptr"].as_object().unwrap() {
            r.ptr.insert(ip.clone(), host.as_str().unwrap().to_string());
        }
        for (host, addrs) in script["forward"].as_object().unwrap() {
            let list = addrs
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a.as_str().unwrap().to_string())
                .collect();
            r.forward.insert(host.clone(), list);
        }
        for (ip, pair) in script["ptr_errors"].as_object().unwrap() {
            let failure = match pair[0].as_i64() {
                Some(1) => DnsFailure::NotFound,
                Some(4) => DnsFailure::NoData,
                Some(2) => DnsFailure::TryAgain,
                _ => DnsFailure::Other(pair[1].as_str().unwrap().to_string()),
            };
            r.failures.insert(ip.clone(), failure);
        }
        r.break_forward = case["break_forward"].as_bool().unwrap() && case["ip"] == "66.249.66.1";
        let v = verifier(Arc::new(r));
        let suffixes: Vec<&str> = case["suffixes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let actual = Value::Object(v.verify_bot_rdns(case["ip"].as_str().unwrap(), &suffixes));
        assert_same(&case["ip"].to_string(), &case["expected"], &actual, &[]);
    }
}

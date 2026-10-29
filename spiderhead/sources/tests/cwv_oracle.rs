//! Oracle-diff test for the pure CWV assessment against `cwv_oracle.json`
//! (the reference suite on the reference implementation).

mod common;

use common::load_fixture;
use serde_json::Value;
use spiderhead_sources::cwv;

// no requests; only the projected result/error is compared.
#[test]
fn cwv_oracle_parity() {
    for scenario in load_fixture("cwv_oracle.json") {
        let name = scenario.get("name").and_then(Value::as_str).unwrap_or("?");
        let record = scenario
            .get("args")
            .and_then(|a| a.get("record"))
            .cloned()
            .unwrap_or(Value::Null);
        let actual = cwv::assess(&record).map_err(|e| e.to_string());
        let expect = scenario.get("expect").expect("expect");
        match (expect.get("result"), expect.get("error"), actual) {
            (Some(want), _, Ok(got)) => {
                assert_eq!(
                    &got, want,
                    "{name}: result differs\nwant: {want}\n got: {got}"
                )
            }
            (_, Some(want), Err(message)) => {
                let class = want.get("class").and_then(Value::as_str).unwrap_or("");
                assert_eq!(class, "ValueError", "{name}: error class differs");
                let want_msg = want.get("message").and_then(Value::as_str).unwrap_or("");
                assert_eq!(message, want_msg, "{name}: error message differs");
            }
            (Some(want), _, Err(e)) => panic!("{name}: expected {want}, got error {e}"),
            (_, Some(want), Ok(got)) => panic!("{name}: expected error {want}, got {got}"),
            (None, None, _) => panic!("{name}: fixture row malformed"),
        }
    }
}

//! Stored trust-signal shape tests ported from the reference suite.
//!
//! Covers the refusal of a malformed `trust_signals` record, using the same six
//! inputs as the reference parametrisation, through the store's validator. The
//! import-pipeline cases (legacy run fixture, round-trip through export) are
//! listed as deferred in the report.

use serde_json::{json, Value};
use spiderhead_store::scan_artifact::trust_signals;

#[test]
fn a_trust_signals_record_of_the_wrong_shape_is_refused() {
    let cases: Vec<(&str, Value)> = vec![
        ("missing article", json!({"author": [], "dates": []})),
        (
            "extra key",
            json!({"author": [], "dates": [], "article": [], "extra": 1}),
        ),
        (
            "signal without value",
            json!({
                "author": [{"signal": "meta_author", "confidence": "high"}],
                "dates": [],
                "article": []
            }),
        ),
        (
            "non-string value",
            json!({
                "author": [{"signal": "meta_author", "confidence": "high", "value": 3}],
                "dates": [],
                "article": []
            }),
        ),
        (
            "article item not a string",
            json!({"author": [], "dates": [], "article": [1]}),
        ),
        ("not an object at all", json!("not an object at all")),
    ];
    for (label, value) in cases {
        let err = trust_signals(&value).expect_err(label);
        assert!(
            err.to_string().contains("trust_signals"),
            "{label}: message was {err}"
        );
    }
}

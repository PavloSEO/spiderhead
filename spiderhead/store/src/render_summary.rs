//! Append-only render phase summaries, without document bodies or local paths.

use crate::discovery_ledger::py_dumps;
use crate::error::StoreError;
use crate::native_scan::NativeScan;
use crate::scan_artifact::ScanError;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

pub const KIND: &str = "render_phase_summary";

/// Sorted for a stable output order.
pub const FIELDS: &[&str] = &[
    "mode",
    "patterns_escalated",
    "patterns_partially_rendered",
    "patterns_sampled",
    "patterns_unprobed",
    "patterns_unprobed_reasons",
    "probe_requests",
    "render_budget_exhausted",
    "render_counts",
    "render_requests",
    "time_budget_exhausted",
];

/// The render-phase outcome, reduced to the fields this module stores (`FIELDS`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RenderPhaseOutcome {
    pub mode: String,
    pub patterns_sampled: i64,
    pub patterns_escalated: Vec<String>,
    pub probe_requests: i64,
    pub render_requests: i64,
    pub render_budget_exhausted: bool,
    pub time_budget_exhausted: bool,
    pub render_counts: BTreeMap<String, i64>,
    pub patterns_partially_rendered: Vec<String>,
    pub patterns_unprobed: Vec<String>,
    pub patterns_unprobed_reasons: BTreeMap<String, String>,
}

fn invalid(message: &str) -> StoreError {
    StoreError::ScanError(ScanError {
        message: message.to_string(),
    })
}

/// True for a non-negative JSON integer.
fn count(value: &Value) -> bool {
    value.is_u64() || value.as_i64().is_some_and(|n| n >= 0)
}

fn string_list(value: &Value) -> bool {
    value
        .as_array()
        .is_some_and(|items| items.iter().all(Value::is_string))
}

pub fn validate(payload: &Value) -> Result<(), StoreError> {
    let Some(map) = payload.as_object() else {
        return Err(invalid("invalid render phase summary"));
    };
    if map.len() != FIELDS.len() || FIELDS.iter().any(|key| !map.contains_key(*key)) {
        return Err(invalid("invalid render phase summary"));
    }
    if !matches!(map["mode"].as_str(), Some("raw" | "js" | "legacy_fragment")) {
        return Err(invalid("invalid render phase mode"));
    }
    for key in ["patterns_sampled", "probe_requests", "render_requests"] {
        if !count(&map[key]) {
            return Err(invalid("invalid render phase count"));
        }
    }
    for key in ["render_budget_exhausted", "time_budget_exhausted"] {
        if !map[key].is_boolean() {
            return Err(invalid("invalid render phase budget state"));
        }
    }
    for key in [
        "patterns_escalated",
        "patterns_partially_rendered",
        "patterns_unprobed",
    ] {
        if !string_list(&map[key]) {
            return Err(invalid("invalid render phase patterns"));
        }
    }
    if !map["render_counts"]
        .as_object()
        .is_some_and(|counts| counts.values().all(count))
    {
        return Err(invalid("invalid render phase pattern count"));
    }
    if !map["patterns_unprobed_reasons"]
        .as_object()
        .is_some_and(|reasons| reasons.values().all(Value::is_string))
    {
        return Err(invalid("invalid render phase reason"));
    }
    Ok(())
}

/// Append one phase summary to the scan's context items (key `phase:<ordinal>`).
pub fn record(scan: &NativeScan, outcome: &RenderPhaseOutcome) -> Result<(), StoreError> {
    let Value::Object(all) = serde_json::to_value(outcome)? else {
        return Err(invalid("invalid render phase summary"));
    };
    let payload = Value::Object(
        all.into_iter()
            .filter(|(key, _)| FIELDS.contains(&key.as_str()))
            .collect::<Map<_, _>>(),
    );
    validate(&payload)?;
    let ordinal: i64 = scan.con().query_row(
        "SELECT COUNT(*) FROM context_items WHERE kind=?",
        [KIND],
        |row| row.get(0),
    )?;
    scan.write_context(&[json!({
        "kind": KIND,
        "item_key": format!("phase:{ordinal}"),
        "payload_version": "scan_context.v1",
        "payload_json": py_dumps(&payload, false, true),
        "completeness": "complete",
        "reason": "",
    })])
}

/// Fold every stored phase summary into `outcome` (counts added, flags OR-ed, patterns unioned).
pub fn aggregate(con: &Connection, outcome: &mut RenderPhaseOutcome) -> Result<(), StoreError> {
    let mut statement =
        con.prepare("SELECT payload_json FROM context_items WHERE kind=? ORDER BY item_key")?;
    let texts = statement
        .query_map([KIND], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for text in texts {
        let payload: Value = serde_json::from_str(&text)?;
        validate(&payload)?;
        let stored: RenderPhaseOutcome = serde_json::from_value(payload)?;
        outcome.patterns_sampled += stored.patterns_sampled;
        outcome.probe_requests += stored.probe_requests;
        outcome.render_requests += stored.render_requests;
        outcome.render_budget_exhausted |= stored.render_budget_exhausted;
        outcome.time_budget_exhausted |= stored.time_budget_exhausted;
        for (mine, theirs) in [
            (&mut outcome.patterns_escalated, &stored.patterns_escalated),
            (
                &mut outcome.patterns_partially_rendered,
                &stored.patterns_partially_rendered,
            ),
            (&mut outcome.patterns_unprobed, &stored.patterns_unprobed),
        ] {
            let merged: BTreeSet<String> = mine.iter().chain(theirs).cloned().collect();
            *mine = merged.into_iter().collect();
        }
        outcome
            .patterns_unprobed_reasons
            .extend(stored.patterns_unprobed_reasons);
        for (pattern, n) in stored.render_counts {
            *outcome.render_counts.entry(pattern).or_insert(0) += n;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload() -> Value {
        json!({
            "mode": "js", "patterns_sampled": 2, "patterns_escalated": ["/a/"],
            "probe_requests": 3, "render_requests": 1, "render_budget_exhausted": false,
            "time_budget_exhausted": true, "render_counts": {"/a/": 1},
            "patterns_partially_rendered": [], "patterns_unprobed": ["/b/"],
            "patterns_unprobed_reasons": {"/b/": "robots"},
        })
    }

    #[test]
    fn validate_accepts_a_complete_payload() {
        validate(&payload()).unwrap();
    }

    #[test]
    fn validate_rejects_each_malformed_field() {
        for (key, bad) in [
            ("mode", json!("other")),
            ("patterns_sampled", json!(-1)),
            ("probe_requests", json!(true)),
            ("render_budget_exhausted", json!(1)),
            ("patterns_escalated", json!([1])),
            ("render_counts", json!({"/a/": -1})),
            ("patterns_unprobed_reasons", json!({"/b/": 3})),
        ] {
            let mut value = payload();
            value[key] = bad;
            assert!(validate(&value).is_err(), "{key}");
        }
        let mut extra = payload();
        extra["extra"] = json!(1);
        assert!(validate(&extra).is_err());
        assert!(validate(&json!([])).is_err());
    }

    fn con_with(payloads: &[Value]) -> Connection {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch("CREATE TABLE context_items(kind TEXT,item_key TEXT,payload_json TEXT)")
            .unwrap();
        for (i, p) in payloads.iter().enumerate() {
            con.execute(
                "INSERT INTO context_items VALUES(?,?,?)",
                rusqlite::params![KIND, format!("phase:{i}"), p.to_string()],
            )
            .unwrap();
        }
        con
    }

    #[test]
    fn aggregate_folds_phases() {
        let second = json!({
            "mode": "raw", "patterns_sampled": 1, "patterns_escalated": ["/c/", "/a/"],
            "probe_requests": 0, "render_requests": 2, "render_budget_exhausted": true,
            "time_budget_exhausted": false, "render_counts": {"/a/": 2, "/c/": 1},
            "patterns_partially_rendered": [], "patterns_unprobed": [],
            "patterns_unprobed_reasons": {"/d/": "x"},
        });
        let con = con_with(&[payload(), second]);
        let mut outcome = RenderPhaseOutcome::default();
        aggregate(&con, &mut outcome).unwrap();
        assert_eq!(outcome.patterns_sampled, 3);
        assert_eq!(outcome.probe_requests, 3);
        assert_eq!(outcome.render_requests, 3);
        assert!(outcome.render_budget_exhausted && outcome.time_budget_exhausted);
        assert_eq!(outcome.patterns_escalated, vec!["/a/", "/c/"]);
        assert_eq!(outcome.patterns_unprobed, vec!["/b/"]);
        assert_eq!(outcome.render_counts["/a/"], 3);
        assert_eq!(outcome.patterns_unprobed_reasons.len(), 2);
    }

    #[test]
    fn aggregate_rejects_corrupt_rows() {
        let con = con_with(&[json!({"mode": "js"})]);
        assert!(aggregate(&con, &mut RenderPhaseOutcome::default()).is_err());
    }
}

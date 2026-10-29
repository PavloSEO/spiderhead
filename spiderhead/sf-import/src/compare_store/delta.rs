//! Finding-level delta across two finding sets: improved, regressed, added,
//! resolved — the same-key comparison the `compare-findings` surface reports.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::{json, Map, Value};
use spiderhead_store::evidence_contract::Obj;

use crate::compare::base::{key_of, repr_key, CompareError};
use crate::pyvalue::str_or_dumps;

/// The four finding-delta buckets, in report order.
const DIRECTIONS: [&str; 4] = ["improved", "regressed", "added", "resolved"];

/// Severity ranks a direction is derived from; an unranked severity means 0.
fn severity_rank(severity: Option<&str>) -> i64 {
    match severity {
        Some("critical") => 2,
        Some("warning") => 1,
        _ => 0,
    }
}

/// The severity a finding declares, or none when it is not a string.
fn severity_of(issue: Option<&Obj>) -> Option<String> {
    issue
        .and_then(|issue| issue.get("severity"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Key one finding by check name and target URL (the empty URL for audit-wide).
fn index_findings(findings: &[Value]) -> Result<HashMap<(String, String), Obj>, CompareError> {
    let mut index = HashMap::new();
    for issue in findings {
        let Some(issue) = issue.as_object() else {
            return Err(CompareError(
                "findings delta requires a finding with a nonempty check".into(),
            ));
        };
        let check = issue.get("check").map(str_or_dumps).unwrap_or_default();
        if check.is_empty() {
            return Err(CompareError(
                "findings delta requires a finding with a nonempty check".into(),
            ));
        }
        let key = key_of(issue);
        if index.contains_key(&key) {
            return Err(CompareError(format!(
                "findings delta has a duplicate finding key {}",
                repr_key(&key)
            )));
        }
        index.insert(key, issue.clone());
    }
    Ok(index)
}

/// Which direction this key moved between the runs, or none when unchanged.
fn direction(
    before: Option<&Obj>,
    after: Option<&Obj>,
    severity_before: Option<&str>,
    severity_after: Option<&str>,
) -> Option<&'static str> {
    let rank_before = severity_rank(severity_before);
    let rank_after = severity_rank(severity_after);
    if before.is_none() {
        return Some(if rank_after >= 2 {
            "regressed"
        } else {
            "added"
        });
    }
    if after.is_none() {
        return Some(if rank_before >= 2 {
            "improved"
        } else {
            "resolved"
        });
    }
    if rank_after > rank_before {
        Some("regressed")
    } else if rank_after < rank_before {
        Some("improved")
    } else {
        None
    }
}

/// Diff two finding sets by shared identity: severity movements become
/// improved/regressed, presence changes become added/resolved. Duplicate
/// findings on either side are refused — a delta over an ambiguous key set
/// would silently undercount.
pub fn findings_delta(before: &[Value], after: &[Value]) -> Result<Obj, CompareError> {
    let before_index = index_findings(before)?;
    let after_index = index_findings(after)?;
    let keys: BTreeSet<(String, String)> = before_index
        .keys()
        .chain(after_index.keys())
        .cloned()
        .collect();

    let mut by_check: BTreeMap<String, BTreeMap<&'static str, i64>> = BTreeMap::new();
    let mut changed: Vec<Value> = Vec::new();
    let mut totals: BTreeMap<&'static str, i64> = DIRECTIONS
        .iter()
        .chain(["unchanged"].iter())
        .map(|d| (*d, 0i64))
        .collect();

    for key in &keys {
        let before_issue = before_index.get(key);
        let after_issue = after_index.get(key);
        let severity_before = severity_of(before_issue);
        let severity_after = severity_of(after_issue);
        let move_direction = direction(
            before_issue,
            after_issue,
            severity_before.as_deref(),
            severity_after.as_deref(),
        );
        // Every key counts per check; unchanged findings are counted, never listed.
        let row_name = move_direction.unwrap_or("unchanged");
        *by_check
            .entry(key.0.clone())
            .or_default()
            .entry(row_name)
            .or_insert(0) += 1;
        *totals.get_mut(row_name).unwrap_or(&mut 0) += 1;
        let Some(move_direction) = move_direction else {
            continue;
        };
        changed.push(json!({
            "check": key.0,
            "target_url": if key.1.is_empty() { Value::Null } else { Value::String(key.1.clone()) },
            "direction": move_direction,
            "severity_before": severity_before,
            "severity_after": severity_after,
        }));
    }

    let mut by_check_json = Map::new();
    for (check, counts) in by_check {
        let mut row = Map::new();
        for name in DIRECTIONS.iter().chain(["unchanged"].iter()) {
            row.insert(
                (*name).to_owned(),
                json!(counts.get(name).copied().unwrap_or(0)),
            );
        }
        by_check_json.insert(check, Value::Object(row));
    }
    let mut totals_json = Map::new();
    for name in DIRECTIONS
        .iter()
        .chain(["unchanged", "before", "after"].iter())
    {
        let value = match *name {
            "before" => before.len() as i64,
            "after" => after.len() as i64,
            other => totals.get(other).copied().unwrap_or(0),
        };
        totals_json.insert((*name).to_owned(), json!(value));
    }

    let mut result = Obj::new();
    result.insert("by_check".into(), Value::Object(by_check_json));
    result.insert("changed".into(), Value::Array(changed));
    result.insert("totals".into(), Value::Object(totals_json));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn directions_follow_severity_ranks() {
        let before = vec![
            json!({"check": "a", "target_url": "https://x/1", "severity": "warning"}),
            json!({"check": "b", "target_url": "https://x/2", "severity": "critical"}),
        ];
        let after = vec![
            json!({"check": "a", "target_url": "https://x/1", "severity": "critical"}),
            json!({"check": "b", "target_url": "https://x/2", "severity": "warning"}),
        ];
        let result = findings_delta(&before, &after).expect("delta");
        assert_eq!(result["totals"]["regressed"], json!(1));
        assert_eq!(result["totals"]["improved"], json!(1));
        assert_eq!(result["changed"][0]["direction"], json!("regressed"));
        assert_eq!(result["changed"][1]["direction"], json!("improved"));
    }

    #[test]
    fn added_resolved_and_unchanged_are_classified() {
        // A new warning is added; a new critical would be regressed instead.
        let before = vec![
            json!({"check": "gone", "target_url": "https://x/3", "severity": "warning"}),
            json!({"check": "same", "target_url": "https://x/4", "severity": "warning"}),
        ];
        let after = vec![
            json!({"check": "new", "target_url": "https://x/5", "severity": "warning"}),
            json!({"check": "same", "target_url": "https://x/4", "severity": "warning"}),
        ];
        let result = findings_delta(&before, &after).expect("delta");
        assert_eq!(result["totals"]["added"], json!(1));
        assert_eq!(result["totals"]["resolved"], json!(1));
        assert_eq!(result["totals"]["regressed"], json!(0));
        assert_eq!(result["totals"]["unchanged"], json!(1));
        assert_eq!(result["totals"]["before"], json!(2));
        assert_eq!(result["totals"]["after"], json!(2));
        // Unchanged findings are counted per check, never listed in `changed`.
        let by_check = result["by_check"].as_object().expect("by_check");
        assert_eq!(
            by_check.get("same").expect("same row"),
            &json!({"improved": 0, "regressed": 0, "added": 0, "resolved": 0, "unchanged": 1})
        );
        assert!(by_check.contains_key("gone"));
        assert!(by_check.contains_key("new"));
        let listed: Vec<&str> = result["changed"]
            .as_array()
            .expect("changed")
            .iter()
            .map(|row| row["check"].as_str().expect("check"))
            .collect();
        assert_eq!(listed, vec!["gone", "new"]);
        // A critical appearing where nothing was is a regression, not an addition.
        let appeared = findings_delta(
            &[],
            &[json!({"check": "c", "target_url": "https://x/1", "severity": "critical"})],
        )
        .expect("delta");
        assert_eq!(appeared["totals"]["regressed"], json!(1));
        assert_eq!(appeared["totals"]["added"], json!(0));
    }

    #[test]
    fn empty_check_is_refused() {
        let result = findings_delta(&[json!({"check": "", "target_url": "https://x/1"})], &[]);
        assert_eq!(
            result.err().map(|e| e.0),
            Some("findings delta requires a finding with a nonempty check".into())
        );
    }

    #[test]
    fn duplicate_keys_are_refused() {
        let result = findings_delta(
            &[
                json!({"check": "a", "target_url": "https://x/1"}),
                json!({"check": "a", "target_url": "https://x/1"}),
            ],
            &[],
        );
        assert_eq!(
            result.err().map(|e| e.0),
            Some("findings delta has a duplicate finding key ('a', 'https://x/1')".into())
        );
    }
}

//! Priority-policy receipt validation for the coverage history.

use std::collections::BTreeSet;

use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

use super::schema::{duplicated, hash, identifier, text};
use crate::error::ProjectsResult;
use crate::pytime;
use spiderhead_core::formats::matches;

fn fact_name_re() -> &'static Regex {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[a-z][a-z0-9_]{0,63}\z").unwrap());
    &RE
}

/// `_priority`: P0/P1/P2.
pub fn priority(value: &Value, label: &str) -> ProjectsResult<()> {
    match value.as_str() {
        Some("P0") | Some("P1") | Some("P2") => Ok(()),
        _ => Err(crate::error::value(format!("invalid {label} priority"))),
    }
}

/// `_priority_origin`: default/operator/policy.
pub fn priority_origin(value: &Value, label: &str) -> ProjectsResult<()> {
    match value.as_str() {
        Some("default") | Some("operator") | Some("policy") => Ok(()),
        _ => Err(crate::error::value(format!(
            "invalid {label} priority origin"
        ))),
    }
}

/// `_application_time`: bounded RFC3339 UTC text.
pub fn application_time(value: &Value) -> ProjectsResult<()> {
    text(value, "priority policy application time", 128)?;
    let parsed = pytime::fromisoformat(&value.as_str().unwrap_or_default().replace('Z', "+00:00"))
        .map_err(|_| crate::error::value("priority policy application time must be RFC3339 UTC"))?;
    if !parsed.is_utc() {
        return Err(crate::error::value(
            "priority policy application time must be RFC3339 UTC",
        ));
    }
    Ok(())
}

/// `_historical_policy`: validate a stored `project-priorities.v1` policy.
pub fn historical_policy(value: &Value) -> ProjectsResult<()> {
    let invalid = || crate::error::value("invalid priority policy receipt");
    let object = value.as_object().filter(|o| {
        o.keys().map(String::as_str).collect::<BTreeSet<_>>()
            == ["format", "rules"].into_iter().collect()
    });
    let Some(object) = object else {
        return Err(invalid());
    };
    if !matches(
        object["format"].as_str(),
        "spiderhead.project-priorities.v1",
    ) || !object["rules"].is_array()
    {
        return Err(invalid());
    }
    let rules = object["rules"].as_array().unwrap();
    if rules.len() > 100 {
        return Err(invalid());
    }
    let mut ids = BTreeSet::new();
    for rule in rules {
        let object = rule.as_object().filter(|o| {
            o.keys().map(String::as_str).collect::<BTreeSet<_>>()
                == ["id", "facts", "priority", "items"].into_iter().collect()
        });
        let Some(object) = object else {
            return Err(invalid());
        };
        let id = object["id"].as_str().unwrap_or_default();
        if id.is_empty() || id.len() > 128 || !ids.insert(id.to_owned()) {
            return Err(invalid());
        }
        priority(&object["priority"], "priority rule")?;
        let facts = object["facts"].as_object().filter(|f| f.len() <= 16);
        let Some(facts) = facts else {
            return Err(invalid());
        };
        for (name, values) in facts {
            let Some(values) = values.as_array() else {
                return Err(invalid());
            };
            if !fact_name_re().is_match(name)
                || values.is_empty()
                || values.len() > 100
                || values
                    .iter()
                    .any(|v| v.as_str().is_none_or(|s| s.is_empty() || s.len() > 128))
            {
                return Err(invalid());
            }
            if duplicated(values) {
                return Err(invalid());
            }
        }
        let items = object["items"]
            .as_array()
            .filter(|i| !i.is_empty() && i.len() <= 100);
        let Some(items) = items else {
            return Err(invalid());
        };
        if items.iter().any(|i| !i.is_string()) || duplicated(items) {
            return Err(invalid());
        }
        for item_id in items {
            identifier(item_id)?;
        }
    }
    Ok(())
}

/// `_receipt_fingerprint`: the decision fields the fingerprint binds.
pub fn receipt_fingerprint(decisions: &[Value]) -> Vec<Value> {
    decisions
        .iter()
        .map(|decision| {
            serde_json::json!({
                "id": decision["id"],
                "after": decision["after"],
                "reason": decision["reason"],
                "consulted_facts": decision["consulted_facts"],
            })
        })
        .collect()
}

/// `_priority_receipt`: the full receipt check binding one application to items and facts.
pub fn priority_receipt(value: &Value, item_ids: &BTreeSet<String>) -> ProjectsResult<()> {
    let invalid = || crate::error::value("invalid priority policy receipt");
    let object = value.as_object().filter(|o| {
        let keys: BTreeSet<&str> = o.keys().map(String::as_str).collect();
        let base: BTreeSet<&str> = [
            "policy",
            "policy_hash",
            "facts",
            "decisions",
            "decision_fingerprint",
        ]
        .into_iter()
        .collect();
        let mut with_items = base.clone();
        with_items.insert("item_ids");
        keys == base || keys == with_items
    });
    let Some(object) = object else {
        return Err(invalid());
    };
    historical_policy(&object["policy"])?;
    if object["policy_hash"].as_str() != Some(hash(&object["policy"])?.as_str()) {
        return Err(invalid());
    }
    if crate::workspace::facts(Some(&object["facts"]))?
        != object["facts"].as_array().cloned().unwrap_or_default()
    {
        return Err(invalid());
    }
    let decisions = object["decisions"]
        .as_array()
        .filter(|d| d.len() <= item_ids.len())
        .ok_or_else(invalid)?;
    let mut seen = BTreeSet::new();
    for decision in decisions {
        let fields = decision.as_object().filter(|d| {
            d.keys().map(String::as_str).collect::<BTreeSet<_>>()
                == ["id", "before", "after", "reason", "consulted_facts"]
                    .into_iter()
                    .collect()
        });
        let Some(fields) = fields else {
            return Err(invalid());
        };
        let item_id = fields["id"].as_str().unwrap_or_default();
        if item_id.is_empty() || !item_ids.contains(item_id) || !seen.insert(item_id.to_owned()) {
            return Err(invalid());
        }
        for state in ["before", "after"] {
            let def = fields[state].as_object().filter(|d| {
                d.keys().map(String::as_str).collect::<BTreeSet<_>>()
                    == ["priority", "priority_origin"].into_iter().collect()
            });
            let Some(def) = def else {
                return Err(invalid());
            };
            priority(&def["priority"], "priority decision")?;
            priority_origin(&def["priority_origin"], "priority decision")?;
        }
        text(&fields["reason"], "priority decision reason", 512)?;
        let consulted = fields["consulted_facts"]
            .as_array()
            .filter(|c| c.len() <= 16);
        let Some(consulted) = consulted else {
            return Err(invalid());
        };
        let names: Vec<String> = consulted
            .iter()
            .map(|c| c.as_str().unwrap_or_default().to_owned())
            .collect();
        let dedup_sorted: Vec<String> = names
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if names != dedup_sorted || names.iter().any(|n| !fact_name_re().is_match(n)) {
            return Err(invalid());
        }
    }
    if let Some(scope) = object.get("item_ids") {
        let list = scope.as_array().filter(|l| l.iter().all(Value::is_string));
        let Some(list) = list else {
            return Err(crate::error::value(
                "priority policy receipt disagrees with its historical item scope",
            ));
        };
        let mut expected: Vec<&str> = seen.iter().map(String::as_str).collect();
        expected.sort_unstable();
        let actual: Vec<&str> = list
            .iter()
            .map(|v| v.as_str().unwrap_or_default())
            .collect();
        if actual != expected {
            return Err(crate::error::value(
                "priority policy receipt disagrees with its historical item scope",
            ));
        }
    }
    if object["decision_fingerprint"].as_array().map(Vec::as_slice)
        != Some(receipt_fingerprint(decisions).as_slice())
    {
        return Err(invalid());
    }
    Ok(())
}

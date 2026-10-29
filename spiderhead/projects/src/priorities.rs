//! Offline, fact-backed priority preview and explicit coverage policy application

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{json, Map, Value};

use crate::coverage::schema::hash;
use crate::coverage::{self, FORMAT, FORMAT_V2};
use crate::error::{value, ProjectsResult};
use spiderhead_core::formats::matches;

const PRIORITIES: &[&str] = &["P0", "P1", "P2"];

const PACKAGED: &str = include_str!("../data/project_priorities.json");

/// `str.casefold()` — full Unicode case folding, which `to_lowercase` does not perform
/// (e.g. `ß` folds to `ss`).
fn casefold(text: &str) -> String {
    icu_casemap::CaseMapper::new().fold_string(text).to_string()
}

fn matches_fact(value: &Value, choices: &[String]) -> bool {
    let Some(text) = value.as_str() else {
        return false;
    };
    let folded = casefold(text);
    choices.iter().any(|choice| casefold(choice) == folded)
}

/// `_policy`: validate a caller policy or load the packaged default.
fn policy(policy_input: Option<&Value>, catalogue: &Map<String, Value>) -> ProjectsResult<Value> {
    let raw = match policy_input {
        Some(v) => v.clone(),
        None => serde_json::from_str(PACKAGED).map_err(crate::error::ProjectsError::Json)?,
    };
    let object = raw.as_object().filter(|o| {
        o.keys().map(String::as_str).collect::<BTreeSet<_>>()
            == ["format", "rules"].into_iter().collect()
    });
    let Some(object) = object else {
        return Err(value("priority policy has unsupported fields"));
    };
    if !matches(
        object["format"].as_str(),
        "spiderhead.project-priorities.v1",
    ) || !object["rules"].is_array()
    {
        return Err(value("unsupported priority policy"));
    }
    let rules = object["rules"].as_array().unwrap();
    if rules.len() > 100 {
        return Err(value("priority policy has too many rules"));
    }
    let mut seen = BTreeSet::new();
    let mut normalized = Vec::new();
    for rule in rules {
        let object = rule.as_object().filter(|o| {
            o.keys().map(String::as_str).collect::<BTreeSet<_>>()
                == ["id", "facts", "priority", "items"].into_iter().collect()
        });
        let Some(object) = object else {
            return Err(value("priority rule has unsupported fields"));
        };
        let rule_id = object["id"].as_str().unwrap_or_default().to_owned();
        if rule_id.is_empty() || rule_id.chars().count() > 128 || !seen.insert(rule_id.clone()) {
            return Err(value("priority rule ID is invalid or duplicated"));
        }
        let priority = object["priority"].as_str().unwrap_or_default();
        if !PRIORITIES.contains(&priority) {
            return Err(value("priority rule has invalid priority"));
        }
        let facts_obj = object["facts"]
            .as_object()
            .filter(|f| f.len() <= 16)
            .ok_or_else(|| value("priority rule facts are invalid"))?;
        let mut facts = Map::new();
        for (name, values) in facts_obj {
            let values = values.as_array().filter(|v| !v.is_empty());
            if name.is_empty() || values.is_none() {
                return Err(value("priority rule facts are invalid"));
            }
            let values = values.unwrap();
            if values.iter().any(|item| {
                item.as_str()
                    .is_none_or(|s| s.is_empty() || s.chars().count() > 128)
            }) {
                return Err(value("priority rule fact values are invalid"));
            }
            let mut sorted: Vec<String> = values
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect();
            sorted.sort();
            sorted.dedup();
            facts.insert(name.clone(), json!(sorted));
        }
        let items = object["items"]
            .as_array()
            .filter(|i| !i.is_empty() && i.len() <= 100)
            .ok_or_else(|| value("priority rule items are invalid"))?;
        if items
            .iter()
            .any(|item| item.as_str().is_none_or(|s| !catalogue.contains_key(s)))
        {
            return Err(value("priority rule references an unknown catalogue item"));
        }
        let mut sorted_items: Vec<String> = items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
        sorted_items.sort();
        sorted_items.dedup();
        normalized.push(json!({
            "id": rule_id,
            "facts": Value::Object(facts),
            "priority": priority,
            "items": sorted_items,
        }));
    }
    Ok(json!({"format": object["format"], "rules": normalized}))
}

/// `_facts`: collapse `project["facts"]` into a name map plus a sorted snapshot.
fn facts(project: &Value) -> ProjectsResult<(Map<String, Value>, Vec<Value>)> {
    let mut values = Map::new();
    let mut snapshot = Vec::new();
    for fact in project["facts"].as_array().cloned().unwrap_or_default() {
        let name = fact["name"].as_str().unwrap_or_default().to_owned();
        if values.contains_key(&name) && values[&name] != fact["value"] {
            return Err(value(format!("conflicting project fact: {name}")));
        }
        values.insert(name, fact["value"].clone());
        snapshot.push(fact.clone());
    }
    snapshot.sort_by(|a, b| {
        a["name"]
            .as_str()
            .unwrap_or_default()
            .cmp(b["name"].as_str().unwrap_or_default())
    });
    Ok((values, snapshot))
}

fn decisions(
    document: &Value,
    policy: &Value,
    facts: &Map<String, Value>,
) -> ProjectsResult<Vec<Value>> {
    let mut matched: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for rule in policy["rules"].as_array().unwrap() {
        let matches_all = rule["facts"]
            .as_object()
            .unwrap()
            .iter()
            .all(|(name, values)| {
                let choices: Vec<String> = values
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect();
                matches_fact(&facts.get(name).cloned().unwrap_or(Value::Null), &choices)
            });
        if matches_all {
            for item_id in rule["items"].as_array().unwrap() {
                let id = item_id.as_str().unwrap_or_default().to_owned();
                matched.entry(id).or_default().push(rule.clone());
            }
        }
    }
    let mut decisions = Vec::new();
    let mut items: Vec<(&String, &Value)> = document["items"].as_object().unwrap().iter().collect();
    items.sort_by(|a, b| a.0.cmp(b.0));
    for (item_id, item) in items {
        let definition = &item["definition"];
        let before = json!({
            "priority": definition["priority"],
            "priority_origin": definition["priority_origin"],
        });
        let rules = matched.get(item_id.as_str()).cloned().unwrap_or_default();
        let (after, reason, consulted): (Value, String, Vec<String>);
        if definition["priority_origin"].as_str() == Some("operator") {
            after = before.clone();
            reason = "operator priority preserved".into();
            consulted = Vec::new();
        } else if rules.is_empty() {
            if definition["priority_origin"].as_str() == Some("policy") {
                after = json!({"priority": "P1", "priority_origin": "default"});
                reason = "policy no longer matched saved facts".into();
            } else {
                after = before.clone();
                reason = "no policy rule matched saved facts".into();
            }
            consulted = Vec::new();
        } else {
            let priorities: BTreeSet<&str> = rules
                .iter()
                .filter_map(|r| r["priority"].as_str())
                .collect();
            if priorities.len() != 1 {
                return Err(value(format!("conflicting priority rules for {item_id}")));
            }
            let priority = *priorities.iter().next().unwrap();
            after = json!({"priority": priority, "priority_origin": "policy"});
            let mut consulted_set = BTreeSet::new();
            for rule in &rules {
                for name in rule["facts"].as_object().unwrap().keys() {
                    consulted_set.insert(name.clone());
                }
            }
            consulted = consulted_set.into_iter().collect();
            let ids: Vec<&str> = rules.iter().filter_map(|r| r["id"].as_str()).collect();
            reason = format!("matched {}", ids.join(", "));
        }
        decisions.push(json!({
            "id": item_id,
            "before": before,
            "after": after,
            "reason": reason,
            "consulted_facts": consulted,
        }));
    }
    Ok(decisions)
}

fn receipt(policy: &Value, facts: &[Value], decisions: &[Value]) -> ProjectsResult<Value> {
    let mut ids: Vec<String> = decisions
        .iter()
        .filter_map(|d| d["id"].as_str().map(str::to_owned))
        .collect();
    ids.sort();
    let fingerprint: Vec<Value> = decisions
        .iter()
        .map(|d| {
            json!({
                "id": d["id"],
                "after": d["after"],
                "reason": d["reason"],
                "consulted_facts": d["consulted_facts"],
            })
        })
        .collect();
    Ok(json!({
        "policy": policy,
        "policy_hash": hash(policy)?,
        "facts": facts,
        "decisions": decisions,
        "item_ids": ids,
        "decision_fingerprint": fingerprint,
    }))
}

/// `project_priorities`: preview or persist offline priority decisions from saved facts.
pub fn project_priorities(
    directory: &Path,
    policy_value: Option<&Value>,
    apply: bool,
    expected_revision: Option<i64>,
    full: bool,
) -> ProjectsResult<Value> {
    let (root, project) = crate::workspace::load(directory)?;
    let document = coverage::store::read(&root, &project)?
        .ok_or_else(|| value("initialize the checklist first: run project-checklist-init"))?;
    if apply && expected_revision.is_none() {
        return Err(value(
            "expected_revision must be an integer when apply=true",
        ));
    }
    let catalogue = crate::catalogue::catalogue()?.clone();
    let selected = policy(policy_value, &catalogue)?;
    let (values, snapshot) = facts(&project)?;
    let decisions_list = decisions(&document, &selected, &values)?;
    let receipt_value = receipt(&selected, &snapshot, &decisions_list)?;
    let mut result = json!({
        "ok": true,
        "applied": false,
        "base_revision": document["revision"],
        "policy_hash": receipt_value["policy_hash"],
        "decisions": decisions_list,
        "checklist": coverage::coverage_status(directory, true)?,
    });
    if !full {
        let decisions_arr = result["decisions"].as_array().cloned().unwrap_or_default();
        let changed: Vec<Value> = decisions_arr
            .iter()
            .filter(|d| d["before"] != d["after"])
            .cloned()
            .collect();
        result["decision_counts"] = json!({
            "total": decisions_arr.len(),
            "changed": changed.len(),
            "unchanged": decisions_arr.len() - changed.len(),
        });
        result["changed_decisions"] = json!(changed);
        result["item_ids"] = receipt_value["item_ids"].clone();
        result.as_object_mut().unwrap().remove("decisions");
    }
    if !apply {
        return Ok(result);
    }
    let repeated = coverage::transaction(
        directory,
        expected_revision,
        |_root, locked_project, writable| {
            let (current_values, current_snapshot) = facts(locked_project)?;
            let current_decisions = decisions(writable, &selected, &current_values)?;
            let current_receipt = receipt(&selected, &current_snapshot, &current_decisions)?;
            let history = writable["priority_policy"]["applications"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let prior = history.last().map(|h| h["receipt"].clone());
            let same = prior.as_ref().is_some_and(|prior| {
                ["policy_hash", "facts", "decision_fingerprint"]
                    .iter()
                    .all(|key| prior.get(*key) == current_receipt.get(*key))
            });
            if same {
                return Ok(true);
            }
            for decision in &current_decisions {
                let id = decision["id"].as_str().unwrap_or_default();
                let definition = writable["items"][id]["definition"].clone();
                if decision["before"] != decision["after"] {
                    let mut def = definition;
                    def["priority"] = decision["after"]["priority"].clone();
                    def["priority_origin"] = decision["after"]["priority_origin"].clone();
                    coverage::items::set(writable["items"].as_object_mut().unwrap(), def);
                }
            }
            let mut history = history;
            history.push(json!({"applied_at": coverage::now(), "receipt": current_receipt}));
            writable["priority_policy"] = json!({"applications": history});
            if writable["format"].as_str() == Some(FORMAT) {
                writable["format"] = json!(FORMAT_V2);
                writable["version"] = json!(2);
            }
            result["applied"] = json!(true);
            result["decisions"] = json!(current_decisions);
            result["policy_hash"] = current_receipt["policy_hash"].clone();
            Ok(false)
        },
    )?;
    if repeated {
        // Returning early from a repeated apply skips the checklist refresh.
        return Ok(result);
    }
    result["checklist"] = coverage::coverage_status(directory, true)?;
    result["base_revision"] = json!(result["checklist"]["revision"].as_i64().unwrap_or(0) - 1);
    Ok(result)
}

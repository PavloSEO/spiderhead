//! `rdfa:copy` pattern expansion within the round/step/triple budgets.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::rdfa_reader::{Reader, RDFA_PATTERN};
use super::{MAX_RDFA_COPY_ROUNDS, MAX_RDFA_COPY_STEPS, MAX_RDFA_TRIPLES};
pub(super) fn push_list(
    lists: &mut Vec<((String, String), Vec<Value>)>,
    index: &mut HashMap<(String, String), usize>,
    subject: &str,
    predicate: &str,
    value: Value,
) {
    let key = (subject.to_owned(), predicate.to_owned());
    match index.get(&key) {
        Some(&at) => lists[at].1.push(value),
        None => {
            index.insert(key.clone(), lists.len());
            lists.push((key, vec![value]));
        }
    }
}

/// Apply `rdfa:copy` edges from `rdfa:Pattern` subjects, within the round/step/triple budgets.
pub(super) fn copy_patterns(
    r: &mut Reader,
    copy_edges: &mut Vec<(String, String)>,
    pattern_subjects: &HashSet<String>,
    triple_count: &mut usize,
) {
    let mut edge_set: HashSet<(String, String)> = copy_edges.iter().cloned().collect();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut valid_edges: Vec<(String, String)> = Vec::new();
    for edge in copy_edges.iter() {
        if seen.insert(edge.clone()) && pattern_subjects.contains(&edge.1) {
            valid_edges.push(edge.clone());
        }
    }
    let mut edges_by_subject: HashMap<String, Vec<String>> = HashMap::new();
    for (subject, target) in copy_edges.iter() {
        let targets = edges_by_subject.entry(subject.clone()).or_default();
        if !targets.contains(target) {
            targets.push(target.clone());
        }
    }
    if valid_edges.len() != edge_set.len() {
        r.reasons
            .insert("RDFa property-copy target is missing or is not an rdfa:Pattern".to_owned());
    }
    let mut copy_rounds = 0usize;
    let mut copy_steps = 0usize;
    let mut charge = |r: &mut Reader, add_triple: bool, triples: &mut usize| -> bool {
        copy_steps += 1;
        if copy_steps > MAX_RDFA_COPY_STEPS {
            r.reasons.insert(format!(
                "RDFa property copying stopped at the {MAX_RDFA_COPY_STEPS}-step limit"
            ));
            return false;
        }
        if add_triple {
            if *triples >= MAX_RDFA_TRIPLES {
                r.reasons.insert(format!(
                    "RDFa property copying stopped at the {MAX_RDFA_TRIPLES}-triple limit"
                ));
                return false;
            }
            *triples += 1;
        }
        true
    };
    let mut exhausted_rounds = true;
    while !valid_edges.is_empty() && copy_rounds < MAX_RDFA_COPY_ROUNDS {
        copy_rounds += 1;
        let mut changed = false;
        let mut budget_hit = false;
        for (subject, target) in valid_edges.clone() {
            if !charge(r, false, triple_count) {
                budget_hit = true;
                break;
            }
            let pattern = r.index.get(&target).and_then(|&a| r.entities[a].clone());
            let destination = r
                .index
                .get(&subject)
                .copied()
                .filter(|&a| r.entities[a].is_some());
            let (Some(pattern), Some(destination)) = (pattern, destination) else {
                r.reasons
                    .insert("RDFa property-copy subject is unavailable".to_owned());
                continue;
            };
            for (property_name, raw_value) in &pattern {
                if property_name.starts_with('_') {
                    continue;
                }
                let values: Vec<Value> = match raw_value {
                    Value::Array(l) => l.clone(),
                    other => vec![other.clone()],
                };
                if property_name == "@type" {
                    for type_name in values.iter().filter(|v| v.as_str() != Some(RDFA_PATTERN)) {
                        if !charge(r, true, triple_count) {
                            budget_hit = true;
                            break;
                        }
                        let item = r.item(destination);
                        let mut current: Vec<Value> = item
                            .get("@type")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                        if !current.contains(type_name) {
                            current.push(type_name.clone());
                            changed = true;
                        }
                        item.insert("@type".into(), Value::Array(current));
                    }
                    if budget_hit {
                        break;
                    }
                    continue;
                }
                if property_name.starts_with('@') {
                    continue;
                }
                for value in values {
                    if !charge(r, true, triple_count) {
                        budget_hit = true;
                        break;
                    }
                    if r.append(destination, property_name, value) {
                        changed = true;
                    }
                }
                if budget_hit {
                    break;
                }
            }
            if budget_hit {
                break;
            }
            let nested = edges_by_subject.get(&target).cloned().unwrap_or_default();
            for nested_target in nested {
                let nested_edge = (subject.clone(), nested_target.clone());
                if !pattern_subjects.contains(&nested_target) {
                    r.reasons.insert(
                        "RDFa property-copy target is missing or is not an rdfa:Pattern".to_owned(),
                    );
                    continue;
                }
                if !charge(r, true, triple_count) {
                    budget_hit = true;
                    break;
                }
                if !edge_set.contains(&nested_edge) {
                    edge_set.insert(nested_edge.clone());
                    copy_edges.push(nested_edge.clone());
                    edges_by_subject
                        .entry(subject.clone())
                        .or_default()
                        .push(nested_target);
                    valid_edges.push(nested_edge);
                    changed = true;
                }
            }
            if budget_hit {
                break;
            }
        }
        if budget_hit {
            valid_edges.clear();
            exhausted_rounds = false;
            break;
        }
        if !changed {
            exhausted_rounds = false;
            break;
        }
    }
    if exhausted_rounds && !valid_edges.is_empty() {
        r.reasons.insert(format!(
            "RDFa property copying stopped at the {MAX_RDFA_COPY_ROUNDS}-round limit"
        ));
    }
}

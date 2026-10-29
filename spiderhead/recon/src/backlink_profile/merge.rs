//! Merging of backlink observations by URL pair.

use super::*;

/// Transparent triage heuristic; it is never a search-engine penalty diagnosis.
pub(super) fn spam(observations: &[Json]) -> Json {
    let mut reasons: Vec<String> = Vec::new();
    let mut score = 0.0f64;
    let metrics: Vec<&Map<String, Json>> = observations
        .iter()
        .filter_map(|e| e["metrics"].as_object())
        .collect();
    let reported: Vec<f64> = metrics
        .iter()
        .filter_map(|m| m.get("provider_spam_score").and_then(|v| v.as_f64()))
        .collect();
    if !reported.is_empty() {
        score = reported.iter().cloned().fold(0.0, f64::max).min(100.0);
        reasons.push("provider_reported_spam_score".to_string());
    }
    if metrics.iter().any(|m| {
        m.get("external_links")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
            >= 1000.0
    }) {
        score = score.max(30.0);
        reasons.push("at_least_1000_external_links".to_string());
    }
    static SPAM_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(?:casino|viagra|payday loan)\b").unwrap_or_else(|e| panic!("{e}"))
    });
    if observations
        .iter()
        .any(|e| SPAM_RE.is_match(e["anchor"].as_str().unwrap_or("")))
    {
        score = (score + 30.0).min(100.0);
        reasons.push("spam_topic_anchor_requires_context".to_string());
    }
    json!({
        "score": if reasons.is_empty() { Json::Null } else { json!(score) },
        "reasons": reasons,
        "kind": "review_heuristic",
        "penalty_evidence": false,
    })
}

/// Deduplicate URL pairs while keeping every observation.
pub(super) fn merge(observations: &[Json], target_status: &BTreeMap<String, Json>) -> Vec<Json> {
    let mut pairs: BTreeMap<(String, String), Vec<Json>> = BTreeMap::new();
    for observation in observations {
        let key = (
            observation["source_url"].as_str().unwrap_or("").to_string(),
            observation["target_url"].as_str().unwrap_or("").to_string(),
        );
        let bucket = pairs.entry(key).or_default();
        if !bucket.contains(observation) {
            bucket.push(observation.clone());
        }
    }
    let mut rows = Vec::new();
    for ((donor, target), entries) in pairs {
        let first: Vec<&str> = entries
            .iter()
            .filter_map(|e| e["first_seen"].as_str())
            .collect();
        let last: Vec<&str> = entries
            .iter()
            .filter_map(|e| e["last_seen"].as_str())
            .collect();
        let rel_values: BTreeSet<Vec<String>> = entries
            .iter()
            .filter_map(|e| e["rel"].as_array().cloned())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .collect();
        let rel: Option<Vec<String>> = if rel_values.is_empty() {
            None
        } else {
            let mut all: Vec<String> = rel_values.iter().flatten().cloned().collect();
            all.sort();
            all.dedup();
            Some(all)
        };
        // Latest per source decides the merged state.
        let mut sorted_entries = entries.clone();
        sorted_entries.sort_by(|a, b| a["observed_at"].as_str().cmp(&b["observed_at"].as_str()));
        let mut latest: BTreeMap<String, &Json> = BTreeMap::new();
        for entry in &sorted_entries {
            latest.insert(entry["source"].as_str().unwrap_or("").to_string(), entry);
        }
        let states: BTreeSet<String> = latest
            .values()
            .filter_map(|e| e["state"].as_str())
            .filter(|s| *s != "unknown")
            .map(str::to_string)
            .collect();
        let state = if states.len() == 1 {
            states.iter().next().cloned().unwrap_or_default()
        } else {
            "unknown".to_string()
        };
        let categories: Vec<&str> = ["nofollow", "sponsored", "ugc"]
            .iter()
            .filter(|token| {
                rel.as_ref()
                    .map(|r| r.iter().any(|t| t == *token))
                    .unwrap_or(false)
            })
            .copied()
            .collect();
        let qualification_complete = entries
            .iter()
            .any(|e| e["qualification_complete"].as_bool() == Some(true));
        let mut follow_values: BTreeSet<bool> = entries
            .iter()
            .filter_map(|e| e["observed_follow"].as_bool())
            .collect();
        for entry in &entries {
            let entry_rels: BTreeSet<&str> = entry["rel"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str())
                .collect();
            if entry_rels
                .iter()
                .any(|r| ["nofollow", "sponsored", "ugc"].contains(r))
            {
                follow_values.insert(false);
            } else if entry["qualification_complete"].as_bool() == Some(true) {
                follow_values.insert(true);
            }
        }
        let follow = if follow_values.len() == 1 && rel_values.len() <= 1 {
            json!(follow_values.iter().next().copied().unwrap_or(false))
        } else {
            Json::Null
        };
        let anchors: Vec<String> = {
            let mut v: Vec<String> = entries
                .iter()
                .filter_map(|e| e["anchor"].as_str().map(str::to_string))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            v.sort();
            v
        };
        let sources: Vec<String> = {
            let mut v: Vec<String> = entries
                .iter()
                .filter_map(|e| e["source"].as_str().map(str::to_string))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            v.sort();
            v
        };
        let referring = Url::parse(&donor)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .map(|h| registrable_domain(&h))
            .unwrap_or_default();
        rows.push(json!({
            "source_url": donor,
            "target_url": if target.is_empty() { Json::Null } else { json!(target) },
            "referring_domain": referring,
            "anchors": anchors,
            "rel": rel,
            "rel_conflict": rel_values.len() > 1,
            "qualification_complete": qualification_complete,
            "unknown_rel_observations": entries.iter().filter(|e| e["rel"].is_null()).count(),
            "link_types": if categories.is_empty() {
                if qualification_complete { vec![json!("normal")] } else { vec![json!("unknown")] }
            } else {
                categories.iter().map(|c| json!(c)).collect()
            },
            "follow": follow,
            "follow_conflict": follow_values.len() > 1,
            "state": state,
            "state_conflict": states.len() > 1,
            "first_seen": first.iter().min().map(|s| json!(s)).unwrap_or(Json::Null),
            "last_seen": last.iter().max().map(|s| json!(s)).unwrap_or(Json::Null),
            "lost_at": entries
                .iter()
                .filter_map(|e| e["lost_at"].as_str())
                .max()
                .map(|s| json!(s))
                .unwrap_or(Json::Null),
            "sources": sources,
            "observations": entries,
            "spam": spam(&entries),
            "target_status": if target.is_empty() {
                Json::Null
            } else {
                target_status.get(&target).cloned().unwrap_or(Json::Null)
            },
        }));
    }
    rows
}

/// Summary block over merged rows.
pub(super) fn summary(rows: &[Json], domain_edges: &[Json]) -> Json {
    let mut anchors: Vec<(String, usize)> = Vec::new();
    let mut domains: BTreeMap<String, usize> = BTreeMap::new();
    for row in rows {
        for anchor in row["anchors"].as_array().into_iter().flatten() {
            if let Some(a) = anchor.as_str() {
                match anchors.iter_mut().find(|(name, _)| name == a) {
                    Some(entry) => entry.1 += 1,
                    None => anchors.push((a.to_string(), 1)),
                }
            }
        }
        if let Some(d) = row["referring_domain"].as_str() {
            *domains.entry(d.to_string()).or_default() += 1;
        }
    }
    for edge in domain_edges {
        if let Some(d) = edge["source_domain"].as_str() {
            domains.entry(d.to_string()).or_insert(0);
        }
    }
    // Order by count descending, keeping first-seen order for ties.
    let mut anchor_list: Vec<(String, usize)> = anchors.into_iter().collect();
    anchor_list.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    // First-seen order, like the provider counters it summarizes.
    let mut type_counts = serde_json::Map::new();
    for row in rows {
        for kind in row["link_types"].as_array().into_iter().flatten() {
            if let Some(k) = kind.as_str() {
                let seen = type_counts.get(k).and_then(Json::as_u64).unwrap_or(0);
                type_counts.insert(k.to_string(), json!(seen + 1));
            }
        }
    }
    json!({
        "backlinks": rows.len(),
        "referring_domains": domains.len(),
        "live": rows.iter().filter(|r| r["state"].as_str() == Some("live")).count(),
        "lost": rows.iter().filter(|r| r["state"].as_str() == Some("lost")).count(),
        "unknown_state": rows.iter().filter(|r| r["state"].as_str() == Some("unknown")).count(),
        "link_types": type_counts,
        "target_404": rows.iter().filter(|r| r["target_status"].as_u64() == Some(404)).count(),
        "target_redirect": rows
            .iter()
            .filter(|r| {
                r["target_status"]
                    .as_u64()
                    .map(|s| (300..400).contains(&s))
                    .unwrap_or(false)
            })
            .count(),
        "anchors": anchor_list
            .iter()
            .map(|(anchor, count)| json!({"anchor": anchor, "backlinks": count}))
            .collect::<Vec<_>>(),
        "domains": domains
            .iter()
            .map(|(domain, count)| json!({"domain": domain, "backlinks": count}))
            .collect::<Vec<_>>(),
    })
}

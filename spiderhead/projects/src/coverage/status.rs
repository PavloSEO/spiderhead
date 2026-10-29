use super::*;

/// `_status`: compose the complete offline completion view of one document.
pub fn status(
    root: &Path,
    document: Value,
    catalogue: &Map<String, Value>,
    project: Option<&Value>,
    verify_evidence: bool,
) -> ProjectsResult<Value> {
    let mut document = document;
    let (_, project_owned) = workspace_load(root)?;
    let project = project.cloned().unwrap_or(project_owned);
    let site = project["site"]["target"].as_str().unwrap_or_default();
    for (order, (item_id, entry)) in catalogue.iter().enumerate() {
        if !document["items"]
            .as_object()
            .is_none_or(|i| !i.contains_key(item_id))
        {
            continue;
        }
        let def = items::builtin(item_id, entry, order as i64, site);
        items::set(document["items"].as_object_mut().unwrap(), def);
    }
    let plans = document
        .get("plans")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let plan = plans.last().cloned();
    let plan_revision = plan
        .as_ref()
        .and_then(|p| p["revision"].as_i64())
        .unwrap_or(0);
    let agreement = plan
        .as_ref()
        .and_then(|p| p.get("tasks"))
        .cloned()
        .unwrap_or(json!({"kind": "all_agreed"}));
    let agreed: Option<std::collections::BTreeSet<String>> =
        if agreement["kind"].as_str() == Some("selection") {
            Some(
                agreement["ids"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|i| i.as_str().map(str::to_owned))
                    .collect(),
            )
        } else {
            None
        };
    let mut rows: Map<String, Value> = Map::new();
    let mut affected_cache: views::ScanCache = Default::default();
    let mut count_cache: views::CountCache = Default::default();
    let mut digests = std::collections::HashMap::new();
    let mut exclusions = Vec::new();
    for (item_id, item) in document["items"].as_object().unwrap().clone() {
        let definition = item["definition"].clone();
        let record = item["records"].as_array().and_then(|r| r.last().cloned());
        let source_stale = definition["kind"].as_str() != Some("custom")
            && (!catalogue.contains_key(&item_id)
                || definition["source_hash"].as_str()
                    != catalogue
                        .get(&item_id)
                        .and_then(|e| e["definition_hash"].as_str()));
        let mut stale_reason = String::new();
        if let Some(record) = &record {
            if !record.is_object()
                || record["definition_hash"].as_str()
                    != Some(completion_hash(&definition)?.as_str())
            {
                stale_reason = "item definition changed".into();
            } else {
                stale_reason = crate::evidence::evidence_stale(
                    root,
                    record,
                    catalogue,
                    &mut digests,
                    verify_evidence,
                );
            }
        }
        if record.is_some()
            && stale_reason.is_empty()
            && plan_revision > 0
            && record
                .as_ref()
                .and_then(|r| r.get("revision"))
                .and_then(Value::as_i64)
                .unwrap_or(0)
                < plan_revision
        {
            stale_reason = "execution predates the current agreed audit scope".into();
        }
        if source_stale
            && record.as_ref().and_then(|r| r["status"].as_str()) != Some("not_applicable")
            && stale_reason.is_empty()
        {
            stale_reason = "source definition changed or was removed".into();
        }
        let mut state = "not_run";
        if record.is_some() && stale_reason.is_empty() {
            match record.as_ref().and_then(|r| r["status"].as_str()) {
                Some("not_applicable") => state = "not_applicable",
                Some("succeeded") => state = "run",
                _ => {}
            }
        }
        let (applicability, applicability_detail) =
            if agreed.as_ref().is_some_and(|a| !a.contains(&item_id)) {
                ("not_agreed".to_owned(), None)
            } else {
                views::disposition(&item, record.as_ref(), &stale_reason)
            };
        if applicability == "excluded" {
            let mut entry = applicability_detail.clone().unwrap_or(json!({}));
            entry["id"] = json!(item_id);
            exclusions.push(entry);
        }
        let measurement = record
            .as_ref()
            .and_then(|r| r.get("measurement"))
            .cloned()
            .unwrap_or(json!({}));
        let partial_measurement = definition["execution_kind"].as_str() == Some("automatic")
            && definition["scope"]["urls"]
                .as_array()
                .is_none_or(Vec::is_empty)
            && measurement["state"].as_str() == Some("limited");
        let evidence_state = if stale_reason.contains("unverified:") {
            "unverified"
        } else if !stale_reason.is_empty() {
            "stale"
        } else if record.as_ref().is_some_and(|r| r.get("artifact").is_some()) && verify_evidence {
            "verified"
        } else if record.as_ref().is_some_and(|r| r.get("artifact").is_some()) {
            "metadata_matches"
        } else {
            "not_applicable"
        };
        let reason = if !stale_reason.is_empty() {
            stale_reason.clone()
        } else {
            record
                .as_ref()
                .and_then(|r| r.get("reason"))
                .and_then(Value::as_str)
                .unwrap_or("not attempted")
                .to_owned()
        };
        rows.insert(
            item_id.clone(),
            json!({
                "id": item_id,
                "title": definition["title"],
                "kind": definition["kind"],
                "scope": definition["scope"],
                "priority": definition["priority"],
                "priority_origin": definition["priority_origin"],
                "priority_reason": "not applied",
                "order": definition["order"],
                "enabled": definition["enabled"],
                "execution_kind": definition["execution_kind"],
                "state": state,
                "applicability": applicability,
                "applicability_reason": match applicability.as_str() {
                    "pending_exclusion" => applicability_detail
                        .as_ref()
                        .and_then(|d| d.get("reason"))
                        .cloned()
                        .unwrap_or(Value::Null),
                    "not_agreed" => json!("outside the agreed task selection"),
                    _ => Value::Null,
                },
                "exclusion": if applicability == "excluded" {
                    applicability_detail.clone().unwrap_or(Value::Null)
                } else {
                    Value::Null
                },
                "stale": !stale_reason.is_empty() || source_stale,
                "reason": reason,
                "attempt_status": record
                    .as_ref()
                    .and_then(|r| r.get("status"))
                    .cloned()
                    .unwrap_or(json!("not_run")),
                "actor": record
                    .as_ref()
                    .and_then(|r| r.get("actor").or_else(|| r.get("reviewer")))
                    .cloned()
                    .unwrap_or(Value::Null),
                "measurement": record.as_ref().and_then(|r| r.get("measurement")).cloned().unwrap_or(Value::Null),
                "affected_urls": views::affected_url_count(
                    root,
                    &definition,
                    record.as_ref().and_then(|r| r.get("measurement")),
                    &mut affected_cache,
                    &mut count_cache,
                ),
                "task": {
                    "assignee": definition.get("assignee").cloned().unwrap_or(Value::Null),
                    "description": definition.get("description").cloned().unwrap_or(Value::Null),
                    "acceptance_criterion": definition.get("acceptance_criterion").cloned().unwrap_or(Value::Null),
                    "finding_ref": definition.get("finding_ref").cloned().unwrap_or(Value::Null),
                    "method_ref": definition.get("method_ref").cloned().unwrap_or(Value::Null),
                    "competitors": definition.get("competitors").cloned().unwrap_or(json!([])),
                },
                "evidence_verification": {
                    "mode": if verify_evidence { "bytes" } else { "metadata_only" },
                    "state": evidence_state,
                    "verified_at": record
                        .as_ref()
                        .and_then(|r| r.get("artifact_verification"))
                        .and_then(|a| a.get("verified_at"))
                        .cloned()
                        .unwrap_or(Value::Null),
                },
                "blocked_by": [],
                "complete": state == "run"
                    && applicability == "applicable"
                    && !partial_measurement
                    && (definition["execution_kind"].as_str() == Some("automatic")
                        || record.as_ref().is_some_and(views::owner_decided)),
                "definition_versions": item["definitions"].as_array().map_or(0, Vec::len),
                "attempts": item["records"].as_array().map_or(0, Vec::len),
            }),
        );
    }
    // Resolve blocked_by (deps of non-complete applicable rows).
    let mut visited = std::collections::BTreeSet::new();
    let ids: Vec<String> = rows.keys().cloned().collect();
    for item_id in &ids {
        resolve_blocked(item_id, &mut rows, &document, &mut visited);
    }
    let mut ordered: Vec<Value> = rows.values().cloned().collect();
    ordered.sort_by(|a, b| {
        (
            a["order"].as_i64().unwrap_or(0),
            a["id"].as_str().unwrap_or_default(),
        )
            .cmp(&(
                b["order"].as_i64().unwrap_or(0),
                b["id"].as_str().unwrap_or_default(),
            ))
    });
    let denominator_rows: Vec<Value> = ordered
        .iter()
        .filter(|r| {
            matches!(
                r["applicability"].as_str(),
                Some("applicable" | "pending_exclusion")
            )
        })
        .cloned()
        .collect();
    let count_of = |name: &str| {
        denominator_rows
            .iter()
            .filter(|r| r["state"].as_str() == Some(name))
            .count() as i64
    };
    let counts = json!({
        "run": count_of("run"),
        "not_applicable": count_of("not_applicable"),
        "not_run": count_of("not_run"),
        "total": denominator_rows.len(),
        "disabled": ordered.iter().filter(|r| r["enabled"].as_bool() == Some(false)).count(),
        "excluded": exclusions.len(),
        "pending_exclusion": ordered.iter().filter(|r| r["applicability"].as_str() == Some("pending_exclusion")).count(),
        "not_agreed": ordered.iter().filter(|r| r["applicability"].as_str() == Some("not_agreed")).count(),
        "complete": denominator_rows.iter().filter(|r| r["complete"].as_bool() == Some(true)).count(),
        "stale": denominator_rows.iter().filter(|r| r["stale"].as_bool() == Some(true)).count(),
        "remaining": denominator_rows.len() as i64 - denominator_rows.iter().filter(|r| r["complete"].as_bool() == Some(true)).count() as i64,
    });
    let view = |filter: &dyn Fn(&Value) -> bool| -> Vec<Value> {
        denominator_rows
            .iter()
            .filter(|r| filter(r))
            .map(|r| r["id"].clone())
            .collect()
    };
    let views_out = json!({
        "running": view(&|r| r["attempt_status"].as_str() == Some("running") && r["stale"].as_bool() != Some(true)),
        "blocked": view(&|r| !r["blocked_by"].as_array().unwrap_or(&Vec::new()).is_empty()
            || matches!(r["attempt_status"].as_str(), Some("failed" | "unavailable"))),
        "waiting_for_manual_review": view(&|r| r["complete"].as_bool() != Some(true)
            && matches!(r["execution_kind"].as_str(), Some("manual" | "deliverable"))),
        "deliverable_ready": view(&|r| r["complete"].as_bool() == Some(true)
            && r["state"].as_str() == Some("run")
            && r["execution_kind"].as_str() == Some("deliverable")),
        "pending_exclusion": ordered.iter().filter(|r| r["applicability"].as_str() == Some("pending_exclusion")).map(|r| r["id"].clone()).collect::<Vec<_>>(),
        "not_agreed": ordered.iter().filter(|r| r["applicability"].as_str() == Some("not_agreed")).map(|r| r["id"].clone()).collect::<Vec<_>>(),
        "remaining": view(&|r| r["complete"].as_bool() != Some(true)),
    });
    let by_kind = {
        let mut out = Map::new();
        for kind in ["check", "skill", "scenario", "custom"] {
            out.insert(
                kind.to_owned(),
                json!({
                    "total": denominator_rows.iter().filter(|r| r["kind"].as_str() == Some(kind)).count(),
                    "complete": denominator_rows.iter().filter(|r| r["kind"].as_str() == Some(kind) && r["complete"].as_bool() == Some(true)).count(),
                }),
            );
        }
        out
    };
    let applications = document
        .get("priority_policy")
        .and_then(|p| p.get("applications"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let latest_policy = applications.last().cloned();
    if let Some(latest) = &latest_policy {
        for decision in latest["receipt"]["decisions"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            let id = decision["id"].as_str().unwrap_or_default().to_owned();
            if let Some(row) = ordered
                .iter_mut()
                .find(|row| row["id"].as_str() == Some(id.as_str()))
            {
                row["priority_reason"] = if row["priority_origin"].as_str() == Some("operator") {
                    json!("operator priority preserved")
                } else {
                    decision["reason"].clone()
                };
            }
        }
    }
    let saved_facts_state = if let Some(latest) = &latest_policy {
        let mut current_facts = project["facts"].as_array().cloned().unwrap_or_default();
        current_facts.sort_by(|a, b| {
            a["name"]
                .as_str()
                .unwrap_or_default()
                .cmp(b["name"].as_str().unwrap_or_default())
        });
        if Value::Array(current_facts) == latest["receipt"]["facts"] {
            "matches_current_project"
        } else {
            "changed_since_application"
        }
    } else {
        "not_applied"
    };
    let ordered_refs: Vec<Value> = ordered.clone();
    let automatic_rows: Vec<&Value> = ordered_refs
        .iter()
        .filter(|r| r["execution_kind"].as_str() == Some("automatic"))
        .collect();
    let manual_rows: Vec<&Value> = ordered_refs
        .iter()
        .filter(|r| r["execution_kind"].as_str() == Some("manual"))
        .collect();
    let deliverable_rows: Vec<&Value> = ordered_refs
        .iter()
        .filter(|r| r["execution_kind"].as_str() == Some("deliverable"))
        .collect();
    let all_rows: Vec<&Value> = ordered_refs.iter().collect();
    let mut coverage = json!({
        "audit_tasks": views::task_axis(&all_rows, "applicable audit tasks completed over the agreed applicable task set"),
        "checks": views::task_axis(&automatic_rows, "applicable automatic checks completed over the agreed applicable check set"),
        "manual_review": views::task_axis(&manual_rows, "manual-review tasks completed over the agreed applicable manual-review set"),
        "deliverable_review": views::task_axis(&deliverable_rows, "approved deliverables over the agreed applicable deliverable set"),
        "url_population": views::url_axis(&ordered_refs, plan.as_ref()),
    });
    coverage["checks"]["measured_urls"] = coverage["url_population"]["measured_urls"].clone();
    let custom_tasks: Vec<&Value> = ordered_refs
        .iter()
        .filter(|r| r["kind"].as_str() == Some("custom"))
        .collect();
    let measured_affected: Vec<i64> = custom_tasks
        .iter()
        .filter(|r| r["affected_urls"]["state"].as_str() == Some("measured"))
        .map(|r| r["affected_urls"]["count"].as_i64().unwrap_or(0))
        .collect();
    let affected_unmeasured = custom_tasks.len() - measured_affected.len();
    let task_kpis = json!({
        "scope": "primary_project",
        "total": custom_tasks.len(),
        "complete": custom_tasks.iter().filter(|r| r["complete"].as_bool() == Some(true)).count(),
        "remaining": custom_tasks.iter().filter(|r| r["complete"].as_bool() != Some(true)).count(),
        "assigned": custom_tasks.iter().filter(|r| !r["task"]["assignee"].is_null()).count(),
        "affected_urls": {
            "state": if !custom_tasks.is_empty() && affected_unmeasured == 0 {
                "measured"
            } else if !measured_affected.is_empty() {
                "partial"
            } else {
                "unavailable"
            },
            "count": if measured_affected.is_empty() { Value::Null } else { json!(measured_affected.iter().sum::<i64>()) },
            "measured_tasks": measured_affected.len(),
            "unmeasured_tasks": affected_unmeasured,
            "reason": if !custom_tasks.is_empty() && affected_unmeasured == 0 {
                Value::Null
            } else {
                json!("exact affected-URL evidence is unavailable for one or more custom tasks")
            },
        },
    });
    let url_population = &coverage["url_population"];
    let population_finished = url_population["state"].as_str() == Some("measured")
        && url_population["denominator"].as_i64().unwrap_or(0) > 0
        && url_population["numerator"].as_i64() == url_population["denominator"].as_i64();
    let tasks = &coverage["audit_tasks"];
    Ok(json!({
        "state": "initialized",
        "revision": document["revision"],
        "project_uuid": document["project_uuid"],
        "counts": counts,
        "task_kpis": task_kpis,
        "by_kind": by_kind,
        "views": views_out,
        "items": ordered,
        "coverage": coverage,
        "plan": plan,
        "plan_history": plans.iter().take(plans.len().saturating_sub(1)).cloned().collect::<Vec<_>>(),
        "exclusions": exclusions,
        "priority_policy": {
            "state": if latest_policy.is_some() { "applied" } else { "not_applied" },
            "policy_hash": latest_policy.as_ref().map(|l| l["receipt"]["policy_hash"].clone()).unwrap_or(Value::Null),
            "applied_at": latest_policy.as_ref().map(|l| l["applied_at"].clone()).unwrap_or(Value::Null),
            "facts_state": saved_facts_state,
        },
        "complete": tasks["denominator"].as_i64().unwrap_or(0) > 0
            && tasks["unfinished"].as_i64() == Some(0)
            && tasks["pending_exclusion"].as_i64() == Some(0)
            && population_finished,
    }))
}

pub(super) fn resolve_blocked(
    item_id: &str,
    rows: &mut Map<String, Value>,
    document: &Value,
    visited: &mut std::collections::BTreeSet<String>,
) {
    if visited.contains(item_id) {
        return;
    }
    let deps = document["items"][item_id]["definition"]["dependencies"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let applicability = rows[item_id]["applicability"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    for dep in deps {
        let dep = dep.as_str().unwrap_or_default().to_owned();
        resolve_blocked(&dep, rows, document, visited);
        if matches!(applicability.as_str(), "excluded" | "not_agreed") {
            continue;
        }
        let dep_app = rows[&dep]["applicability"].as_str().unwrap_or_default();
        let dep_complete = rows[&dep]["complete"].as_bool().unwrap_or(false);
        if matches!(dep_app, "applicable" | "pending_exclusion" | "not_agreed") && !dep_complete {
            rows[item_id]["blocked_by"]
                .as_array_mut()
                .unwrap()
                .push(json!(dep));
        }
    }
    if !rows[item_id]["blocked_by"]
        .as_array()
        .unwrap_or(&Vec::new())
        .is_empty()
    {
        rows[item_id]["complete"] = json!(false);
    }
    visited.insert(item_id.to_owned());
}

/// `coverage_status`: read definitions and evidence without writes or network requests.
pub fn coverage_status(directory: &Path, verify_evidence: bool) -> ProjectsResult<Value> {
    let (root, project) = workspace_load(directory)?;
    let document = store::read(&root, &project)?;
    let Some(document) = document else {
        return Ok(json!({
            "state": "not_initialized",
            "reason": "coverage checklist is initialized by project checklist setup, not project creation",
        }));
    };
    let catalogue = crate::catalogue::catalogue()?.clone();
    status(&root, document, &catalogue, Some(&project), verify_evidence)
}

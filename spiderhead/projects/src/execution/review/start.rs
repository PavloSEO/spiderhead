use super::*;

/// `start`: register a new workflow run for a scenario or skill.
pub fn start(
    directory: &Path,
    scenario_id: &Value,
    steps: &Value,
    expected_revision: i64,
    context: &Value,
) -> ProjectsResult<Value> {
    let (root, project, mut document) = load(directory)?;
    if document["revision"].as_i64() != Some(expected_revision) {
        return Err(value("execution revision conflict"));
    }
    let scenario_id = text_bound(scenario_id, "scenario_id", 512)?;
    let step_list = steps.as_array();
    if step_list.is_none_or(|list| {
        list.is_empty() || list.len() > 200 || {
            let unique: std::collections::HashSet<String> =
                list.iter().map(crate::pyjson::dumps_compact).collect();
            unique.len() != list.len()
        }
    }) {
        return Err(value(
            "steps must be a unique nonempty list of at most 200 checklist ids",
        ));
    }
    let step_list = step_list.cloned().unwrap_or_default();
    let rows = rows(&root, None)?;
    let scenario = rows.get(&scenario_id);
    if scenario.is_none_or(|row| {
        !matches!(
            row.get("kind").and_then(Value::as_str),
            Some("scenario") | Some("skill")
        )
    }) {
        return Err(value(
            "scenario_id must be a registered project scenario or skill",
        ));
    }
    if step_list
        .iter()
        .any(|step| step.as_str().is_none_or(|id| !rows.contains_key(id)))
    {
        return Err(value(
            "every workflow step must be an existing checklist identity",
        ));
    }
    let attempt = 1 + document["runs"]
        .as_array()
        .map(|runs| {
            runs.iter()
                .filter(|run| run["scenario"]["id"].as_str() == Some(scenario_id.as_str()))
                .count() as i64
        })
        .unwrap_or(0);
    let context_value = resolve_context(context, &root, &project, &rows)?;
    let phase = context
        .as_object()
        .and_then(|c| c.get("phase").cloned())
        .unwrap_or_else(|| json!("registered"));
    let mut run_steps = Vec::new();
    for step in &step_list {
        let id = step.as_str().unwrap_or_default();
        run_steps.push(json!({
            "id": step,
            "definition_hash": catalogue_hash(id)?,
            "kind": rows[id]["kind"],
            "execution_kind": rows[id]["execution_kind"],
            "state": "pending",
            "evidence": [],
        }));
    }
    let run = json!({
        "id": format!("run:{uuid}", uuid = Uuid::new_v4()),
        "attempt": attempt,
        "scenario": {"id": scenario_id, "definition_hash": catalogue_hash(&scenario_id)?},
        "context": context_value,
        "phase": phase,
        "steps": run_steps,
        "state": "running",
    });
    if let Some(runs) = document["runs"].as_array_mut() {
        runs.push(run.clone());
    }
    save(&root, &mut document, expected_revision)?;
    Ok(json!({
        "ok": true,
        "revision": document["revision"],
        "run": public_run(&run, &rows, Some(&root), &mut HashMap::new(), None, None, true)?,
        "next_action": step_list[0].clone(),
    }))
}

/// `checkpoint`: record one pending step outcome in registered order.
#[allow(clippy::too_many_arguments)]
pub fn checkpoint(
    directory: &Path,
    run_id: &str,
    step_id: &str,
    state: &str,
    evidence_input: &Value,
    expected_revision: i64,
    review_input: &Value,
    phase: Option<&str>,
) -> ProjectsResult<Value> {
    let (root, _project, mut document) = load(directory)?;
    if document["revision"].as_i64() != Some(expected_revision) {
        return Err(value("execution revision conflict"));
    }
    if !STEP_STATES.contains(&state) {
        return Err(value("checkpoint state is invalid"));
    }
    let run_index = document["runs"]
        .as_array()
        .and_then(|runs| {
            runs.iter()
                .position(|run| run["id"].as_str() == Some(run_id))
        })
        .ok_or_else(|| value("workflow run is not running"))?;
    {
        let run = &document["runs"][run_index];
        if !matches!(run["state"].as_str(), Some("running") | Some("blocked")) {
            return Err(value("workflow run is not running"));
        }
        let steps = run["steps"].as_array().cloned().unwrap_or_default();
        let step_index = steps
            .iter()
            .position(|step| step["id"].as_str() == Some(step_id))
            .ok_or_else(|| value("workflow step is not pending"))?;
        if steps[step_index]["state"].as_str() != Some("pending") {
            return Err(value("workflow step is not pending"));
        }
        if steps[..step_index]
            .iter()
            .any(|step| step["state"].as_str() == Some("pending"))
        {
            return Err(value(
                "workflow steps must be checkpointed in registered order",
            ));
        }
    }
    let rows = rows(&root, None)?;
    let mut digests: Digests = HashMap::new();
    let (max_seconds, max_bytes) = hash_limits(&root)?;
    let deadline = pytime::monotonic() + max_seconds;
    let stale = stale_dependencies(
        &document["runs"][run_index],
        &rows,
        Some(&root),
        &mut digests,
        Some(deadline),
        Some(max_bytes),
        true,
    )?;
    let current = rows.get(step_id);
    if state == "succeeded"
        && (!stale.is_empty()
            || current.is_none()
            || current.is_some_and(|row| {
                !falsy(row.get("stale").unwrap_or(&Value::Null))
                    || !falsy(row.get("blocked_by").unwrap_or(&Value::Null))
            }))
    {
        return Err(value(
            "stale or blocked dependencies must be reconciled before a successful checkpoint",
        ));
    }
    let records = evidence(
        evidence_input,
        if state == "succeeded" {
            Some(&root)
        } else {
            None
        },
        &mut digests,
        Some(deadline),
        Some(max_bytes),
    )?;
    if state == "succeeded" && records.is_empty() {
        return Err(value(
            "successful workflow checkpoints require exact evidence",
        ));
    }
    let execution_kind;
    {
        let run = &mut document["runs"][run_index];
        let step = run["steps"]
            .as_array_mut()
            .and_then(|steps| {
                steps
                    .iter_mut()
                    .find(|step| step["id"].as_str() == Some(step_id))
            })
            .ok_or_else(|| value("workflow step is not pending"))?;
        step["state"] = json!(state);
        step["evidence"] = json!(records);
        if state == "succeeded" {
            step["evidence_verification"] = verification_receipt(&root, &records, &digests)?;
        }
        execution_kind = step["execution_kind"]
            .as_str()
            .unwrap_or_default()
            .to_string();
    }
    if let Some(approved) = review(review_input, &execution_kind, state)? {
        if let Some(step) = document["runs"][run_index]["steps"]
            .as_array_mut()
            .and_then(|steps| steps.iter_mut().find(|s| s["id"].as_str() == Some(step_id)))
        {
            step["review"] = approved;
        }
    }
    if let Some(phase) = phase {
        document["runs"][run_index]["phase"] = json!(text_bound(&json!(phase), "phase", 128)?);
    }
    {
        let run = &mut document["runs"][run_index];
        let all_succeeded = run["steps"].as_array().is_some_and(|steps| {
            steps
                .iter()
                .all(|step| step["state"].as_str() == Some("succeeded"))
        });
        if state == "succeeded" && all_succeeded {
            run["state"] = json!("completed");
        } else if state != "succeeded" {
            run["state"] = json!(if state == "interrupted" {
                "interrupted"
            } else {
                "blocked"
            });
        }
    }
    save(&root, &mut document, expected_revision)?;
    let run = document["runs"][run_index].clone();
    let public = public_run(
        &run,
        &rows,
        Some(&root),
        &mut digests,
        Some(deadline),
        Some(max_bytes),
        true,
    )?;
    Ok(json!({
        "ok": true,
        "revision": document["revision"],
        "run": public,
        "next_action": public["next_action"],
    }))
}

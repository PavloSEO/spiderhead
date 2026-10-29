use super::*;

/// `resume`: deliberately reopen the first non-successful step.
pub fn resume(directory: &Path, run_id: &str, expected_revision: i64) -> ProjectsResult<Value> {
    let (root, _project, mut document) = load(directory)?;
    if document["revision"].as_i64() != Some(expected_revision) {
        return Err(value("execution revision conflict"));
    }
    let run_index = document["runs"]
        .as_array()
        .and_then(|runs| {
            runs.iter()
                .position(|run| run["id"].as_str() == Some(run_id))
        })
        .ok_or_else(|| value("workflow run is not resumable"))?;
    if !matches!(
        document["runs"][run_index]["state"].as_str(),
        Some("blocked") | Some("interrupted") | Some("completed")
    ) {
        return Err(value("workflow run is not resumable"));
    }
    let rows = rows(&root, None)?;
    let mut digests: Digests = HashMap::new();
    let (max_seconds, max_bytes) = hash_limits(&root)?;
    let deadline = pytime::monotonic() + max_seconds;
    let public = public_run(
        &document["runs"][run_index],
        &rows,
        Some(&root),
        &mut digests,
        Some(deadline),
        Some(max_bytes),
        true,
    )?;
    if public["stale_dependencies"]
        .as_array()
        .is_some_and(|stale| {
            stale.iter().any(|item| {
                item["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("budget"))
            })
        })
    {
        return Err(value(
            "workflow evidence verification budget exceeded; retry before resuming",
        ));
    }
    let stale_index = reopen_index(
        &document["runs"][run_index],
        &public["stale_dependencies"]
            .as_array()
            .cloned()
            .unwrap_or_default(),
    );
    let retry_index;
    if let Some(index) = stale_index {
        if stale_checklist_dependency(&document["runs"][run_index], &rows) {
            return Err(value(
                "reconcile stale checklist dependencies before resuming workflow",
            ));
        }
        {
            let run = &mut document["runs"][run_index];
            let count = run["steps"].as_array().map(Vec::len).unwrap_or(0);
            for offset in index..count {
                let step = &mut run["steps"][offset];
                if !falsy(step.get("evidence").unwrap_or(&Value::Null)) {
                    step["stale_evidence"] = step["evidence"].clone();
                }
                step["state"] = json!("pending");
                step["evidence"] = json!([]);
                if let Some(object) = step.as_object_mut() {
                    object.remove("review");
                    object.remove("evidence_verification");
                }
            }
        }
        refresh_current_definition_hashes(&mut document["runs"][run_index], &rows)?;
        retry_index = index;
    } else {
        retry_index = document["runs"][run_index]["steps"]
            .as_array()
            .and_then(|steps| {
                steps
                    .iter()
                    .position(|step| step["state"].as_str() != Some("succeeded"))
            })
            .ok_or_else(|| value("completed workflow run cannot be resumed"))?;
        let step = &mut document["runs"][run_index]["steps"][retry_index];
        step["state"] = json!("pending");
        step["evidence"] = json!([]);
        if let Some(object) = step.as_object_mut() {
            object.remove("review");
            object.remove("evidence_verification");
        }
    }
    let retry_id = document["runs"][run_index]["steps"][retry_index]["id"].clone();
    document["runs"][run_index]["state"] = json!("running");
    save(&root, &mut document, expected_revision)?;
    let run = document["runs"][run_index].clone();
    Ok(json!({
        "ok": true,
        "revision": document["revision"],
        "run": public_run(&run, &rows, Some(&root), &mut digests, Some(deadline), Some(max_bytes), true)?,
        "next_action": retry_id,
    }))
}

/// `status`: every run with live dependency staleness.
pub fn status(
    directory: &Path,
    coverage: Option<&Value>,
    verify_evidence: bool,
) -> ProjectsResult<Value> {
    let (root, _project, document) = load(directory)?;
    let rows = rows(&root, coverage)?;
    let mut digests: Digests = HashMap::new();
    let (max_seconds, max_bytes) = hash_limits(&root)?;
    let deadline = pytime::monotonic() + max_seconds;
    let mut runs = Vec::new();
    for run in document["runs"].as_array().cloned().unwrap_or_default() {
        runs.push(public_run(
            &run,
            &rows,
            Some(&root),
            &mut digests,
            Some(deadline),
            Some(max_bytes),
            verify_evidence,
        )?);
    }
    let active = runs
        .iter()
        .rev()
        .find(|run| run["state"].as_str() == Some("running"));
    let resumable = runs.iter().rev().find(|run| {
        matches!(
            run["state"].as_str(),
            Some("blocked") | Some("interrupted") | Some("stale")
        )
    });
    Ok(json!({
        "ok": true,
        "revision": document["revision"],
        "runs": runs,
        "next_action": active
            .map(|run| run["next_action"].clone())
            .or_else(|| resumable.map(|run| run["next_action"].clone()))
            .unwrap_or(Value::Null),
        "resumable_run": resumable.and_then(|run| run["id"].as_str().map(str::to_string)),
    }))
}

/// `execute`: checkpoint supplied local outcomes; executes no commands or network work.
pub fn execute(
    directory: &Path,
    scenario_id: &Value,
    steps: &Value,
    outcomes: &Value,
    context: &Value,
) -> ProjectsResult<Value> {
    let revision = status(directory, None, true)?["revision"]
        .as_i64()
        .unwrap_or(0);
    let started = start(directory, scenario_id, steps, revision, context)?;
    let run = started["run"].clone();
    let mut revision = started["revision"].as_i64().unwrap_or(0);
    let outcome_list = outcomes.as_array();
    let outcome_ids: Vec<String> = outcome_list
        .map(|list| {
            list.iter()
                .filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let step_ids: Vec<String> = steps
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if outcome_list.is_none() || outcome_ids != step_ids {
        return Err(value(
            "outcomes must cover registered steps once in declared order",
        ));
    }
    for outcome in outcome_list.cloned().unwrap_or_default() {
        let result = checkpoint(
            directory,
            run["id"].as_str().unwrap_or_default(),
            outcome
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            outcome
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            outcome.get("evidence").unwrap_or(&Value::Null),
            revision,
            outcome.get("review").unwrap_or(&Value::Null),
            outcome.get("phase").and_then(Value::as_str),
        )?;
        revision = result["revision"].as_i64().unwrap_or(0);
        if result["run"]["state"].as_str() != Some("running") {
            return Ok(result);
        }
    }
    status(directory, None, true)
}

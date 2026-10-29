use super::*;

mod start;
pub use start::*;

pub(super) fn review(
    item: &Value,
    execution_kind: &str,
    state: &str,
) -> ProjectsResult<Option<Value>> {
    if state != "succeeded" || execution_kind == "automatic" {
        return Ok(None);
    }
    let keys: BTreeSet<&str> = item
        .as_object()
        .map(|object| object.keys().map(String::as_str).collect())
        .unwrap_or_default();
    if item.as_object().is_none_or(|o| o.len() != 3)
        || !keys.contains("actor")
        || !keys.contains("state")
        || !keys.contains("reason")
    {
        return Err(value(
            "manual and deliverable steps require an explicit approved review",
        ));
    }
    if item["state"].as_str() != Some("approved") {
        return Err(value(
            "manual and deliverable steps require an approved review",
        ));
    }
    Ok(Some(json!({
        "actor": text_bound(&item["actor"], "review actor", 128)?,
        "state": "approved",
        "reason": text_bound(&item["reason"], "review reason", 512)?,
    })))
}

pub(super) fn rows(root: &Path, view: Option<&Value>) -> ProjectsResult<BTreeMap<String, Value>> {
    let owned;
    let view = match view {
        Some(view) => view,
        None => {
            owned = coverage_status(root, true)?;
            &owned
        }
    };
    if view.get("state").and_then(Value::as_str) == Some("not_initialized") {
        return Ok(BTreeMap::new());
    }
    let Some(items) = view.get("items").and_then(Value::as_array) else {
        return Err(value(
            "initialized coverage status has invalid checklist items",
        ));
    };
    if items
        .iter()
        .any(|row| !row.is_object() || !row.as_object().unwrap().contains_key("id"))
    {
        return Err(value(
            "initialized coverage status has invalid checklist items",
        ));
    }
    Ok(items
        .iter()
        .filter_map(|row| {
            row.get("id")
                .and_then(Value::as_str)
                .map(|id| (id.to_string(), row.clone()))
        })
        .collect())
}

pub(super) fn stale_dependencies(
    run: &Value,
    rows: &BTreeMap<String, Value>,
    root: Option<&Path>,
    digests: &mut Digests,
    deadline: Option<f64>,
    max_bytes: Option<u64>,
    verify_evidence: bool,
) -> ProjectsResult<Vec<Value>> {
    let mut stale: Vec<Value> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let limits = match root {
        Some(root) if deadline.is_none() || max_bytes.is_none() => Some(hash_limits(root)?),
        _ => None,
    };
    let max_seconds = limits.map(|(s, _)| s).unwrap_or(EVIDENCE_HASH_SECONDS);
    let max_bytes = max_bytes
        .or_else(|| limits.map(|(_, b)| b))
        .unwrap_or(EVIDENCE_HASH_BYTES);
    let deadline = deadline.unwrap_or_else(|| pytime::monotonic() + max_seconds);

    macro_rules! add {
        ($id:expr, $reason:expr) => {{
            let key = crate::pyjson::dumps_compact(&$id);
            if seen.insert(key) {
                stale.push(json!({"id": $id, "reason": $reason}));
            }
        }};
    }

    let scenario_id = get(get(run, "scenario")?, "id")?.clone();
    match scenario_id.as_str().and_then(|id| rows.get(id)) {
        None => add!(scenario_id, "registered scenario or skill was removed"),
        Some(scenario) if !falsy(scenario.get("stale").unwrap_or(&Value::Null)) => {
            add!(
                scenario_id,
                scenario.get("reason").cloned().unwrap_or(Value::Null)
            )
        }
        Some(_) => {
            if catalogue_hash(scenario_id.as_str().unwrap_or_default())?
                != get(run, "scenario")?.get("definition_hash").cloned()
            {
                add!(scenario_id, "scenario or skill definition changed");
            }
        }
    }
    let prompt = run
        .get("context")
        .and_then(|context| context.get("prompt"))
        .cloned();
    if let Some(prompt) = prompt {
        let prompt_id = prompt.get("id").and_then(Value::as_str).unwrap_or_default();
        if catalogue_hash(prompt_id)? != prompt.get("definition_hash").cloned() {
            add!(
                prompt.get("id").cloned().unwrap_or(Value::Null),
                "registered prompt skill definition changed"
            );
        }
    }
    for step in run["steps"].as_array().cloned().unwrap_or_default() {
        if root.is_some() && step.get("state").and_then(Value::as_str) == Some("succeeded") {
            let outcome = if verify_evidence {
                evidence(
                    step.get("evidence").unwrap_or(&Value::Null),
                    root,
                    digests,
                    Some(deadline),
                    Some(max_bytes),
                )
                .and_then(|records| {
                    if records.is_empty() {
                        Err(value(
                            "successful workflow checkpoint has no retained evidence",
                        ))
                    } else {
                        Ok(())
                    }
                })
            } else {
                match root {
                    Some(root) => verify_receipt_metadata(root, &step),
                    None => Ok(()),
                }
            };
            if let Err(error) = outcome {
                add!(
                    step.get("id").cloned().unwrap_or(Value::Null),
                    format!("retained evidence unavailable: {error}")
                );
            }
        }
        let step_id = get(&step, "id")?;
        match step_id.as_str().and_then(|id| rows.get(id)) {
            None => add!(step_id.clone(), "registered checklist step was removed"),
            Some(row) if !falsy(row.get("stale").unwrap_or(&Value::Null)) => {
                add!(
                    step_id.clone(),
                    row.get("reason").cloned().unwrap_or(Value::Null)
                )
            }
            Some(_) => {
                if catalogue_hash(step_id.as_str().unwrap_or_default())?
                    != step
                        .get("definition_hash")
                        .filter(|hash| !hash.is_null())
                        .cloned()
                {
                    add!(step_id.clone(), "registered step definition changed");
                }
            }
        }
    }
    Ok(stale)
}

pub(super) fn reopen_index(run: &Value, stale: &[Value]) -> Option<usize> {
    if stale.is_empty() {
        return None;
    }
    let affected: BTreeSet<String> = stale
        .iter()
        .filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    let scenario_id = run["scenario"]["id"].as_str().unwrap_or_default();
    let prompt_id = run
        .get("context")
        .and_then(|c| c.get("prompt"))
        .filter(|p| !falsy(p))
        .and_then(|p| p.get("id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if affected.contains(scenario_id) || affected.contains(prompt_id) {
        return Some(0);
    }
    Some(
        run["steps"]
            .as_array()
            .map(|steps| {
                steps
                    .iter()
                    .position(|step| {
                        step.get("id")
                            .and_then(Value::as_str)
                            .is_some_and(|id| affected.contains(id))
                    })
                    .unwrap_or(0)
            })
            .unwrap_or(0),
    )
}

pub(super) fn refresh_current_definition_hashes(
    run: &mut Value,
    rows: &BTreeMap<String, Value>,
) -> ProjectsResult<()> {
    let scenario_id = run["scenario"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    if let Some(scenario) = rows.get(&scenario_id) {
        if falsy(scenario.get("stale").unwrap_or(&Value::Null)) {
            run["scenario"]["definition_hash"] =
                catalogue_hash(&scenario_id)?.unwrap_or(Value::Null);
        }
    }
    if let Some(prompt) = run
        .get_mut("context")
        .and_then(|c| c.get_mut("prompt"))
        .filter(|p| p.is_object())
    {
        let id = prompt["id"].as_str().unwrap_or_default().to_string();
        prompt["definition_hash"] = catalogue_hash(&id)?.unwrap_or(Value::Null);
    }
    if let Some(steps) = run["steps"].as_array_mut() {
        for step in steps {
            let id = step["id"].as_str().unwrap_or_default().to_string();
            if let Some(row) = rows.get(&id) {
                if falsy(row.get("stale").unwrap_or(&Value::Null)) {
                    step["definition_hash"] = catalogue_hash(&id)?.unwrap_or(Value::Null);
                }
            }
        }
    }
    Ok(())
}

pub(super) fn stale_checklist_dependency(run: &Value, rows: &BTreeMap<String, Value>) -> bool {
    let mut identities = vec![run["scenario"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string()];
    identities.extend(
        run["steps"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|step| step.get("id").and_then(Value::as_str).map(str::to_string)),
    );
    identities.iter().any(|id| {
        rows.get(id)
            .is_some_and(|row| !falsy(row.get("stale").unwrap_or(&Value::Null)))
    })
}

pub(super) fn public_run(
    run: &Value,
    rows: &BTreeMap<String, Value>,
    root: Option<&Path>,
    digests: &mut Digests,
    deadline: Option<f64>,
    max_bytes: Option<u64>,
    verify_evidence: bool,
) -> ProjectsResult<Value> {
    let mut item = run.clone();
    let stale = stale_dependencies(
        &item,
        rows,
        root,
        digests,
        deadline,
        max_bytes,
        verify_evidence,
    )?;
    item["stale_dependencies"] = json!(stale);
    item["evidence_verification_mode"] = json!(if verify_evidence {
        "bytes"
    } else {
        "metadata_only"
    });
    if let Some(steps) = item["steps"].as_array_mut() {
        for step in steps {
            if step.get("state").and_then(Value::as_str) == Some("succeeded") {
                let step_id = step.get("id").cloned().unwrap_or(Value::Null);
                let failure = stale
                    .iter()
                    .find(|row| row.get("id") == Some(&step_id))
                    .and_then(|row| row.get("reason").cloned());
                let failure_text = failure.as_ref().and_then(Value::as_str);
                let state = if failure_text.is_some_and(|f| f.contains("unverified:")) {
                    "unverified"
                } else if failure_text.is_some_and(|f| f.contains("budget")) {
                    "unavailable"
                } else if failure.is_some() && !falsy(failure.as_ref().unwrap()) {
                    "stale"
                } else if verify_evidence {
                    "verified"
                } else {
                    "metadata_matches"
                };
                let verified_at = if verify_evidence && failure.as_ref().is_none_or(falsy) {
                    Value::String(pytime::iso_now())
                } else {
                    step.get("evidence_verification")
                        .filter(|v| !falsy(v))
                        .and_then(|v| v.get("verified_at"))
                        .cloned()
                        .unwrap_or(Value::Null)
                };
                step["evidence_status"] = json!({
                    "state": state,
                    "verified_at": verified_at,
                    "observed_at": pytime::iso_now(),
                    "reason": failure.unwrap_or(Value::Null),
                });
            }
        }
    }
    let mut next_action = item["steps"]
        .as_array()
        .and_then(|steps| {
            steps
                .iter()
                .find(|step| step.get("state").and_then(Value::as_str) == Some("pending"))
                .and_then(|step| step.get("id").cloned())
        })
        .unwrap_or(Value::Null);
    if let Some(reopened) = reopen_index(&item, &stale) {
        item["recorded_state"] = item["state"].clone();
        item["state"] = json!("stale");
        next_action = item["steps"][reopened]["id"].clone();
    }
    item["next_action"] = next_action;
    Ok(item)
}

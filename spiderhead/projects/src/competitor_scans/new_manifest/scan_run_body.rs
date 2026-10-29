use super::*;

/// Body of `scan_task_competitors` under the durable run lock.
#[allow(clippy::too_many_arguments)]
pub(super) fn scan_run_body(
    root: &Path,
    project: &Value,
    lock_file: &std::fs::File,
    task_id: &str,
    expected_revision: i64,
    concurrency: i64,
    approve_large_crawl: bool,
    producer_build: Option<&str>,
    tools: &CompetitorTools,
    run_id: Option<&str>,
    target_rows: &[Value],
    plans: &[Value],
    budgets: &Value,
) -> ProjectsResult<Value> {
    let current = crate::coverage::coverage_status(root, true)?;
    if current["revision"].as_i64() != Some(expected_revision) {
        return Err(value(format!(
            "coverage revision conflict: current revision is {}",
            py_str(current.get("revision").unwrap_or(&Value::Null))
        )));
    }
    let mut resumed = false;
    let mut manifest = match run_id {
        Some(id) if manifest_path(root, id, false)?.exists() => {
            let manifest = read_manifest(
                root,
                id,
                project["project_uuid"].as_str().unwrap_or_default(),
            )?;
            if !manifest_matches_inputs(&manifest, task_id, plans) {
                return Err(competitor_error(
                    "run inputs changed since this run_id was created",
                ));
            }
            if !incomplete(&manifest) {
                return Ok(manifest_result(&manifest, true));
            }
            resumed = true;
            manifest
        }
        Some(id) => new_manifest(
            root,
            project,
            id,
            task_id,
            expected_revision,
            concurrency,
            approve_large_crawl,
            producer_build,
            plans,
            budgets,
        )?,
        None => {
            let all = manifests(root, project["project_uuid"].as_str().unwrap_or_default())?;
            let previous: Vec<&Value> = all
                .iter()
                .filter(|m| {
                    m["task_id"].as_str() == Some(task_id)
                        && incomplete(m)
                        && manifest_matches_inputs(m, task_id, plans)
                })
                .collect();
            let latest = previous.into_iter().reduce(|a, b| {
                if b["updated_at"].as_str() > a["updated_at"].as_str() {
                    b
                } else {
                    a
                }
            });
            if let Some(latest) = latest {
                resumed = true;
                (*latest).clone()
            } else {
                if all.len() >= MAX_RUN_MANIFESTS {
                    return Err(competitor_error(&format!(
                        "competitor scan history is limited to {MAX_RUN_MANIFESTS} runs"
                    )));
                }
                new_manifest(
                    root,
                    project,
                    &uuid::Uuid::new_v4().to_string(),
                    task_id,
                    expected_revision,
                    concurrency,
                    approve_large_crawl,
                    producer_build,
                    plans,
                    budgets,
                )?
            }
        }
    };
    if resumed
        && (manifest["approve_large_crawl"].as_bool() != Some(approve_large_crawl)
            || manifest
                .get("producer_build")
                .cloned()
                .unwrap_or(Value::Null)
                != producer_build.map(|p| json!(p)).unwrap_or(Value::Null))
    {
        return Err(competitor_error(
            "approval or producer settings changed since the run checkpoint",
        ));
    }
    manifest["concurrency"] = json!(concurrency);
    let current_plans: HashMap<(String, String), &Value> = plans
        .iter()
        .map(|p| {
            (
                (
                    p["project_uuid"].as_str().unwrap_or_default().to_string(),
                    p["url"].as_str().unwrap_or_default().to_string(),
                ),
                p,
            )
        })
        .collect();
    let sites_changed = manifest["sites"].as_array().into_iter().flatten().any(|s| {
        let key = (
            s["project_uuid"].as_str().unwrap_or_default().to_string(),
            s["url"].as_str().unwrap_or_default().to_string(),
        );
        match current_plans.get(&key) {
            Some(plan) => !site_inputs_match(s, plan),
            None => true,
        }
    });
    if sites_changed {
        return Err(competitor_error(
            "competitor scan site settings changed since the run checkpoint",
        ));
    }
    let run_id_value = manifest["run_id"].as_str().unwrap_or_default().to_string();
    let cancel = cancel_path(root, &run_id_value, true)?;
    manifest["state"] = json!("running");
    manifest["finished_at"] = Value::Null;
    if let Some(map) = manifest.as_object_mut() {
        map.remove("cancel_reason");
    }
    manifest["runner_token"] = json!(uuid::Uuid::new_v4().to_string().replace('-', ""));
    {
        let _guard = crate::runtime::project_write_lock(root, 5.0)?;
        set_run_lock_owner(
            lock_file,
            &run_id_value,
            manifest["runner_token"].as_str().unwrap_or_default(),
        )?;
    }
    publish_start(root, &manifest, &cancel)?;
    let site_count = manifest["sites"].as_array().map(|s| s.len()).unwrap_or(0);
    let shared = Mutex::new(manifest);
    let cancel_event = AtomicBool::new(false);
    let next_index = AtomicUsize::new(0);
    let (tx, rx) = channel::<(usize, SiteOutcome)>();
    let mut cancellation: Option<(&'static str, String)> = None;
    std::thread::scope(|scope| {
        for _ in 0..concurrency.max(1) {
            let tx = tx.clone();
            let shared = &shared;
            let cancel_event = &cancel_event;
            let next_index = &next_index;
            let current_plans = &current_plans;
            scope.spawn(move || loop {
                let index = next_index.fetch_add(1, Ordering::SeqCst);
                if index >= site_count {
                    break;
                }
                let outcome = execute_site(
                    index,
                    root,
                    target_rows,
                    current_plans,
                    shared,
                    cancel_event,
                    tools,
                );
                if let Err(outcome) = outcome {
                    let _ = tx.send((index, outcome));
                }
            });
        }
        drop(tx);
        for (index, outcome) in rx {
            match outcome {
                SiteOutcome::Interrupted(name, message) => {
                    cancellation = Some((name, message));
                    cancel_event.store(true, Ordering::SeqCst);
                    {
                        let _guard = match crate::runtime::project_write_lock(root, 5.0) {
                            Ok(guard) => guard,
                            Err(_) => break,
                        };
                        let _ = spiderhead_core::filesystem::atomic_write_bytes(
                            &cancel,
                            b"cancel requested\n",
                            None,
                            Some(0o600),
                            true,
                        );
                    }
                    break;
                }
                SiteOutcome::Failed(error) => {
                    let (name, message) = py_exception_name(&error);
                    let reason: String = format!("{name}: {message}").chars().take(1024).collect();
                    if let Ok(mut manifest) = shared.lock() {
                        manifest["sites"][index]["state"] = json!("failed");
                        manifest["sites"][index]["reason"] = json!(reason);
                        manifest["sites"][index]["finished_at"] = json!(zulu());
                        manifest["updated_at"] = json!(zulu());
                        let _ = write_manifest(root, &manifest);
                    }
                }
            }
        }
    });
    let mut manifest = shared.into_inner().unwrap_or_else(|e| e.into_inner());
    if let Some((name, message)) = cancellation {
        manifest["state"] = json!("interrupted");
        manifest["cancel_reason"] = json!(name);
        if let Some(sites) = manifest["sites"].as_array_mut() {
            for site in sites {
                if matches!(site["state"].as_str(), Some("pending" | "running")) {
                    site["state"] = json!("interrupted");
                    site["reason"] = json!(name);
                    site["finished_at"] = json!(zulu());
                }
            }
        }
        manifest["finished_at"] = json!(zulu());
        publish_final(root, &manifest, &cancel)?;
        return Err(py_named(name, message));
    }
    {
        let site_states: Vec<&str> = manifest["sites"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| s["state"].as_str().unwrap_or_default())
            .collect();
        manifest["state"] = if site_states
            .iter()
            .any(|s| matches!(*s, "pending" | "running" | "interrupted"))
        {
            json!("interrupted")
        } else if site_states.contains(&"cancelled") {
            json!("cancelled")
        } else if site_states.contains(&"failed") {
            json!("failed")
        } else if site_states.contains(&"partial") {
            json!("partial")
        } else {
            json!("completed")
        };
        manifest["finished_at"] = json!(zulu());
        publish_final(root, &manifest, &cancel)?;
    }
    Ok(manifest_result(&manifest, resumed))
}

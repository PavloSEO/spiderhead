use super::*;

pub(super) fn load(directory: &Path) -> ProjectsResult<(PathBuf, Value, Value)> {
    let (root, project, mut document) = load_document(directory, NAME, &|uuid| empty(uuid))?;
    normalize(&mut document);
    validate(
        &document,
        project["project_uuid"].as_str().unwrap_or_default(),
    )?;
    Ok((root, project, document))
}

pub(super) fn event(
    run: &mut Value,
    phase: &str,
    code: &str,
    message: Option<&str>,
) -> ProjectsResult<()> {
    if !PHASES.contains(&phase) {
        return Err(crate::error::value("run observation phase is invalid"));
    }
    let mut entry = json!({"at": pytime::zulu(), "phase": phase, "code": text(&json!(code), "event code", 64)?});
    if let Some(message) = message {
        entry["message"] = json!(text(&json!(message), "event message", MAX_EVENT_MESSAGE)?);
    }
    let events = run["events"].as_array_mut().unwrap();
    events.push(entry);
    let overflow = events.len().saturating_sub(MAX_EVENTS_PER_RUN);
    if overflow > 0 {
        events.drain(..overflow);
    }
    Ok(())
}

pub(super) fn find<'a>(document: &'a mut Value, run_id: &str) -> ProjectsResult<&'a mut Value> {
    text(&json!(run_id), "run id", 64)?;
    let found = document["runs"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|item| item["id"].as_str() == Some(run_id));
    found.ok_or_else(|| crate::error::value("run observation is not part of this project"))
}

pub(super) fn new_run_id(value: Option<&str>) -> ProjectsResult<String> {
    match value {
        None => Ok(Uuid::new_v4().simple().to_string()),
        Some("") => Err(crate::error::value("observer run ID must be a UUID")),
        Some(v) => Uuid::parse_str(v)
            .map(|u| u.to_string())
            .map_err(|_| crate::error::value("observer run ID must be a UUID")),
    }
}

pub(super) fn save(root: &Path, document: &mut Value) -> ProjectsResult<()> {
    save_revision(root, NAME, document)
}

pub(super) fn contention(e: &ProjectsError) -> bool {
    let msg = e.to_string();
    msg == "project document revision conflict" || msg == format!("another writer owns {NAME}")
}

/// A freshly loaded observer document awaiting its optimistic publish (`_Pending`).
pub struct Pending {
    /// Workspace root.
    pub root: PathBuf,
    /// The loaded, normalized, validated document.
    pub document: Value,
}

impl Pending {
    /// `_save`: publish against the loaded revision.
    pub fn save(&mut self) -> ProjectsResult<()> {
        save(&self.root, &mut self.document)
    }
}

/// `optimistic_write`: mutate and persist against concurrent writers.
///
/// Each attempt hands `apply` a freshly loaded document; contention re-runs the whole
/// mutation up to `WRITE_ATTEMPTS` times with the same backoff. `apply` returning `None`
/// ends the loop without writing.
pub fn optimistic_write<T>(
    directory: &Path,
    apply: impl Fn(&mut Pending) -> ProjectsResult<Option<T>>,
) -> ProjectsResult<Option<T>> {
    for attempt in 0..WRITE_ATTEMPTS {
        let (root, _project, document) = load(directory)?;
        let mut pending = Pending { root, document };
        match apply(&mut pending) {
            Ok(outcome) => return Ok(outcome),
            Err(e) if contention(&e) && attempt + 1 != WRITE_ATTEMPTS => {
                std::thread::sleep(std::time::Duration::from_secs_f64(
                    0.01 * (attempt as f64 + 1.0),
                ));
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!("optimistic_write loop bounds its attempts")
}

/// `_owner_busy`: a running record whose controller may still be alive blocks a new scan.
pub(super) fn owner_busy(run: &Value) -> bool {
    let state = runtime_state(
        run["controller_pid"].as_i64(),
        run["controller_start_identity"].as_str(),
        true,
    );
    matches!(state, "live" | "unknown")
}

/// `live_holder`: describe the running collector whose controller is still alive.
pub fn live_holder(directory: &Path) -> ProjectsResult<Option<String>> {
    let (_, _, document) = load(directory)?;
    for run in document["runs"].as_array().unwrap() {
        if run["state"].as_str() == Some("running") && owner_busy(run) {
            return Ok(Some(format!(
                "{} run {} (pid {})",
                run["kind"].as_str().unwrap_or_default(),
                run["id"].as_str().unwrap_or_default(),
                run["controller_pid"].as_i64().unwrap_or_default()
            )));
        }
    }
    Ok(None)
}

/// `start`: persist one project-bound local collector before it starts doing work.
#[allow(clippy::too_many_arguments)]
pub fn start(
    directory: &Path,
    kind: &str,
    mode: &str,
    max_urls: i64,
    max_requests: i64,
    max_crawl_seconds: i64,
    max_requests_per_second: Option<f64>,
    config_fingerprint: &str,
    artifact: Option<&str>,
    resumed: bool,
    origin: Option<&str>,
    aggregate_max_requests_per_second: Option<f64>,
    run_id: Option<&str>,
    counters: Option<&Map<String, Value>>,
) -> ProjectsResult<Value> {
    if !KINDS.contains(&kind) || !MODES.contains(&mode) {
        return Err(crate::error::value("run kind or collector mode is invalid"));
    }
    counter(&json!(max_urls), "max_urls")?;
    counter(&json!(max_requests), "max_requests")?;
    counter(&json!(max_crawl_seconds), "max_crawl_seconds")?;
    if !rate_ok(
        &max_requests_per_second
            .map(Value::from)
            .unwrap_or(Value::Null),
    ) {
        return Err(crate::error::value(
            "max_requests_per_second must be a finite nonnegative float",
        ));
    }
    if let Some(origin) = origin {
        if origin.is_empty() || origin.len() > 253 {
            return Err(crate::error::value(
                "origin must be bounded nonempty text or null",
            ));
        }
    }
    if !rate_ok(
        &aggregate_max_requests_per_second
            .map(Value::from)
            .unwrap_or(Value::Null),
    ) {
        return Err(crate::error::value(
            "aggregate maximum request rate must be finite and nonnegative",
        ));
    }
    let run_id = new_run_id(run_id)?;
    let started_at = pytime::zulu();
    let controller_pid = std::process::id() as i64;
    let controller_identity = process_identity(controller_pid);
    let counters = counters.cloned().unwrap_or_default();
    optimistic_write(directory, |pending| {
        let root = pending.root.clone();
        let document = &mut pending.document;
        if document["runs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"].as_str() == Some(run_id.as_str()))
        {
            return Err(crate::error::value(
                "observer run ID already exists in this project",
            ));
        }
        for item in document["runs"].as_array().unwrap() {
            if item["state"].as_str() == Some("running") && owner_busy(item) {
                return Err(crate::error::value(format!(
                    "project busy; owner={} pid={}",
                    item["id"].as_str().unwrap_or_default(),
                    item["controller_pid"].as_i64().unwrap_or_default()
                )));
            }
        }
        let mut counter_fields = Map::new();
        for name in ["fetched", "queued", "inflight", "excluded"] {
            counter_fields.insert(
                name.to_owned(),
                optional_counter(&counters.get(name).cloned().unwrap_or(Value::Null), name)?,
            );
        }
        counter_fields.insert(
            "rate_per_second".into(),
            counters
                .get("rate_per_second")
                .cloned()
                .unwrap_or(Value::Null),
        );
        let mut run = json!({
            "id": run_id,
            "kind": kind,
            "state": "running",
            "started_at": started_at,
            "finished_at": null,
            "pid": controller_pid,
            "controller_pid": controller_pid,
            "controller_start_identity": controller_identity,
            "collector_pid": null,
            "collector_start_identity": null,
            "collector_started_at": null,
            "collector": {
                "mode": mode,
                "max_urls": max_urls,
                "max_requests": max_requests,
                "max_crawl_seconds": max_crawl_seconds,
                "max_requests_per_second": max_requests_per_second,
                "config_fingerprint": text(&json!(config_fingerprint), "config fingerprint", 256)?,
                "resumed": resumed,
                "origin": origin,
                "aggregate_max_requests_per_second": aggregate_max_requests_per_second,
            },
            "artifact": relative_artifact(&root, artifact)?,
            "counters": Value::Object(counter_fields),
            "finish_reason": null,
            "events": [],
            "source_metadata": null,
            "telemetry": {
                "sampled_at": null,
                "rate_window_seconds": null,
                "queue_semantics": "unknown",
                "source": "collector",
            },
        });
        event(&mut run, "admission", "started", None)?;
        let runs = document["runs"].as_array_mut().unwrap();
        if runs.len() >= MAX_RUNS {
            let index = runs
                .iter()
                .position(|item| item["state"].as_str() != Some("running"));
            match index {
                Some(index) => {
                    runs.remove(index);
                }
                None => {
                    return Err(crate::error::value(
                        "run observation capacity reached; active runs cannot be evicted",
                    ))
                }
            }
        }
        runs.push(run.clone());
        pending.save()?;
        crate::journal::note(
            &root,
            "scans",
            "agent",
            &format!("run {run_id} started ({kind}/{mode})"),
        );
        let _ = crate::action_log::record_interface_action(
            &root,
            "run.start",
            "applied",
            &format!("Started {kind}/{mode} run"),
            None,
            None,
        );
        Ok(Some(run))
    })
    .map(|outcome| outcome.unwrap_or(Value::Null))
}

/// `phase`: record a phase transition on a running record.
pub fn phase(directory: &Path, run_id: &str, name: &str, code: &str) -> ProjectsResult<()> {
    optimistic_write(directory, |pending| {
        let run = find(&mut pending.document, run_id)?;
        if run["state"].as_str() != Some("running") {
            return Ok(None);
        }
        event(run, name, code, None)?;
        pending.save()?;
        Ok(Some(()))
    })?;
    Ok(())
}

/// `collector_started`: bind an SF child PID after spawn.
pub fn collector_started(directory: &Path, run_id: &str, pid: i64) -> ProjectsResult<()> {
    if pid <= 0 {
        return Err(crate::error::value(
            "collector PID must be a positive integer",
        ));
    }
    let identity = process_identity(pid);
    let started_at = pytime::zulu();
    optimistic_write(directory, |pending| {
        let run = find(&mut pending.document, run_id)?;
        if run["state"].as_str() != Some("running") {
            return Ok(None);
        }
        run["collector_pid"] = json!(pid);
        run["collector_start_identity"] = json!(identity);
        run["collector_started_at"] = json!(started_at);
        event(run, "collection", "collector_started", None)?;
        pending.save()?;
        Ok(Some(()))
    })?;
    Ok(())
}

/// `progress`: record measured collector counters; no totals are inferred.
#[allow(clippy::too_many_arguments)]
pub fn progress(
    directory: &Path,
    run_id: &str,
    fetched: Option<i64>,
    queued: Option<i64>,
    inflight: Option<i64>,
    excluded: Option<i64>,
    rate_per_second: Option<f64>,
    rate_window_seconds: Option<f64>,
    queue_semantics: &str,
    source: &str,
) -> ProjectsResult<()> {
    let values = json!({
        "fetched": optional_counter(&fetched.map(Value::from).unwrap_or(Value::Null), "fetched")?,
        "queued": optional_counter(&queued.map(Value::from).unwrap_or(Value::Null), "queued")?,
        "inflight": optional_counter(&inflight.map(Value::from).unwrap_or(Value::Null), "inflight")?,
        "excluded": optional_counter(&excluded.map(Value::from).unwrap_or(Value::Null), "excluded")?,
        "rate_per_second": rate_per_second,
    });
    if !rate_ok(&json!(rate_per_second)) {
        return Err(crate::error::value(
            "rate_per_second must be a finite nonnegative float",
        ));
    }
    optimistic_write(directory, |pending| {
        let document = &mut pending.document;
        let run = find(document, run_id)?;
        if run["state"].as_str() != Some("running") {
            return Ok(None);
        }
        let first_progress = !run["events"].as_array().unwrap().iter().any(|event| {
            event["phase"].as_str() == Some("collection")
                && event["code"].as_str() == Some("progress")
        });
        run["counters"] = values.clone();
        run["telemetry"] = json!({
            "sampled_at": pytime::zulu(),
            "rate_window_seconds": rate_window_seconds,
            "queue_semantics": queue_semantics,
            "source": source,
        });
        validate(
            document,
            document["project_uuid"].as_str().unwrap_or_default(),
        )?;
        let run = find(document, run_id)?;
        event(run, "collection", "progress", None)?;
        pending.save()?;
        if first_progress {
            let root = pending.root.clone();
            crate::journal::note(
                &root,
                "scans",
                "agent",
                &format!(
                    "run {run_id} progress: fetched={}, queued={}, inflight={}",
                    py_counter(&values["fetched"]),
                    py_counter(&values["queued"]),
                    py_counter(&values["inflight"]),
                ),
            );
            let _ = crate::action_log::record_interface_action(
                &root,
                "run.progress",
                "applied",
                &format!("Recorded first measured progress for run {run_id}"),
                None,
                None,
            );
        }
        Ok(Some(()))
    })?;
    Ok(())
}

/// `finish`: reach a terminal state with final counters and a bounded reason.
pub fn finish(
    directory: &Path,
    run_id: &str,
    state: &str,
    reason: &str,
    counters: Option<&Map<String, Value>>,
) -> ProjectsResult<()> {
    if !STATES.contains(&state) || state == "running" {
        return Err(crate::error::value("run terminal state is invalid"));
    }
    let normalized = counters
        .map(|counters| -> ProjectsResult<Value> {
            let mut out = Map::new();
            for name in ["fetched", "queued", "inflight", "excluded"] {
                out.insert(
                    name.to_owned(),
                    optional_counter(&counters.get(name).cloned().unwrap_or(Value::Null), name)?,
                );
            }
            out.insert(
                "rate_per_second".into(),
                counters
                    .get("rate_per_second")
                    .cloned()
                    .unwrap_or(Value::Null),
            );
            Ok(Value::Object(out))
        })
        .transpose()?;
    let finish_reason = text(&json!(reason), "finish reason", 500)?;
    let finished_at = pytime::zulu();
    optimistic_write(directory, |pending| {
        let root = pending.root.clone();
        let document = &mut pending.document;
        let run = find(document, run_id)?;
        if run["state"].as_str() != Some("running") {
            return Ok(None);
        }
        if let Some(normalized) = &normalized {
            run["counters"] = normalized.clone();
            run["telemetry"]["sampled_at"] = json!(finished_at);
        }
        run["state"] = json!(state);
        run["finish_reason"] = json!(finish_reason);
        run["finished_at"] = json!(finished_at);
        event(
            run,
            "finalizing",
            if state == "finished" {
                "finished"
            } else {
                state
            },
            None,
        )?;
        pending.save()?;
        let _ = crate::status_measurement::refresh_storage_measurement(&root);
        crate::journal::note(
            &root,
            "scans",
            "agent",
            &format!("crawl {run_id} {state}: {finish_reason}"),
        );
        let _ = crate::action_log::record_interface_action(
            &root,
            "run.finish",
            "applied",
            &format!("Run {run_id} reached {state}"),
            None,
            None,
        );
        Ok(Some(()))
    })?;
    Ok(())
}

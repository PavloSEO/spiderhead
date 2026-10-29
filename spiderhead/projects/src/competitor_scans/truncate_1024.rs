use super::*;

pub(super) fn truncate_1024(text: &str) -> String {
    text.chars().take(1024).collect()
}

/// Repr-style text for JSON-shaped values (strings, numbers, bools, lists, dicts).
pub(super) fn py_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_owned(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_owned(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => py_repr_str(s),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(py_repr).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(k, v)| format!("{}: {}", py_repr_str(k), py_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Quote text with `'`, switching to `"` when it contains `'` but not `"`.
pub(super) fn py_repr_str(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// The outcome `future.result()` can surface out of `execute` to the run loop.
pub(super) enum SiteOutcome {
    /// `KeyboardInterrupt`/`SystemExit`/`CancelledError`: class name and `str(exc)`.
    Interrupted(&'static str, String),
    /// An uncaught project error; the run loop records the site as failed.
    Failed(ProjectsError),
}

/// `Path.resolve(strict=False)`: canonicalize the deepest existing ancestor.
pub(crate) fn resolve_lenient(path: &Path) -> PathBuf {
    let mut cursor = path.to_path_buf();
    let mut missing = Vec::new();
    loop {
        match cursor.canonicalize() {
            Ok(resolved) => {
                let mut out = resolved;
                for name in missing.iter().rev() {
                    out.push(name);
                }
                return out;
            }
            Err(_) => {
                if let Some(name) = cursor.file_name() {
                    missing.push(name.to_os_string());
                    cursor.pop();
                } else {
                    return cursor;
                }
            }
        }
    }
}

/// Update one site record under the state lock, then publish the manifest.
pub(super) fn site_update(
    root: &Path,
    shared: &Mutex<Value>,
    index: usize,
    updates: &[(&str, Value)],
) {
    if let Ok(mut manifest) = shared.lock() {
        for (key, val) in updates {
            manifest["sites"][index][*key] = val.clone();
        }
        manifest["updated_at"] = json!(zulu());
        let _ = write_manifest(root, &manifest);
    }
}

/// `execute(site_index)`: run one site with its durable per-site checkpoint contract.
#[allow(clippy::too_many_arguments)]
pub(super) fn execute_site(
    index: usize,
    root: &Path,
    target_rows: &[Value],
    current_plans: &HashMap<(String, String), &Value>,
    shared: &Mutex<Value>,
    cancel_event: &AtomicBool,
    tools: &CompetitorTools,
) -> Result<(), SiteOutcome> {
    let (site, target, child_root, artifact_rel, artifact, run_id) = {
        let manifest = shared.lock().map_err(|_| {
            SiteOutcome::Failed(competitor_error("competitor scan state lock is poisoned"))
        })?;
        let site = manifest["sites"][index].clone();
        let target = resolve_site_target(target_rows, &site).map_err(SiteOutcome::Failed)?;
        let child_root = PathBuf::from(target["root"].as_str().unwrap_or_default());
        let artifact_rel = site["artifact"].as_str().unwrap_or_default().to_string();
        let artifact = child_root.join(&artifact_rel);
        let scans_root = resolve_lenient(&child_root.join("scans"));
        if artifact.is_symlink() || !resolve_lenient(&artifact).starts_with(&scans_root) {
            return Err(SiteOutcome::Failed(competitor_error(
                "competitor scan checkpoint path is unsafe",
            )));
        }
        let run_id = manifest["run_id"].as_str().unwrap_or_default().to_string();
        (
            site,
            target.clone(),
            child_root,
            artifact_rel,
            artifact,
            run_id,
        )
    };
    let recovering;
    {
        let manifest = shared.lock().map_err(|_| {
            SiteOutcome::Failed(competitor_error("competitor scan state lock is poisoned"))
        })?;
        let current_state = manifest["sites"][index]["state"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if matches!(
            current_state.as_str(),
            "completed" | "partial" | "failed" | "cancelled"
        ) {
            return Ok(());
        }
        if cancelled(root, &run_id, tools.cancel_requested).map_err(SiteOutcome::Failed)?
            || cancel_event.load(Ordering::SeqCst)
        {
            drop(manifest);
            site_update(
                root,
                shared,
                index,
                &[
                    ("state", json!("cancelled")),
                    ("reason", json!("cancelled before this site started")),
                    ("finished_at", json!(zulu())),
                ],
            );
            return Ok(());
        }
        recovering = matches!(current_state.as_str(), "running" | "interrupted");
        if recovering && !artifact.is_file() {
            drop(manifest);
            site_update(
                root,
                shared,
                index,
                &[
                    ("state", json!("failed")),
                    (
                        "reason",
                        json!("interrupted site has no retained scan artifact to resume"),
                    ),
                    ("finished_at", json!(zulu())),
                ],
            );
            return Ok(());
        }
        if !recovering && artifact.exists() {
            drop(manifest);
            site_update(
                root,
                shared,
                index,
                &[
                    ("state", json!("failed")),
                    (
                        "reason",
                        json!("scan artifact exists without a started checkpoint; refusing to overwrite"),
                    ),
                    ("finished_at", json!(zulu())),
                ],
            );
            return Ok(());
        }
        let started_at = manifest["sites"][index]
            .get("started_at")
            .cloned()
            .unwrap_or(Value::Null);
        let started_at = if falsy(&started_at) {
            json!(zulu())
        } else {
            started_at
        };
        drop(manifest);
        site_update(
            root,
            shared,
            index,
            &[
                ("state", json!("running")),
                ("started_at", started_at),
                ("reason", Value::Null),
            ],
        );
    }
    let kwargs = if recovering {
        match spiderhead_store::history::read_scan(&artifact) {
            Err(e) => {
                let (name, _) = py_exception_name(&ProjectsError::Store(e));
                site_update(
                    root,
                    shared,
                    index,
                    &[
                        ("state", json!("failed")),
                        (
                            "reason",
                            json!(format!("interrupted scan is unreadable: {name}")),
                        ),
                        ("finished_at", json!(zulu())),
                    ],
                );
                return Ok(());
            }
            Ok(record) => {
                let record = Value::Object(record);
                let partial = !falsy(&record["crawl_partial"]) || !falsy(&record["corpus_partial"]);
                if !partial {
                    let scan_posix = format!(
                        "{}/{}",
                        site["directory"].as_str().unwrap_or_default(),
                        artifact_rel
                    );
                    site_update(
                        root,
                        shared,
                        index,
                        &[
                            ("state", json!("completed")),
                            ("scan", json!(scan_posix)),
                            ("scan_uuid", record["scan_uuid"].clone()),
                            ("partial", json!(false)),
                            ("finish_reason", record["finish_reason"].clone()),
                            ("finished_at", json!(zulu())),
                            ("reason", Value::Null),
                        ],
                    );
                    return Ok(());
                }
                let manifest = shared.lock().map_err(|_| {
                    SiteOutcome::Failed(competitor_error("competitor scan state lock is poisoned"))
                })?;
                let producer = manifest
                    .get("producer_build")
                    .cloned()
                    .ok_or_else(|| SiteOutcome::Failed(key_error("producer_build")))?;
                json!({
                    "project": child_root.to_string_lossy(),
                    "resume": artifact.to_string_lossy(),
                    "approve_large_crawl": manifest["approve_large_crawl"],
                    "producer_build": producer,
                })
            }
        }
    } else {
        let key = (
            site["project_uuid"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            site["url"].as_str().unwrap_or_default().to_string(),
        );
        let Some(plan) = current_plans.get(&key) else {
            return Err(SiteOutcome::Failed(competitor_error(
                "competitor workspace changed since the run checkpoint",
            )));
        };
        let manifest = shared.lock().map_err(|_| {
            SiteOutcome::Failed(competitor_error("competitor scan state lock is poisoned"))
        })?;
        let producer = manifest
            .get("producer_build")
            .cloned()
            .ok_or_else(|| SiteOutcome::Failed(key_error("producer_build")))?;
        let mut kwargs = request(
            &target,
            plan,
            &artifact,
            manifest["approve_large_crawl"].as_bool().unwrap_or(false),
        );
        kwargs["producer_build"] = producer;
        kwargs
    };
    let site_url = site["url"].as_str().unwrap_or_default().to_string();
    let last_write = Mutex::new(0.0f64);
    let cancel_requested = tools.cancel_requested;
    let progress_cb = tools.progress;
    let run_id_progress = run_id.clone();
    let on_progress = move |fetched: i64, queued: i64| -> Result<(), (&'static str, String)> {
        {
            if let Ok(mut manifest) = shared.lock() {
                manifest["sites"][index]["fetched"] = json!(fetched.max(0));
                manifest["sites"][index]["queued"] = json!(queued.max(0));
                let checkpoint = monotonic();
                let mut last = last_write.lock().unwrap_or_else(|e| e.into_inner());
                if checkpoint - *last >= 5.0 {
                    *last = checkpoint;
                    manifest["updated_at"] = json!(zulu());
                    let _ = write_manifest(root, &manifest);
                }
            }
        }
        if cancel_event.load(Ordering::SeqCst)
            || cancelled(root, &run_id_progress, cancel_requested).unwrap_or(false)
        {
            return Err(("KeyboardInterrupt", "competitor scan cancelled".to_string()));
        }
        if let Some(progress) = progress_cb {
            progress(&site_url, fetched, queued);
        }
        Ok(())
    };
    let kwargs_map = kwargs.as_object().cloned().unwrap_or_default();
    let result = match (tools.crawl_site)(&kwargs_map, &on_progress) {
        Err(CrawlFault::Interrupted(name, message)) => {
            let scan = if artifact.is_file() {
                json!(format!(
                    "{}/{}",
                    site["directory"].as_str().unwrap_or_default(),
                    artifact_rel
                ))
            } else {
                Value::Null
            };
            site_update(
                root,
                shared,
                index,
                &[
                    ("state", json!("interrupted")),
                    ("reason", json!(name)),
                    ("finished_at", json!(zulu())),
                    ("scan", scan),
                ],
            );
            return Err(SiteOutcome::Interrupted(name, message));
        }
        Err(CrawlFault::Failed(message)) => {
            site_update(
                root,
                shared,
                index,
                &[
                    ("state", json!("failed")),
                    ("reason", json!(truncate_1024(&message))),
                    ("finished_at", json!(zulu())),
                ],
            );
            return Ok(());
        }
        Ok(result) => result,
    };
    if !result.is_object() || result.get("ok").is_some_and(|ok| ok == &Value::Bool(false)) {
        let reason = if result.is_object() {
            let default_error = json!("crawl failed");
            let error = result.get("error").unwrap_or(&default_error);
            if error.is_string() {
                py_str(error)
            } else {
                py_repr(error)
            }
        } else {
            "invalid crawl result".to_string()
        };
        site_update(
            root,
            shared,
            index,
            &[
                ("state", json!("failed")),
                ("reason", json!(truncate_1024(&reason))),
                ("finished_at", json!(zulu())),
            ],
        );
        return Ok(());
    }
    let scan_value = {
        let scan = result.get("scan").cloned().unwrap_or(Value::Null);
        let scan_path = result.get("scan_path").cloned().unwrap_or(Value::Null);
        if !falsy(&scan) {
            scan
        } else if !falsy(&scan_path) {
            scan_path
        } else {
            json!(artifact.to_string_lossy())
        }
    };
    let (scan_rel, scan_record) =
        match scan_reference(root, &child_root, &scan_value).and_then(|(rel, path)| {
            spiderhead_store::history::read_scan(&path)
                .map(|record| (rel, Value::Object(record)))
                .map_err(ProjectsError::Store)
        }) {
            Ok(pair) => pair,
            Err(e) => {
                let (_, message) = py_exception_name(&e);
                site_update(
                    root,
                    shared,
                    index,
                    &[
                        ("state", json!("failed")),
                        ("reason", json!(truncate_1024(&message))),
                        ("finished_at", json!(zulu())),
                    ],
                );
                return Ok(());
            }
        };
    let partial = !falsy(&scan_record["crawl_partial"]) || !falsy(&scan_record["corpus_partial"]);
    let cancelled_now = cancelled(root, &run_id, tools.cancel_requested).unwrap_or(false)
        || cancel_event.load(Ordering::SeqCst);
    let state = if cancelled_now && partial {
        "interrupted"
    } else if partial {
        "partial"
    } else {
        "completed"
    };
    site_update(
        root,
        shared,
        index,
        &[
            ("state", json!(state)),
            ("scan", json!(scan_rel)),
            ("scan_uuid", scan_record["scan_uuid"].clone()),
            ("partial", json!(partial)),
            ("finish_reason", scan_record["finish_reason"].clone()),
            (
                "reason",
                if state == "interrupted" {
                    json!("cancelled")
                } else {
                    Value::Null
                },
            ),
            ("finished_at", json!(zulu())),
        ],
    );
    Ok(())
}

/// `get_competitor_scan_run`: durable per-site state of one run.
pub fn get_competitor_scan_run(directory: &Path, run_id: &str) -> ProjectsResult<Value> {
    let (root, project) = crate::workspace::load(directory)?;
    let (manifest, matches_owner) = {
        let _guard = crate::runtime::project_write_lock(&root, 5.0)?;
        let manifest = read_manifest(
            &root,
            run_id,
            project["project_uuid"].as_str().unwrap_or_default(),
        )?;
        let (active, owner) = run_lock_owner_locked(&root)?;
        let owner_matches = manifest["state"].as_str() == Some("running")
            && active
            && owner.as_ref().is_some_and(|o| {
                o["run_id"].as_str() == manifest["run_id"].as_str()
                    && manifest["runner_token"].as_str() == o["runner_token"].as_str()
            });
        (manifest, owner_matches)
    };
    let marker = cancel_path(&root, run_id, false)?;
    if marker.is_symlink() {
        return Err(competitor_error(
            "competitor scan cancellation marker is unsafe",
        ));
    }
    let requested = marker.is_file() && matches_owner;
    let mut results = manifest_result(&manifest, false)["results"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if manifest["state"].as_str() == Some("running") && !matches_owner {
        for item in &mut results {
            if item["state"].as_str() == Some("running") {
                item["state"] = json!("interrupted");
                item["reason"] = json!("this run no longer owns the project lock");
            }
        }
    }
    let state = manifest["state"].as_str().unwrap_or_default();
    Ok(json!({
        "ok": true,
        "run_id": manifest["run_id"],
        "state": if requested && state == "running" && matches_owner {
            "cancel_requested"
        } else if state == "running" && !matches_owner {
            "interrupted"
        } else {
            state
        },
        "cancel_requested": requested,
        "runner_active": matches_owner,
        "task_id": manifest["task_id"],
        "coverage_revision": manifest["coverage_revision"],
        "updated_at": manifest["updated_at"],
        "budgets": manifest["budgets"],
        "results": results,
    }))
}

/// `cancel_competitor_scan_run`: ask the active runner to stop at its next boundary.
pub fn cancel_competitor_scan_run(directory: &Path, run_id: &str) -> ProjectsResult<Value> {
    let (root, project) = crate::workspace::load(directory)?;
    manifest_path(&root, run_id, false)?;
    {
        let _guard = crate::runtime::project_write_lock(&root, 5.0)?;
        let manifest = read_manifest(
            &root,
            run_id,
            project["project_uuid"].as_str().unwrap_or_default(),
        )?;
        let (active, owner) = run_lock_owner_locked(&root)?;
        let matches_owner = manifest["state"].as_str() == Some("running")
            && active
            && owner.as_ref().is_some_and(|o| {
                o["run_id"].as_str() == manifest["run_id"].as_str()
                    && manifest["runner_token"].as_str() == o["runner_token"].as_str()
            });
        if manifest["state"].as_str() != Some("running") || !matches_owner {
            return Ok(json!({
                "ok": true,
                "run_id": manifest["run_id"],
                "state": if manifest["state"].as_str() == Some("running") {
                    "interrupted"
                } else {
                    manifest["state"].as_str().unwrap_or_default()
                },
                "cancelled": false,
            }));
        }
        let marker = cancel_path(&root, run_id, false)?;
        if marker.is_symlink() {
            return Err(competitor_error(
                "competitor scan cancellation marker is unsafe",
            ));
        }
        spiderhead_core::filesystem::atomic_write_bytes(
            &marker,
            b"cancel requested\n",
            None,
            Some(0o600),
            true,
        )
        .map_err(ProjectsError::Io)?;
    }
    let normalized = uuid::Uuid::parse_str(run_id)
        .map(|u| u.to_string())
        .map_err(|_| competitor_error("run_id must be a UUID"))?;
    Ok(json!({
        "ok": true,
        "run_id": normalized,
        "state": "cancel_requested",
        "cancelled": true,
    }))
}

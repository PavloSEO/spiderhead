use super::*;

/// `_validate_export`: one retained history item must satisfy the export schema.
pub(super) fn validate_export(item: &serde_json::Value) -> ProjectsResult<()> {
    let valid_id = item["id"].as_str().is_some_and(|id| {
        uuid::Uuid::parse_str(id)
            .map(|u| u.to_string() == id)
            .unwrap_or(false)
    });
    let valid_job_id = item.get("job_id").is_none_or(|j| {
        j.is_null()
            || j.as_str().is_some_and(|j| {
                uuid::Uuid::parse_str(j)
                    .map(|u| u.to_string() == j)
                    .unwrap_or(false)
            })
    });
    let sha_ok = match item.get("sha256") {
        None | Some(serde_json::Value::Null) => true,
        Some(v) => v.as_str().is_some_and(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }),
    };
    if !valid_id
        || !item.get("recorded_at").is_some_and(|v| v.is_string())
        || !matches!(
            item.get("dataset").and_then(|v| v.as_str()),
            Some("audit" | "tasks" | "task_comments" | "task_history" | "compare")
        )
        || !matches!(
            item.get("format").and_then(|v| v.as_str()),
            Some("json" | "csv" | "xlsx" | "docx" | "md" | "pdf")
        )
        || !item.get("path").is_some_and(|v| v.is_string())
        || !item["row_count"].is_i64() && !item["row_count"].is_u64()
        || item["row_count"].as_i64().is_some_and(|n| n < 0)
        || !item["byte_count"].is_i64() && !item["byte_count"].is_u64()
        || item["byte_count"].as_i64().is_some_and(|n| n < 0)
        || !item.get("source").is_some_and(|v| v.is_object())
        || !sha_ok
        || !valid_job_id
    {
        return Err(export_error("invalid export history item"));
    }
    Ok(())
}

/// `exports_page`: newest-first page of retained export history.
pub fn exports_page(
    directory: &Path,
    offset: i64,
    limit: i64,
) -> ProjectsResult<serde_json::Value> {
    if offset < 0 || !(1..=100).contains(&limit) {
        return Err(export_error(
            "offset must be nonnegative and limit must be 1..100",
        ));
    }
    let (root, project) = crate::workspace::load(directory)?;
    let document = crate::runtime::read_document(&root, HISTORY_NAME)?;
    let (items, revision): (Vec<serde_json::Value>, i64) = match document {
        None => (Vec::new(), 0),
        Some(document) => {
            if document.get("format").and_then(|v| v.as_str()) != Some(HISTORY_FORMAT)
                || document.get("project_uuid") != Some(&project["project_uuid"])
            {
                return Err(export_error(
                    "export history belongs to another project or format",
                ));
            }
            let items = document.get("items").cloned().unwrap_or_else(|| json!([]));
            let revision = document
                .get("revision")
                .cloned()
                .unwrap_or(serde_json::json!(0));
            if !items.is_array()
                || !revision.is_i64() && !revision.is_u64()
                || revision.as_i64().is_some_and(|r| r < 0)
                || items
                    .as_array()
                    .is_some_and(|list| list.iter().any(|i| !i.is_object()))
            {
                return Err(export_error("export history is malformed"));
            }
            if let Some(list) = items.as_array() {
                for item in list {
                    validate_export(item)?;
                }
            }
            (
                items.as_array().cloned().unwrap_or_default(),
                revision.as_i64().unwrap_or(0),
            )
        }
    };
    let total = items.len() as i64;
    let page: Vec<serde_json::Value> = items
        .iter()
        .rev()
        .skip(offset.max(0) as usize)
        .take(limit as usize)
        .cloned()
        .collect();
    Ok(serde_json::json!({
        "ok": true,
        "revision": revision,
        "items": page,
        "pagination": {
            "total": total,
            "offset": offset,
            "limit": limit,
            "next_offset": if offset + limit < total { serde_json::json!(offset + limit) } else { serde_json::Value::Null },
            "previous_offset": if offset != 0 { serde_json::json!(0.max(offset - limit)) } else { serde_json::Value::Null },
        },
    }))
}

/// `_run_export`: the worker body — prepare, write, publish, then finalize the record.
pub(super) fn run_export(
    root: PathBuf,
    directory: PathBuf,
    mut job: Value,
    lock: Option<std::fs::File>,
    prepared: Option<std::sync::mpsc::Sender<Value>>,
) {
    job["state"] = json!("running");
    job["phase"] = json!("preparing");
    job["started_at"] = json!(zulu());
    job["updated_at"] = json!(zulu());
    let _ = write_job(&root, &job);
    if let Some(tx) = prepared {
        let _ = tx.send(job.clone());
    }
    let outcome = (|| -> ProjectsResult<()> {
        let (rows, total, source) = rows_for(
            &directory,
            job["dataset"].as_str().unwrap_or_default(),
            job["compare_manifest"].as_str(),
        )?;
        job["total_rows"] = json!(total);
        job["source"] = source;
        write_job(&root, &job)?;
        let columns: &[&str] = match job["dataset"].as_str().unwrap_or_default() {
            "tasks" => &TASK_COLUMNS,
            "task_comments" => &TASK_COMMENT_COLUMNS,
            "task_history" => &TASK_HISTORY_COLUMNS,
            _ => &COMPARE_COLUMNS,
        };
        write_rows(&root, &mut job, &rows, total, columns)?;
        record_published_export(&root, &directory, &mut job)?;
        Ok(())
    })();
    match outcome {
        Ok(()) => {}
        Err(e) if is_cancelled(&e) => {
            job["state"] = json!("cancelled");
            job["phase"] = json!("complete");
            job["result"] = Value::Null;
            job["history_state"] = json!("not_recorded");
            let (_, message) = py_exception_name(&e);
            job["reason"] = json!(message);
            job["finished_at"] = json!(zulu());
        }
        Err(e) => {
            let published = artifact_paths(&root, &job)
                .map(|(target, _)| matches_artifact(&target, &job["result"]))
                .unwrap_or(false);
            let (name, message) = py_exception_name(&e);
            if published {
                job["state"] = json!("completed");
                job["phase"] = json!("complete");
                job["history_state"] = json!("failed");
                job["history_reason"] = json!(format!(
                    "history receipt could not be finalized ({name}); retry this job_id"
                ));
                job["finished_at"] = json!(zulu());
            } else {
                job["state"] = json!("failed");
                job["phase"] = json!("complete");
                job["result"] = Value::Null;
                job["history_state"] = json!("not_recorded");
                job["reason"] = json!(format!("{name}: {message}"));
                job["finished_at"] = json!(zulu());
            }
        }
    }
    let _ = write_job(&root, &job);
    if let Ok(cancel) = job_path(&root, job["job_id"].as_str().unwrap_or_default(), false) {
        let _ = std::fs::remove_file(cancel.with_extension("cancel"));
    }
    if let Ok(mut active) = ACTIVE.lock() {
        active.remove(job["job_id"].as_str().unwrap_or_default());
    }
    release_job_lock(lock);
}

/// `_resume_existing_export`: return a terminal job, finish a publish, or reset partial work.
pub(super) fn resume_existing_export(
    root: &Path,
    directory: &Path,
    job: &mut Value,
) -> ProjectsResult<Option<Value>> {
    if job["state"].as_str() == Some("cancelled") {
        return Ok(Some(job.clone()));
    }
    let (target, temporary) = artifact_paths(root, job)?;
    if job["state"].as_str() == Some("completed") {
        if !matches_artifact(&target, &job["result"]) {
            return Err(export_error(
                "completed export is missing or no longer matches its recorded checksum",
            ));
        }
        if job["history_state"].as_str() == Some("failed") {
            return record_published_export(root, directory, job).map(Some);
        }
        return Ok(Some(job.clone()));
    }
    let mut result = job.get("result").cloned().unwrap_or(Value::Null);
    if result.is_object() {
        let (target_exists, target_symlink) = path_flags(&target);
        if target_exists || target_symlink {
            if !matches_artifact(&target, &result) {
                return Err(export_error(
                    "export destination exists but does not match this job",
                ));
            }
        } else if matches_artifact(&temporary, &result) {
            std::fs::hard_link(&temporary, &target).map_err(ProjectsError::Io)?;
            std::fs::remove_file(&temporary).map_err(ProjectsError::Io)?;
        } else {
            result = Value::Null;
            if let Some(map) = job.as_object_mut() {
                map.remove("result");
            }
        }
        if result.is_object() {
            return record_published_export(root, directory, job).map(Some);
        }
    }
    let (target_exists, target_symlink) = path_flags(&target);
    if target_exists || target_symlink {
        return Err(export_error(
            "export destination exists without a verifiable job receipt",
        ));
    }
    if temporary.is_symlink() {
        return Err(export_error("unsafe export temporary path"));
    }
    if temporary.exists() {
        if !temporary.is_file() {
            return Err(export_error("export temporary path is not a regular file"));
        }
        std::fs::remove_file(&temporary).map_err(ProjectsError::Io)?;
    }
    let cancel =
        job_path(root, job["job_id"].as_str().unwrap_or_default(), false)?.with_extension("cancel");
    if cancel.is_symlink() {
        return Err(export_error("unsafe export cancellation marker"));
    }
    match std::fs::remove_file(&cancel) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(ProjectsError::Io(e)),
    }
    if let Some(map) = job.as_object_mut() {
        for key in [
            "result",
            "source",
            "started_at",
            "finished_at",
            "reason",
            "history_reason",
            "export",
        ] {
            map.remove(key);
        }
    }
    job["state"] = json!("queued");
    job["phase"] = json!("queued");
    job["processed_rows"] = json!(0);
    job["total_rows"] = Value::Null;
    job["history_state"] = json!("not_recorded");
    job["updated_at"] = json!(zulu());
    write_job(root, job)?;
    Ok(None)
}

/// `_recoverable`: can this job be retried without losing verifiable work?
pub(super) fn recoverable(root: &Path, job: &Value) -> bool {
    let Ok((target, temporary)) = artifact_paths(root, job) else {
        return false;
    };
    let result = job.get("result").cloned().unwrap_or(Value::Null);
    if result.is_object() {
        return matches_artifact(&target, &result) || matches_artifact(&temporary, &result);
    }
    let (target_exists, target_symlink) = path_flags(&target);
    let (temp_exists, temp_symlink) = path_flags(&temporary);
    !target_exists && !target_symlink && (!temp_exists || (temporary.is_file() && !temp_symlink))
}

/// `start_export`: run or start a local task/compare export; no network request is made.
pub fn start_export(
    directory: &Path,
    dataset: &str,
    fmt: &str,
    output: &str,
    compare_manifest: Option<&str>,
    job_id: Option<&str>,
    wait: bool,
) -> ProjectsResult<Value> {
    let (root, project) = crate::workspace::load(directory)?;
    if !matches!(
        dataset,
        "tasks" | "task_comments" | "task_history" | "compare"
    ) {
        return Err(export_error(
            "dataset must be tasks, task_comments, task_history, or compare",
        ));
    }
    if !matches!(fmt, "json" | "csv" | "xlsx") {
        return Err(export_error("format must be json, csv, or xlsx"));
    }
    if (dataset == "compare") != compare_manifest.is_some() {
        return Err(export_error(
            "compare_manifest is required only for compare exports",
        ));
    }
    let bare_name_error = || {
        export_error(
            "output must be a new filename directly under reports/ (a bare name), or a path inside <project>/reports/",
        )
    };
    let mut output = output.to_string();
    if output.trim_matches('/').replace('\\', "/").contains('/') {
        let candidate = Path::new(output.trim());
        let parts: Vec<String> = candidate
            .components()
            .filter_map(|c| match c {
                Component::Normal(part) => Some(part.to_string_lossy().to_string()),
                _ => None,
            })
            .collect();
        let Some(index) = parts.iter().position(|p| p == "reports") else {
            return Err(bare_name_error());
        };
        let rest = &parts[index + 1..];
        if rest.len() != 1 {
            return Err(export_error(
                "output must be a new filename directly under reports/",
            ));
        }
        output = rest[0].clone();
    }
    if py_path_name(&output) != output || output.starts_with('.') {
        return Err(bare_name_error());
    }
    let suffix = match fmt {
        "json" => ".json",
        "csv" => ".csv",
        _ => ".xlsx",
    };
    if py_suffix(&output).is_empty() {
        output = format!("{output}{suffix}");
    }
    if casefold(&py_suffix(&output)) != suffix {
        return Err(export_error("output extension must match format"));
    }
    let identity = match job_id {
        Some(job_id) => uuid::Uuid::parse_str(job_id)
            .map(|u| u.to_string())
            .map_err(|_| py_named("ValueError", "badly formed hexadecimal UUID string"))?,
        None => uuid::Uuid::new_v4().to_string(),
    };
    let job;
    let mut lock_file;
    {
        let _guard = crate::runtime::project_write_lock(&root, 5.0)?;
        let (current_root, current_project) = crate::workspace::load(directory)?;
        if current_root != root || current_project["project_uuid"] != project["project_uuid"] {
            return Err(export_error("project changed before export admission"));
        }
        let path = job_path(&root, &identity, true)?;
        let (descriptor, busy) = acquire_job_lock(&root, &identity, true)?;
        lock_file = descriptor;
        if busy || lock_file.is_none() {
            return Err(export_error("export job is already running"));
        }
        let admission = (|| -> ProjectsResult<Option<Value>> {
            if lexists(&path) {
                let existing = read_job(&root, &identity)?;
                if existing.get("project_uuid") != Some(&project["project_uuid"])
                    || existing["dataset"].as_str() != Some(dataset)
                    || existing["format"].as_str() != Some(fmt)
                    || existing["output"].as_str() != Some(output.as_str())
                    || existing
                        .get("compare_manifest")
                        .cloned()
                        .unwrap_or(Value::Null)
                        != compare_manifest.map(|m| json!(m)).unwrap_or(Value::Null)
                {
                    return Err(export_error("job_id belongs to a different export request"));
                }
                let mut existing = existing;
                return resume_existing_export(&root, &root, &mut existing).map(|r| {
                    r.map(|done| {
                        let mut done = done;
                        done["ok"] = json!(done["state"].as_str() == Some("completed"));
                        done
                    })
                });
            }
            let fresh = json!({
                "record_format": "spiderhead.project-export-job.v1",
                "job_id": identity,
                "project_uuid": project["project_uuid"],
                "dataset": dataset,
                "format": fmt,
                "output": output,
                "compare_manifest": compare_manifest,
                "state": "queued",
                "phase": "queued",
                "processed_rows": 0,
                "total_rows": Value::Null,
                "history_state": "not_recorded",
                "created_at": zulu(),
                "updated_at": zulu(),
            });
            write_job(&root, &fresh)?;
            Ok(None)
        })();
        match admission {
            Err(e) => {
                release_job_lock(lock_file.take());
                return Err(e);
            }
            Ok(Some(done)) => {
                release_job_lock(lock_file.take());
                return Ok(done);
            }
            Ok(None) => {}
        }
        job = if lexists(&path) {
            read_job(&root, &identity)?
        } else {
            return Err(export_error("export job is unavailable"));
        };
    }
    if wait {
        if let Ok(mut active) = ACTIVE.lock() {
            active.insert(identity.clone());
        }
        run_export(
            root.clone(),
            root.clone(),
            job.clone(),
            lock_file.take(),
            None,
        );
        let mut finished = job;
        if let Ok(current) = read_job(&root, &identity) {
            finished = current;
        }
        let completed = finished["state"].as_str() == Some("completed");
        finished["ok"] = json!(completed);
        return Ok(finished);
    }
    let (tx, rx) = std::sync::mpsc::channel::<Value>();
    let thread_root = root.clone();
    let thread_directory = root.clone();
    let thread_job = job.clone();
    let thread_lock = lock_file.take();
    // On a failed spawn the moved `File` is dropped, closing the descriptor, which is
    // exactly what `_release_job_lock` does after `flock LOCK_UN`.
    match std::thread::Builder::new().spawn(move || {
        run_export(
            thread_root,
            thread_directory,
            thread_job,
            thread_lock,
            Some(tx),
        );
    }) {
        Ok(_) => {
            if let Ok(mut active) = ACTIVE.lock() {
                active.insert(identity.clone());
            }
            let snapshot = rx.recv().unwrap_or_else(|_| {
                let mut queued = job.clone();
                queued["state"] = json!("queued");
                queued["phase"] = json!("queued");
                queued
            });
            let mut returned = snapshot;
            returned["ok"] = json!(true);
            Ok(returned)
        }
        Err(e) => Err(ProjectsError::Io(e)),
    }
}

/// `export_status`: inspect one job's record and liveness.
pub fn export_status(directory: &Path, job_id: &str) -> ProjectsResult<Value> {
    let (root, project) = crate::workspace::load(directory)?;
    let job_id = uuid::Uuid::parse_str(job_id)
        .map(|u| u.to_string())
        .map_err(|_| py_named("ValueError", "badly formed hexadecimal UUID string"))?;
    let job = read_job(&root, &job_id)?;
    if job.get("project_uuid") != Some(&project["project_uuid"]) {
        return Err(export_error("export job belongs to another project"));
    }
    let mut active = ACTIVE
        .lock()
        .map(|active| active.contains(&job_id))
        .unwrap_or(false);
    if !active {
        let (descriptor, busy) = acquire_job_lock(&root, &job_id, false)?;
        active = busy;
        release_job_lock(descriptor);
    }
    let state = job["state"].as_str().unwrap_or_default();
    if (state == "queued" || state == "running") && !active {
        let mut out = job.clone();
        out["ok"] = json!(true);
        out["state"] = json!("interrupted");
        out["recoverable"] = json!(recoverable(&root, &job));
        return Ok(out);
    }
    let mut out = job;
    out["ok"] = json!(true);
    let rec = out["history_state"].as_str() == Some("failed") && recoverable(&root, &out);
    out["recoverable"] = json!(rec);
    Ok(out)
}

/// `cancel_export`: leave a `.cancel` marker a live worker observes.
pub fn cancel_export(directory: &Path, job_id: &str) -> ProjectsResult<Value> {
    let (root, project) = crate::workspace::load(directory)?;
    let job_id = uuid::Uuid::parse_str(job_id)
        .map(|u| u.to_string())
        .map_err(|_| py_named("ValueError", "badly formed hexadecimal UUID string"))?;
    let job = read_job(&root, &job_id)?;
    if job.get("project_uuid") != Some(&project["project_uuid"]) {
        return Err(export_error("export job belongs to another project"));
    }
    let state = job["state"].as_str().unwrap_or_default();
    if state != "queued" && state != "running" {
        return Ok(json!({
            "ok": true,
            "state": state,
            "cancelled": false,
        }));
    }
    let cancel = job_path(&root, &job_id, false)?.with_extension("cancel");
    if cancel.is_symlink() {
        return Err(export_error("unsafe export cancellation marker"));
    }
    // `Path.touch(exist_ok=True)`: create or update mtime, never truncate.
    std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .private_mode(0o600)
        .open(&cancel)
        .map_err(ProjectsError::Io)?;
    Ok(json!({
        "ok": true,
        "state": "cancelling",
        "cancelled": true,
        "job_id": job_id,
    }))
}

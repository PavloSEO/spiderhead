use super::*;

mod scan_run_body;
use scan_run_body::*;

/// `_new_manifest`: a fresh running manifest with pending site records.
#[allow(clippy::too_many_arguments)]
pub(super) fn new_manifest(
    root: &Path,
    project: &Value,
    run_id: &str,
    task_id: &str,
    expected_revision: i64,
    concurrency: i64,
    approve_large_crawl: bool,
    producer_build: Option<&str>,
    plans: &[Value],
    budgets: &Value,
) -> ProjectsResult<Value> {
    let mut sites = Vec::new();
    for plan in plans {
        let target_root = root.join(plan["directory"].as_str().unwrap_or_default());
        let scan_path = spiderhead_store::history::new_scan_path(
            &target_root.join("scans"),
            plan["url"].as_str().unwrap_or_default(),
            &uuid::Uuid::new_v4().to_string(),
        )?;
        let scan_resolved = scan_path
            .canonicalize()
            .unwrap_or_else(|_| scan_path.clone());
        let root_resolved = target_root.canonicalize().map_err(ProjectsError::Io)?;
        let artifact = scan_resolved
            .strip_prefix(&root_resolved)
            .map(|r| r.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let mut site = Map::new();
        for key in PLAN_FIELDS {
            if key == "urls" {
                continue;
            }
            site.insert(
                key.to_string(),
                plan.get(key).cloned().unwrap_or(Value::Null),
            );
        }
        site.insert(
            "input_hash".to_string(),
            json!(plan_digest(&compatibility_plan(plan))),
        );
        site.insert("state".to_string(), json!("pending"));
        site.insert("artifact".to_string(), json!(artifact));
        site.insert("scan".to_string(), Value::Null);
        site.insert("scan_uuid".to_string(), Value::Null);
        site.insert("partial".to_string(), Value::Null);
        site.insert("finish_reason".to_string(), Value::Null);
        site.insert("reason".to_string(), Value::Null);
        site.insert("fetched".to_string(), json!(0));
        site.insert("queued".to_string(), json!(0));
        site.insert("started_at".to_string(), Value::Null);
        site.insert("finished_at".to_string(), Value::Null);
        sites.push(Value::Object(site));
    }
    let stamp = zulu();
    Ok(json!({
        "format": RUN_FORMAT,
        "run_id": run_id,
        "project_uuid": project["project_uuid"],
        "task_id": task_id,
        "coverage_revision": expected_revision,
        "state": "running",
        "runner_token": Value::Null,
        "started_at": stamp,
        "updated_at": stamp,
        "finished_at": Value::Null,
        "concurrency": concurrency,
        "approve_large_crawl": approve_large_crawl,
        "producer_build": producer_build,
        "fingerprint": manifest_fingerprint(task_id, plans),
        "budgets": budgets,
        "sites": sites,
    }))
}

/// `_manifests`: all retained run manifests in name order.
pub(super) fn manifests(root: &Path, project_uuid: &str) -> ProjectsResult<Vec<Value>> {
    let directory = run_directory(root, false)?;
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&directory)
        .map_err(ProjectsError::Io)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(".json"))
        })
        .collect();
    paths.sort();
    if paths.len() > MAX_RUN_MANIFESTS {
        return Err(competitor_error(&format!(
            "competitor scan history exceeds {MAX_RUN_MANIFESTS} runs"
        )));
    }
    let mut output = Vec::new();
    for path in paths {
        if path.is_symlink()
            || !path.is_file()
            || path.metadata().map(|m| m.len()).unwrap_or(u64::MAX) > MAX_MANIFEST_BYTES
        {
            return Err(competitor_error(
                "competitor scan history contains an unsafe manifest",
            ));
        }
        let run_id = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        output.push(read_manifest(root, &run_id, project_uuid)?);
    }
    Ok(output)
}

/// `_incomplete`: any site still in a non-terminal state.
pub(super) fn incomplete(manifest: &Value) -> bool {
    manifest["sites"].as_array().into_iter().flatten().any(|s| {
        matches!(
            s["state"].as_str(),
            Some("pending" | "running" | "interrupted")
        )
    })
}

/// `_resolve_site_target`: the manifest site must still match a current workspace.
pub(super) fn resolve_site_target<'a>(
    targets: &'a [Value],
    site: &Value,
) -> ProjectsResult<&'a Value> {
    let found = targets
        .iter()
        .find(|row| row["project_uuid"] == site["project_uuid"] && row["url"] == site["url"]);
    match found {
        Some(row) if site["directory"] == row["directory"] => Ok(row),
        _ => Err(competitor_error(
            "competitor workspace changed since the run checkpoint",
        )),
    }
}

/// `task_competitor_run_reference`: the latest compatible run for one task (no writes).
/// Returns `Ok(Null)` when no compatible run exists; errors propagate.
pub fn task_competitor_run_reference(
    directory: &Path,
    task_id: &str,
    task: &Value,
) -> ProjectsResult<Value> {
    let probe = (|| -> ProjectsResult<Value> {
        let (root, project) = crate::workspace::load(directory)?;
        let definition = task.get("definition").cloned().unwrap_or_else(|| json!({}));
        if !definition.is_object() {
            return Err(py_named(
                "AttributeError",
                format!(
                    "'{}' object has no attribute 'get'",
                    py_typename(&definition)
                ),
            ));
        }
        if definition.get("kind").and_then(|k| k.as_str()) != Some("custom") {
            return Ok(Value::Null);
        }
        if falsy(&definition["competitors"]) {
            return Ok(Value::Null);
        }
        let target_rows = targets(&root, &project, task)?;
        let (plans, _budgets) = plans_for(&target_rows)?;
        let all = manifests(&root, project["project_uuid"].as_str().unwrap_or_default())?;
        let compatible: Vec<Value> = all
            .into_iter()
            .filter(|m| manifest_matches_inputs(m, task_id, &plans))
            .collect();
        if compatible.is_empty() {
            return Ok(Value::Null);
        }
        let resumable: Vec<&Value> = compatible.iter().filter(|m| incomplete(m)).collect();
        let pool: Vec<&Value> = if resumable.is_empty() {
            compatible.iter().collect()
        } else {
            resumable
        };
        let manifest = pool
            .into_iter()
            .reduce(|a, b| {
                if b["updated_at"].as_str() > a["updated_at"].as_str() {
                    b
                } else {
                    a
                }
            })
            .unwrap_or(&Value::Null);
        let active = run_lock_active(&root)?;
        let mut state = manifest["state"].as_str().unwrap_or_default().to_string();
        if state == "running" && !active {
            state = "interrupted".to_string();
        }
        Ok(json!({
            "run_id": manifest["run_id"],
            "state": state,
            "resumable": incomplete(manifest),
            "approve_large_crawl": manifest["approve_large_crawl"],
            "producer_build": manifest.get("producer_build").cloned().unwrap_or(Value::Null),
            "updated_at": manifest["updated_at"],
            "coverage_revision": manifest["coverage_revision"],
            "completed_sites": manifest["sites"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|s| s["state"].as_str() == Some("completed"))
                .count(),
            "total_sites": manifest["sites"].as_array().map(|s| s.len()).unwrap_or(0),
        }))
    })();
    match probe {
        Ok(v) => Ok(v),
        Err(e) if reference_swallows(&e) => Ok(json!({
            "state": "unavailable",
            "resumable": false,
            "reason": "run history is unavailable",
        })),
        Err(e) => Err(e),
    }
}

/// `task_competitor_run_reference` swallows only `(OSError, TypeError, ValueError)`.
pub(super) fn reference_swallows(error: &ProjectsError) -> bool {
    match error {
        ProjectsError::Io(_) | ProjectsError::Value(_) | ProjectsError::Json(_) => true,
        ProjectsError::Store(_) => false,
        ProjectsError::Py { name, .. } => matches!(
            *name,
            "ValueError"
                | "TypeError"
                | "CompetitorScanError"
                | "JSONDecodeError"
                | "OSError"
                | "FileNotFoundError"
                | "FileExistsError"
                | "PermissionError"
                | "IsADirectoryError"
                | "NotADirectoryError"
        ),
    }
}

/// The crawl outcome an injected `crawl_site` handler can signal.
pub enum CrawlFault {
    /// `KeyboardInterrupt`/`SystemExit`/`asyncio.CancelledError`: class name and `str(exc)`.
    Interrupted(&'static str, String),
    /// Any other exception; carries `str(exc)`.
    Failed(String),
}

/// Progress sink passed to `crawl_site` as `kwargs["progress"]`; `Err((name, message))`
/// raises the interrupt inside the crawl, so the crawl stops at that point.
pub type SiteProgress<'a> =
    dyn Fn(i64, i64) -> Result<(), (&'static str, String)> + Send + Sync + 'a;

/// The `crawl_site` callable shape: `(kwargs, progress) -> result`.
pub type CrawlSite<'a> =
    dyn Fn(&Map<String, Value>, &SiteProgress) -> Result<Value, CrawlFault> + Sync + 'a;

/// Optional user-facing `progress(url, fetched, queued)` callback.
pub type RunProgress<'a> = dyn Fn(&str, i64, i64) + Sync + 'a;

/// Optional cancel probe polled at site and progress boundaries.
pub type CancelProbe<'a> = dyn Fn() -> bool + Sync + 'a;

/// Injected side effects for `scan_task_competitors` (`tools={"crawl_site": ..}`).
pub struct CompetitorTools<'a> {
    /// The existing `crawl_site` handler: `(kwargs, progress) -> result`.
    pub crawl_site: &'a CrawlSite<'a>,
    /// Optional user-facing progress sink (`progress(url, fetched, queued)`).
    pub progress: Option<&'a RunProgress<'a>>,
    /// Optional cancel probe polled at site and progress boundaries.
    pub cancel_requested: Option<&'a CancelProbe<'a>>,
}

/// `scan_task_competitors`: run/resume a durable bounded competitor scan.
#[allow(clippy::too_many_arguments)]
pub fn scan_task_competitors(
    directory: &Path,
    task_id: &str,
    expected_revision: i64,
    confirm: bool,
    concurrency: i64,
    approve_large_crawl: bool,
    producer_build: Option<&str>,
    tools: &CompetitorTools,
    run_id: Option<&str>,
) -> ProjectsResult<Value> {
    if !confirm {
        return Err(value(
            "confirm=true is required to contact competitor sites",
        ));
    }
    if task_id.is_empty() {
        return Err(value("task_id is required"));
    }
    if !(1..=MAX_CONCURRENCY).contains(&concurrency) {
        return Err(value(format!("concurrency must be 1..{MAX_CONCURRENCY}")));
    }
    let run_id = match run_id {
        Some(id) => Some(
            uuid::Uuid::parse_str(id)
                .map(|u| u.to_string())
                .map_err(|_| value("run_id must be a UUID"))?,
        ),
        None => None,
    };
    let (root, project) = crate::workspace::load(directory)?;
    let checklist = crate::coverage::coverage_status(&root, true)?;
    if checklist["revision"].as_i64() != Some(expected_revision) {
        return Err(value(format!(
            "coverage revision conflict: current revision is {}",
            py_str(checklist.get("revision").unwrap_or(&Value::Null))
        )));
    }
    let document = crate::coverage::store::read(&root, &project)?.unwrap_or_else(|| json!({}));
    let saved = document
        .get("items")
        .and_then(|i| i.get(task_id))
        .cloned()
        .unwrap_or(Value::Null);
    if falsy(&saved) {
        return Err(value("task_id must name a current custom task"));
    }
    let saved_definition = obj_get(&saved, "definition")?;
    if !saved_definition.is_object() {
        return Err(py_named(
            "AttributeError",
            format!(
                "'{}' object has no attribute 'get'",
                py_typename(saved_definition)
            ),
        ));
    }
    if saved_definition.get("kind").and_then(|k| k.as_str()) != Some("custom") {
        return Err(value("task_id must name a current custom task"));
    }
    let target_rows = targets(&root, &project, &saved)?;
    let (plans, budgets) = plans_for(&target_rows)?;
    let run_lock;
    {
        let _guard = crate::runtime::project_write_lock(&root, 5.0)?;
        let (current_root, current_project) = crate::workspace::load(directory)?;
        if current_root != root || current_project["project_uuid"] != project["project_uuid"] {
            return Err(competitor_error(
                "project changed before competitor-run admission",
            ));
        }
        run_lock = open_run_lock(&root)?;
    }
    let outcome = scan_run_body(
        &root,
        &project,
        &run_lock.file,
        task_id,
        expected_revision,
        concurrency,
        approve_large_crawl,
        producer_build,
        tools,
        run_id.as_deref(),
        &target_rows,
        &plans,
        &budgets,
    );
    drop(run_lock);
    outcome
}

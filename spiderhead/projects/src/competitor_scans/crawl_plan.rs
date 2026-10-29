use super::*;

/// `_crawl_plan`: the bounded per-site crawl inputs from its schedule settings.
pub(super) fn crawl_plan(target: &Value, requested_max_urls: i64) -> ProjectsResult<Value> {
    let target_root = PathBuf::from(target["root"].as_str().unwrap_or_default());
    let schedule = crate::schedule::show(&target_root, None)?;
    let settings = obj_get(&schedule, "settings")?;
    let crawl = obj_get(settings, "crawl")?;
    let cap = {
        // The URL cap is the smallest of the request, the crawl limit and MAX_SITE_URLS.
        let limit = obj_get(crawl, "max_urls")?;
        let limit = match limit {
            Value::Number(n) if n.is_i64() || n.is_u64() => limit.as_i64().unwrap_or(0),
            other => {
                return Err(py_named(
                    "TypeError",
                    format!(
                        "'<' not supported between instances of 'int' and '{}'",
                        py_typename(other)
                    ),
                ))
            }
        };
        requested_max_urls.min(limit).min(MAX_SITE_URLS)
    };
    if cap < 1 {
        return Err(value("competitor URL cap must be positive"));
    }
    let max_requests = {
        let limit = obj_get(crawl, "max_requests")?;
        let limit = match limit {
            Value::Number(n) if n.is_i64() || n.is_u64() => limit.as_i64().unwrap_or(0),
            other => {
                return Err(py_named(
                    "TypeError",
                    format!(
                        "'<' not supported between instances of 'int' and '{}'",
                        py_typename(other)
                    ),
                ))
            }
        };
        MAX_SITE_REQUESTS
            .min(std::cmp::max(150, cap * 3))
            .min(limit)
    };
    let max_seconds = MAX_SITE_SECONDS.min(std::cmp::max(60, cap * 2));
    let mode_value = obj_get(crawl, "mode")?.clone();
    let mode = mode_value.as_str().unwrap_or_default().to_string();
    let user_agent = {
        let agent = obj_get(settings, "user_agent")?;
        let custom = obj_get(agent, "custom")?;
        let preset = obj_get(agent, "preset")?;
        if !falsy(custom) {
            custom.clone()
        } else {
            preset.clone()
        }
    };
    let min_delay = py_float(obj_get(crawl, "min_delay_seconds")?)?;
    let urls = py_list(obj_get(crawl, "urls")?)?;
    let mut plan = json!({
        "url": target["url"],
        "project_uuid": target["project_uuid"],
        "directory": target["directory"],
        "max_urls": cap,
        "max_requests": max_requests,
        "max_crawl_seconds": max_seconds,
        "mode": mode_value,
        "user_agent": user_agent,
        "min_delay_seconds": min_delay,
        "sitemap_url": obj_get(crawl, "sitemap_url")?,
        "urls": urls,
        "schedule_revision": obj_get(&schedule, "revision")?,
    });
    if mode == "list" && falsy(&plan["urls"]) {
        let monitor = crate::runtime::read_document(&target_root, "monitor.json")?
            .unwrap_or_else(|| json!({}));
        let policy = monitor.get("policy").cloned().unwrap_or(Value::Null);
        let policy = if falsy(&policy) { json!({}) } else { policy };
        let monitor_urls = policy.get("urls").cloned().unwrap_or(Value::Null);
        let monitor_urls = if falsy(&monitor_urls) {
            json!([])
        } else {
            monitor_urls
        };
        plan["urls"] = Value::Array(py_list(&monitor_urls)?);
    }
    if mode == "list" && falsy(&plan["urls"]) {
        return Err(value("the competitor's list crawl has no configured URLs"));
    }
    if mode == "list" {
        let urls_value = &plan["urls"];
        let urls = urls_value.as_array().cloned().unwrap_or_default();
        let bad = !urls_value.is_array()
            || urls.len() > MAX_SITE_URLS as usize
            || urls.iter().any(|u| {
                !u.is_string()
                    || u.as_str().is_some_and(|url| {
                        urlsplit(url)
                            .map(|s| {
                                !(s.scheme == "http" || s.scheme == "https") || s.netloc.is_empty()
                            })
                            .unwrap_or(true)
                    })
            });
        if bad {
            return Err(value("the competitor's URL list is invalid or unbounded"));
        }
        let unique: std::collections::HashSet<&str> =
            urls.iter().filter_map(|u| u.as_str()).collect();
        if unique.len() != urls.len() {
            return Err(value("the competitor's URL list contains duplicates"));
        }
        if urls.len() as i64 > cap {
            return Err(value(
                "the task cap is below the configured list size; refusing to truncate it",
            ));
        }
    }
    Ok(plan)
}

/// Read a JSON value as a list: arrays pass through, objects yield their keys in order.
pub(super) fn py_list(v: &Value) -> ProjectsResult<Vec<Value>> {
    match v {
        Value::Array(items) => Ok(items.clone()),
        Value::Object(map) => Ok(map.keys().map(|k| json!(k)).collect()),
        Value::String(s) => Ok(s.chars().map(|c| json!(c.to_string())).collect()),
        other => Err(py_named(
            "TypeError",
            format!("'{}' object is not iterable", py_typename(other)),
        )),
    }
}

/// Read a JSON value as a float: numbers pass through, numeric strings parse.
pub(super) fn py_float(v: &Value) -> ProjectsResult<f64> {
    match v {
        Value::Number(n) => Ok(n.as_f64().unwrap_or(0.0)),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => s
            .trim()
            .parse::<f64>()
            .map_err(|_| value(format!("could not convert string to float: '{s}'"))),
        other => Err(py_named(
            "TypeError",
            format!(
                "float() argument must be a string or a real number, not '{}'",
                py_typename(other)
            ),
        )),
    }
}

/// `_request`: the `crawl_site` keyword payload for one site.
pub(super) fn request(target: &Value, plan: &Value, scan_out: &Path, approve: bool) -> Value {
    let mut kwargs = json!({
        "project": target["root"].as_str().unwrap_or_default(),
        "max_urls": plan["max_urls"],
        "min_delay": plan["min_delay_seconds"],
        "user_agent": plan["user_agent"],
        "overrides": {
            "limits.max_requests": plan["max_requests"],
            "limits.max_crawl_seconds": plan["max_crawl_seconds"],
        },
        "scan_out": scan_out.to_string_lossy(),
        "approve_large_crawl": approve,
    });
    match plan["mode"].as_str().unwrap_or_default() {
        "spider" => {
            kwargs["url"] = target["url"].clone();
            kwargs["overrides"]["sitemaps.auto_discover"] = json!(true);
        }
        "sitemap" => {
            kwargs["url"] = plan["sitemap_url"].clone();
            kwargs["sitemap"] = plan["sitemap_url"].clone();
            kwargs["sitemap_only"] = json!(true);
        }
        _ => {
            kwargs["urls"] = plan["urls"].clone();
        }
    }
    kwargs
}

/// `_plan_digest`: canonical hash of one site's crawl inputs.
pub(super) fn plan_digest(plan: &Value) -> String {
    format!(
        "{:x}",
        Sha256::digest(pyjson::dumps_compact(plan).as_bytes())
    )
}

/// `_compatibility_plan`: resume identity excludes the metadata-only schedule revision.
pub(super) fn compatibility_plan(plan: &Value) -> Value {
    let mut out = plan.clone();
    if let Some(map) = out.as_object_mut() {
        map.remove("schedule_revision");
    }
    out
}

/// `_manifest_fingerprint`: the run identity over task id and compatibility plans.
pub(super) fn manifest_fingerprint(task_id: &str, plans: &[Value]) -> String {
    let payload = pyjson::dumps_compact(&json!({
        "task_id": task_id,
        "plans": plans.iter().map(compatibility_plan).collect::<Vec<_>>(),
    }));
    format!("{:x}", Sha256::digest(payload.as_bytes()))
}

/// `_legacy_manifest_fingerprint`: manifests written while bound to global revisions.
pub(super) fn legacy_manifest_fingerprint(
    task_id: &str,
    coverage_revision: &Value,
    plans: &[Value],
) -> String {
    let payload = pyjson::dumps_compact(&json!({
        "task_id": task_id,
        "revision": coverage_revision,
        "plans": plans,
    }));
    format!("{:x}", Sha256::digest(payload.as_bytes()))
}

/// `_site_inputs_match`: the stored input hash still matches the site's plan.
pub(super) fn site_inputs_match(site: &Value, plan: &Value) -> bool {
    let input_hash = site["input_hash"].as_str().unwrap_or_default();
    input_hash == plan_digest(plan) || input_hash == plan_digest(&compatibility_plan(plan))
}

/// `_manifest_matches_inputs`: a stored run still describes the current inputs.
pub(super) fn manifest_matches_inputs(manifest: &Value, task_id: &str, plans: &[Value]) -> bool {
    if manifest["task_id"].as_str() != Some(task_id) {
        return false;
    }
    let fingerprint = manifest["fingerprint"].as_str().unwrap_or_default();
    let current = manifest_fingerprint(task_id, plans);
    let legacy = legacy_manifest_fingerprint(task_id, &manifest["coverage_revision"], plans);
    if fingerprint != current && fingerprint != legacy {
        return false;
    }
    let planned: HashMap<(&str, &str), &Value> = plans
        .iter()
        .filter_map(|p| {
            Some((p["project_uuid"].as_str()?, p["url"].as_str()?, p)).map(|(u, s, p)| ((u, s), p))
        })
        .collect();
    let sites = &manifest["sites"];
    if !sites.is_array() || sites.as_array().is_some_and(|s| s.len() != plans.len()) {
        return false;
    }
    for site in sites.as_array().into_iter().flatten() {
        if !site.is_object() {
            return false;
        }
        let key = (
            site["project_uuid"].as_str().unwrap_or_default(),
            site["url"].as_str().unwrap_or_default(),
        );
        match planned.get(&key) {
            Some(plan) if site_inputs_match(site, plan) => {}
            _ => return false,
        }
    }
    true
}

/// `_manifest_result`: the public summary of a finished or resumed run.
pub(super) fn manifest_result(manifest: &Value, resumed: bool) -> Value {
    let keys = [
        "url",
        "project_uuid",
        "max_urls",
        "max_requests",
        "max_crawl_seconds",
        "state",
        "scan",
        "scan_uuid",
        "partial",
        "finish_reason",
        "reason",
        "fetched",
        "queued",
    ];
    let results: Vec<Value> = manifest["sites"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|item| {
            let mut row = Map::new();
            for key in keys {
                row.insert(
                    key.to_string(),
                    item.get(key).cloned().unwrap_or(Value::Null),
                );
            }
            Value::Object(row)
        })
        .collect();
    json!({
        "ok": manifest["state"].as_str() == Some("completed"),
        "run_id": manifest["run_id"],
        "state": manifest["state"],
        "task_id": manifest["task_id"],
        "coverage_revision": manifest["coverage_revision"],
        "concurrency": manifest["concurrency"],
        "budgets": manifest["budgets"],
        "cancel_reason": manifest.get("cancel_reason").cloned().unwrap_or(Value::Null),
        "results": results,
        "resumed": resumed,
    })
}

/// Process-local run locks keyed by resolved project root (`_LOCAL_RUN_LOCKS`).
pub(super) static LOCAL_RUN_LOCKS: LazyLock<Mutex<HashMap<PathBuf, &'static Mutex<()>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) fn local_run_lock(root: &Path) -> &'static Mutex<()> {
    let key = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut map = LOCAL_RUN_LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    map.entry(key)
        .or_insert_with(|| Box::leak(Box::new(Mutex::new(()))))
}

/// Guard holding both the process-local lock and the durable file lock.
pub(super) struct RunLockGuard {
    pub(super) file: std::fs::File,
    held: Option<std::sync::MutexGuard<'static, ()>>,
}

impl Drop for RunLockGuard {
    fn drop(&mut self) {
        let _ = locks::unlock(&self.file);
        drop(self.held.take());
    }
}

/// `_open_run_lock`: claim the in-process lock then the durable file lock.
pub(super) fn open_run_lock(root: &Path) -> ProjectsResult<RunLockGuard> {
    let local = local_run_lock(root);
    let held = match local.try_lock() {
        Ok(guard) => Some(guard),
        Err(_) => return Err(competitor_error("another competitor scan owns the project")),
    };
    let path = root.join(RUN_LOCK);
    match locks::open_lock(&path, true).and_then(|file| locks::lock_exclusive(&file).map(|_| file))
    {
        Ok(file) => Ok(RunLockGuard { file, held }),
        Err(_) => Err(competitor_error("another competitor scan owns the project")),
    }
}

/// `_set_run_lock_owner`: write `{"run_id":..,"runner_token":..}` in place at offset 1.
pub(super) fn set_run_lock_owner(
    file: &std::fs::File,
    run_id: &str,
    runner_token: &str,
) -> ProjectsResult<()> {
    let record = pyjson::dumps_compact(&json!({
        "run_id": run_id,
        "runner_token": runner_token,
    }));
    file.set_len(1).map_err(ProjectsError::Io)?;
    let mut file = file;
    file.seek(SeekFrom::Start(0)).map_err(ProjectsError::Io)?;
    file.write_all(b"\0").map_err(ProjectsError::Io)?;
    file.seek(SeekFrom::Start(1)).map_err(ProjectsError::Io)?;
    file.write_all(record.as_bytes())
        .map_err(ProjectsError::Io)?;
    file.sync_all().map_err(ProjectsError::Io)
}

/// `_run_lock_owner_locked`: read the active runner identity under the write lock.
pub(super) fn run_lock_owner_locked(root: &Path) -> ProjectsResult<(bool, Option<Value>)> {
    let path = root.join(RUN_LOCK);
    if path.symlink_metadata().is_err() {
        return Ok((false, None));
    }
    let mut file = locks::open_lock(&path, false)
        .map_err(|_| competitor_error("competitor scan lock is unsafe or unavailable"))?;
    let mut raw = [0u8; 256];
    file.seek(SeekFrom::Start(1)).map_err(ProjectsError::Io)?;
    let n = file.read(&mut raw).map_err(ProjectsError::Io)?;
    drop(file);
    let active = run_lock_active(root)?;
    if !active {
        return Ok((false, None));
    }
    if !raw[..n].iter().all(|b| b.is_ascii()) {
        return Err(competitor_error(
            "active competitor runner identity is unavailable",
        ));
    }
    let text = String::from_utf8(raw[..n].to_vec())
        .map_err(|_| competitor_error("active competitor runner identity is unavailable"))?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|_| competitor_error("active competitor runner identity is unavailable"))?;
    let keys_ok = value.as_object().is_some_and(|m| {
        m.len() == 2 && m.contains_key("run_id") && m.contains_key("runner_token")
    });
    let token_ok = value["runner_token"]
        .as_str()
        .is_some_and(|t| is_hex(t, 32));
    if !keys_ok || !value["run_id"].is_string() || !token_ok {
        return Err(competitor_error(
            "active competitor runner identity is invalid",
        ));
    }
    let mut value = value;
    let normalized = uuid::Uuid::parse_str(value["run_id"].as_str().unwrap_or_default())
        .map(|u| u.to_string())
        .map_err(|_| competitor_error("active competitor runner identity is invalid"))?;
    value["run_id"] = json!(normalized);
    Ok((true, Some(value)))
}

/// `_run_lock_active`: fail-closed probe for the durable competitor run lock.
pub(super) fn run_lock_active(root: &Path) -> ProjectsResult<bool> {
    let local = local_run_lock(root);
    if local.try_lock().is_err() {
        return Ok(true);
    }
    let path = root.join(RUN_LOCK);
    if path.symlink_metadata().is_err() {
        return Ok(false);
    }
    let file = match locks::open_lock(&path, false) {
        Ok(file) => file,
        Err(_) => {
            return Err(competitor_error(
                "competitor scan lock is unsafe or unavailable",
            ))
        }
    };
    match locks::lock_exclusive(&file) {
        Ok(()) => {
            let _ = locks::unlock(&file);
            Ok(false)
        }
        Err(_) => Ok(true),
    }
}

/// `has_active_run`: whether any competitor runner owns this project's durable lock.
pub fn has_active_run(root: &Path) -> ProjectsResult<bool> {
    run_lock_active(root)
}

/// `_plans`: per-site crawl plans plus the shared run budgets.
pub(super) fn plans_for(target_rows: &[Value]) -> ProjectsResult<(Vec<Value>, Value)> {
    let mut plans = Vec::new();
    for target in target_rows {
        plans.push(crawl_plan(
            target,
            target["max_urls"].as_i64().unwrap_or(0),
        )?);
    }
    let budgets = json!({
        "urls": plans.iter().map(|p| p["max_urls"].as_i64().unwrap_or(0)).sum::<i64>(),
        "requests": plans.iter().map(|p| p["max_requests"].as_i64().unwrap_or(0)).sum::<i64>(),
        "seconds": plans.iter().map(|p| p["max_crawl_seconds"].as_i64().unwrap_or(0)).sum::<i64>(),
        "max_urls": MAX_TOTAL_URLS,
        "max_requests": MAX_TOTAL_REQUESTS,
        "max_seconds": MAX_TOTAL_SECONDS,
    });
    if budgets["urls"].as_i64().unwrap_or(0) > MAX_TOTAL_URLS {
        return Err(value(format!(
            "aggregate competitor URL cap is {MAX_TOTAL_URLS}"
        )));
    }
    if budgets["requests"].as_i64().unwrap_or(0) > MAX_TOTAL_REQUESTS {
        return Err(value(format!(
            "aggregate competitor request cap is {MAX_TOTAL_REQUESTS}"
        )));
    }
    if budgets["seconds"].as_i64().unwrap_or(0) > MAX_TOTAL_SECONDS {
        return Err(value(format!(
            "aggregate competitor crawl-time cap is {MAX_TOTAL_SECONDS} seconds"
        )));
    }
    Ok((plans, budgets))
}

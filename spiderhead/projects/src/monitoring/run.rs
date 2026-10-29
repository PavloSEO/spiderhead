use super::*;

/// `run`: retain an already-collected bounded diff; no fetch, timer or delivery.
pub fn run(
    directory: &Path,
    scan_id: &str,
    observations: &Value,
    expected_revision: i64,
) -> ProjectsResult<Value> {
    run_at(directory, scan_id, observations, expected_revision, None)
}

/// `run` with an injected clock, so tests can fix the time.
pub fn run_at(
    directory: &Path,
    scan_id: &str,
    observations: &Value,
    expected_revision: i64,
    now: Option<&dyn Fn() -> String>,
) -> ProjectsResult<Value> {
    let clock = || -> String { now.map(|n| n()).unwrap_or_else(zulu) };
    let (root, mut document) = monitor_load(directory)?;
    if document["revision"] != json!(expected_revision) || document["policy"].is_null() {
        return Err(value("monitor revision conflict or missing policy"));
    }
    let policy = document["policy"].clone();
    if !policy.is_object() {
        return Err(py_named(
            "TypeError",
            format!("'{}' object is not subscriptable", py_typename(&policy)),
        ));
    }
    if scan_id.trim().is_empty() || scan_id.chars().count() > 512 {
        return Err(value("scan_id required"));
    }
    let runner_state = document["runner"]
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("idle");
    if ["cancelled", "backoff", "interrupted"].contains(&runner_state) {
        return Err(value(
            "restart the monitor claim before retaining observations",
        ));
    }
    let due_plan = planned_due(&document)?;
    let planned_urls = py_list(
        due_plan
            .get("planned_urls")
            .unwrap_or_else(|| &policy["urls"]),
    )?;
    let limit = if due_plan.get("state").and_then(Value::as_str) == Some("due") {
        planned_urls.len() as i64
    } else {
        int_field(&policy, "max_urls")?
    };
    let items = match observations.as_array() {
        Some(items) if !items.is_empty() && items.len() as i64 <= limit => items.clone(),
        _ => return Err(value("observations exceed configured incremental scope")),
    };
    let mut normalized = Vec::new();
    for item in &items {
        normalized.push(observation(item, &policy, &root, &clock)?);
    }
    for item in &mut normalized {
        let derived = measurement_changes(&document, item)?;
        let seen: HashSet<String> = item["changes"]
            .as_array()
            .map(|c| c.iter().map(change_key).collect())
            .unwrap_or_default();
        for change in derived {
            if !seen.contains(&change_key(&change)) {
                if let Some(items) = item["changes"].as_array_mut() {
                    items.push(change);
                }
            }
        }
    }
    let unique: HashSet<String> = normalized
        .iter()
        .map(|item| dumps_compact(&item["url"]))
        .collect();
    if unique.len() != normalized.len() {
        return Err(value("monitor observations may contain each URL only once"));
    }
    let requests: i64 = normalized
        .iter()
        .map(|item| item["request_count"].as_i64().unwrap_or_default())
        .sum();
    if requests > int_field(&policy, "max_requests")? {
        return Err(value(
            "monitor observations exceed configured request budget",
        ));
    }
    let renders: i64 = normalized
        .iter()
        .map(|item| item["render_request_count"].as_i64().unwrap_or_default())
        .sum();
    if renders > int_field(&policy, "max_render_requests")? {
        return Err(value(
            "monitor observations exceed configured render request budget",
        ));
    }
    let observed_set: HashSet<String> = unique;
    let planned_set: HashSet<String> = planned_urls.iter().map(dumps_compact).collect();
    if due_plan.get("state").and_then(Value::as_str) == Some("due")
        && !observed_set.is_subset(&planned_set)
    {
        return Err(value(
            "monitor observations are outside the planned bounded scope",
        ));
    }
    if due_plan.get("state").and_then(Value::as_str) == Some("due")
        && due_plan.get("mode").and_then(Value::as_str) == Some("full")
        && observed_set != planned_set
    {
        return Err(value(
            "periodic full refresh must retain an observation for every configured URL",
        ));
    }
    let cov = coverage(&planned_urls, &normalized);
    let suppression = int_field(&policy, "suppression_runs").unwrap_or(-1);
    let previous = recent_alerts(
        &document,
        if suppression > 0 {
            suppression as usize
        } else {
            usize::MAX
        },
    );
    let mut alerts = Vec::new();
    let mut recoveries = Vec::new();
    for item in &normalized {
        if ["stale", "unavailable", "partial", "failed"]
            .contains(&item["qualifier"].as_str().unwrap_or_default())
        {
            continue;
        }
        let meaningful: Vec<Value> = item["changes"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|change| {
                threshold(&policy, change)
                    && change.get("kind").and_then(Value::as_str) != Some("recovered")
            })
            .collect();
        let fresh: Vec<Value> = meaningful
            .into_iter()
            .filter(|change| !previous.contains(&change_key(change)))
            .collect();
        if !fresh.is_empty() {
            alerts.push(json!({
                "url": item["url"],
                "changes": fresh,
                "state": "actionable",
            }));
        }
        for change in item["changes"].as_array().cloned().unwrap_or_default() {
            if change.get("kind").and_then(Value::as_str) == Some("recovered")
                && threshold(&policy, &change)
            {
                recoveries.push(json!({
                    "url": item["url"],
                    "change": change,
                    "state": "recovery",
                }));
            }
        }
    }
    let state = if cov["failed_count"].as_i64().unwrap_or_default() > 0
        && cov["fresh_count"].as_i64().unwrap_or_default()
            + cov["revalidated_count"].as_i64().unwrap_or_default()
            == 0
    {
        "failed"
    } else if !cov["complete"].as_bool().unwrap_or(false) {
        "partial"
    } else if !alerts.is_empty() || !recoveries.is_empty() {
        "actionable"
    } else {
        "quiet"
    };
    let mode = due_plan
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("incremental");
    let observed_urls: Vec<Value> = normalized.iter().map(|item| item["url"].clone()).collect();
    let planned_len = planned_urls.len() as i64;
    let retained = json!({
        "scan_id": scan_id.trim(),
        "recorded_at": clock(),
        "mode": mode,
        "planned_urls": planned_urls,
        "observations": normalized,
        "observed_urls": observed_urls,
        "coverage": cov.clone(),
        "alerts": alerts,
        "recoveries": recoveries,
        "baseline": {"scan_id": scan_id.trim(), "observations": normalized.clone()},
        "state": state,
        "notification": "none",
    });
    if let Some(runs) = document["runs"].as_array_mut() {
        runs.push(retained.clone());
    }
    let completed_full = mode == "full" && cov["complete"].as_bool().unwrap_or(false);
    let completed_incremental = mode == "incremental" && cov["complete"].as_bool().unwrap_or(false);
    let mut offset = document["runner"]
        .get("next_url_offset")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let urls_len = policy["urls"].as_array().map(Vec::len).unwrap_or(1) as i64;
    if completed_full {
        offset = 0;
    } else if completed_incremental {
        offset = (offset + planned_len).rem_euclid(urls_len);
    }
    let runs_since = if completed_full {
        0
    } else if mode == "full" {
        int_field(&policy, "full_refresh_every")?
    } else {
        document["runner"]
            .get("runs_since_full_refresh")
            .and_then(Value::as_i64)
            .unwrap_or_default()
            + if completed_incremental { 1 } else { 0 }
    };
    document["runner"] = json!({
        "state": "idle",
        "runs_since_full_refresh": runs_since,
        "next_url_offset": offset,
        "last_finished_at": retained["recorded_at"],
        "next_due_at": expiry(&clock(), int_field(&policy, "interval_seconds")?)?,
    });
    document["revision"] = json!(expected_revision + 1);
    write_document(&root, NAME, &document, Some(expected_revision))?;
    Ok(json!({
        "ok": true,
        "revision": document["revision"],
        "run": retained,
        "coverage": cov,
        "notification": "none",
        "due": due(&document)?,
    }))
}

/// `schedule`: claim, cancel, back off or recover a local runner; never a timer.
pub fn schedule(
    directory: &Path,
    action: &str,
    expected_revision: i64,
    lease_seconds: Option<i64>,
) -> ProjectsResult<Value> {
    let lease = lease_seconds.unwrap_or(DEFAULT_LEASE_SECONDS);
    let (root, mut document) = monitor_load(directory)?;
    if document["revision"] != json!(expected_revision) || document["policy"].is_null() {
        return Err(value("monitor revision conflict or missing policy"));
    }
    if !["start", "cancel", "backoff", "recover"].contains(&action) {
        return Err(value(
            "schedule action must be start, cancel, backoff or recover",
        ));
    }
    if !(1..=3600).contains(&lease) {
        return Err(value(
            "monitor lease_seconds must be an integer from 1 to 3600",
        ));
    }
    let current = document["runner"]
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("idle");
    let runner = match action {
        "start" => {
            if !py_truthy(obj_get(&document["policy"], "enabled")?) {
                return Err(value(
                    "monitor policy is disabled; explicit enablement is required",
                ));
            }
            if current == "running" {
                return Err(value(
                    "monitor runner already active; overlapping schedules are refused",
                ));
            }
            let claimed_at = zulu();
            let mut runner = document["runner"].clone();
            runner["state"] = json!("running");
            runner["claimed_at"] = json!(claimed_at);
            runner["claim_id"] = json!(format!("monitor:{}", uuid::Uuid::new_v4()));
            runner["lease_expires_at"] = json!(expiry(&claimed_at, lease)?);
            runner["plan"] = due(&document)?;
            runner["dispatched_urls"] = json!([]);
            runner
        }
        "recover" => {
            if current != "running" {
                return Err(value("only an interrupted running claim can be recovered"));
            }
            let mut runner = document["runner"].clone();
            runner["state"] = json!("interrupted");
            runner["interrupted_at"] = json!(zulu());
            runner
        }
        _ => {
            let mut runner = document["runner"].clone();
            runner["state"] = json!(if action == "cancel" {
                "cancelled"
            } else {
                "backoff"
            });
            runner["changed_at"] = json!(zulu());
            runner
        }
    };
    document["runner"] = runner.clone();
    document["revision"] = json!(expected_revision + 1);
    write_document(&root, NAME, &document, Some(expected_revision))?;
    Ok(json!({
        "ok": true,
        "revision": document["revision"],
        "runner": runner,
        "scheduled": false,
        "due": due(&document)?,
    }))
}

/// `_artifact`: publish one payload under reports/ and return (relative, digest).
pub(super) fn monitor_artifact(
    root: &Path,
    relative: &str,
    payload: &[u8],
) -> ProjectsResult<(String, String)> {
    let path = root.join(relative);
    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
    if path.is_symlink()
        || !parent.is_dir()
        || !resolve_lenient(&parent).starts_with(resolve_lenient(root))
    {
        return Err(value("monitor artifact path is unsafe"));
    }
    io(spiderhead_core::filesystem::atomic_write_bytes(
        &path, payload, None, None, true,
    ))?;
    let written = digest(&path, Some(monotonic() + 5.0), Some(payload.len() as u64))?;
    if written != hex_digest(payload) {
        return Err(value("monitor artifact hash verification failed"));
    }
    Ok((relative.to_string(), written))
}

/// `_reports_root`: require an existing safe reports/ directory.
pub(super) fn reports_root(root: &Path) -> ProjectsResult<PathBuf> {
    let reports = root.join("reports");
    if reports.is_symlink()
        || !reports.is_dir()
        || !resolve_lenient(&reports).starts_with(resolve_lenient(root))
    {
        return Err(value("project reports directory is unsafe"));
    }
    Ok(reports)
}

/// `_source_artifacts`: retain body and post-cache validation evidence.
pub(super) fn source_artifacts(
    root: &Path,
    claim_id: &str,
    record: &Value,
    parsed: Option<&Value>,
    transport: &Value,
) -> ProjectsResult<Value> {
    let url = record
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let token = hex_digest(url.as_bytes());
    let reports = reports_root(root)?;
    let monitor_root = reports.join("monitor");
    if monitor_root.exists() && (monitor_root.is_symlink() || !monitor_root.is_dir()) {
        return Err(value("monitor artifact directory is unsafe"));
    }
    io(std::fs::DirBuilder::new()
        .private_mode(0o700)
        .create(&monitor_root)
        .or_else(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                Ok(())
            } else {
                Err(e)
            }
        }))?;
    let directory = monitor_root.join(claim_id.strip_prefix("monitor:").unwrap_or(claim_id));
    if directory.is_symlink() || !resolve_lenient(&directory).starts_with(resolve_lenient(root)) {
        return Err(value("monitor artifact directory is unsafe"));
    }
    io(std::fs::DirBuilder::new()
        .recursive(true)
        .private_mode(0o700)
        .create(&directory))?;
    let body = parsed
        .and_then(|p| p.get("_raw_html"))
        .cloned()
        .unwrap_or(Value::Null);
    let links = parsed
        .and_then(|p| p.get("links"))
        .cloned()
        .unwrap_or(json!([]));
    let link_digest =
        hex_digest(dumps_default(&if links.is_array() { links } else { json!([]) }).as_bytes());
    let status_code = transport.get("status_code").and_then(Value::as_i64);
    let cache_control = transport
        .get("cache_control")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or_default();
    let directives = spiderhead_core::cache_control::parse_cache_control(cache_control);
    let retain_body = body.is_string()
        && matches!(status_code, Some(200) | Some(304))
        && !directives.contains_key("no-store")
        && !py_truthy(&transport.get("set_cookie").cloned().unwrap_or(Value::Null));
    let (body_ref, body_hash) = if retain_body {
        let relative = directory
            .strip_prefix(root)
            .map(|p| p.join(format!("{token}.body.html")))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (reference, digest_value) = monitor_artifact(
            root,
            &relative,
            body.as_str().unwrap_or_default().as_bytes(),
        )?;
        (json!(reference), json!(digest_value))
    } else {
        (Value::Null, Value::Null)
    };
    let validation = json!({
        "url": url,
        "effective_status_code": record.get("status_code").cloned().unwrap_or(Value::Null),
        "cache_status": record.get("cache_status").cloned().unwrap_or(Value::Null),
        "transport": transport,
        "validators": {
            "etag": transport.get("etag").cloned().unwrap_or(Value::Null),
            "last_modified": transport.get("last_modified").cloned().unwrap_or(Value::Null),
        },
        "body_ref": body_ref,
        "body_sha256": body_hash,
        "links_sha256": link_digest,
        "record": record,
    });
    let relative = directory
        .strip_prefix(root)
        .map(|p| p.join(format!("{token}.validation.json")))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let payload = format!("{}\n", dumps_default(&validation));
    let (validation_ref, validation_hash) = monitor_artifact(root, &relative, payload.as_bytes())?;
    Ok(json!({
        "body_ref": body_ref,
        "body_sha256": body_hash,
        "validation_ref": validation_ref,
        "validation_sha256": validation_hash,
        "links_sha256": link_digest,
    }))
}

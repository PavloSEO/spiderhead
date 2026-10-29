use super::*;

/// `admission`: bound requested work before the first request.
pub fn admission(directory: &Path, settings: &Value, approved: bool) -> ProjectsResult<Value> {
    let policy = project_policy(directory, None, false, None, None)?;
    let limits = &settings["limits"];
    let requested = json!({
        "pages": limits.get("max_urls"),
        "requests": limits.get("max_requests"),
        "seconds": limits.get("max_crawl_seconds"),
    });
    let mut exceeded = Vec::new();
    for (key, maximum) in policy["policy"]["approval_thresholds"]
        .as_object()
        .cloned()
        .unwrap_or_default()
    {
        let requested_value = &requested[&key];
        let number = match requested_value {
            Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
            _ => f64::NAN,
        };
        if !number.is_finite() || number <= 0.0 {
            exceeded.push(json!({
                "budget": key,
                "requested": requested_value,
                "threshold": maximum,
                "reason": "request is not explicitly bounded",
            }));
        } else if number > maximum.as_f64().unwrap_or(0.0) {
            exceeded.push(json!({
                "budget": key,
                "requested": requested_value,
                "threshold": maximum,
                "reason": "requested budget exceeds project threshold",
            }));
        }
    }
    Ok(json!({
        "ok": exceeded.is_empty() || approved,
        "approved": approved,
        "requested": requested,
        "exceeded": exceeded,
        "reason": if approved {
            "explicit budget override accepted"
        } else if exceeded.is_empty() {
            "within project thresholds"
        } else {
            "project admission requires approve_large_crawl=true"
        },
    }))
}

/// `preparation_status`: read the bounded preparation document.
pub fn preparation_status(directory: &Path) -> ProjectsResult<Value> {
    let (root, project) = workspace_load(directory)?;
    let Some(state) = read_document(&root, "preparation.json")? else {
        return Ok(json!({
            "state": "pending",
            "reason": "bounded project preparation has not run",
        }));
    };
    if state.get("format").and_then(Value::as_str) != Some(PREPARATION_FORMAT)
        || state.get("project_uuid").and_then(Value::as_str)
            != project.get("project_uuid").and_then(Value::as_str)
        || !state.get("steps").is_some_and(Value::is_object)
        || !state.get("competitors").is_some_and(Value::is_array)
    {
        return Err(crate::error::value("invalid project preparation state"));
    }
    if state.get("state").and_then(Value::as_str) == Some("running")
        && !root.join(".prepare.lock").exists()
    {
        return Ok(json!({
            "format": state["format"],
            "project_uuid": state["project_uuid"],
            "revision": state["revision"],
            "started_at": state["started_at"],
            "state": "interrupted",
            "steps": state["steps"],
            "competitors": state["competitors"],
            "reason": "preparation writer stopped before recording completion",
        }));
    }
    Ok(state)
}

/// `_candidate_url`: validate like a target but keep the entered host (www included).
pub(super) fn candidate_url(raw: &str) -> ProjectsResult<String> {
    workspace_target(raw)?;
    let parts = crate::urls::urlsplit(raw.trim())?;
    let port = match parts.port() {
        Err(_) => return Err(crate::error::value("target must have a valid HTTP(S) port")),
        Ok(Some(p)) => format!(":{p}"),
        Ok(None) => String::new(),
    };
    let host = parts.hostname().unwrap_or_default();
    let path = if parts.path.is_empty() {
        "/"
    } else {
        parts.path.as_str()
    };
    Ok(crate::urls::urlunsplit(
        &parts.scheme.to_lowercase(),
        &format!("{host}{port}"),
        path,
        "",
        "",
    ))
}

/// `_candidate`: one operator-supplied or observed competitor candidate.
pub fn candidate(raw: &Value) -> ProjectsResult<Value> {
    if let Some(url) = raw.as_str() {
        return Ok(json!({
            "url": candidate_url(url)?,
            "source": "operator supplied candidate",
            "observed_at": null,
        }));
    }
    let object = raw.as_object().filter(|o| {
        o.keys().map(String::as_str).collect::<BTreeSet<_>>()
            == ["url", "source", "observed_at"].into_iter().collect()
    });
    let Some(object) = object else {
        return Err(crate::error::value(
            "competitor candidates require url, source and observed_at",
        ));
    };
    let source = object["source"]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= 512);
    if source.is_none() {
        return Err(crate::error::value("competitor source is required"));
    }
    Ok(json!({
        "url": candidate_url(object["url"].as_str().unwrap_or_default())?,
        "source": object["source"],
        "observed_at": object["observed_at"],
    }))
}

/// `_competitor_identity`: the core site identity a candidate URL resolves to.
pub(crate) fn competitor_identity(url: &str) -> ProjectsResult<String> {
    workspace_target(url)
}

pub(super) fn competitor_workspaces(
    root: &Path,
    candidates: &[Value],
) -> ProjectsResult<Vec<Value>> {
    let directory_root = root.join("competitors");
    if directory_root.is_symlink() {
        return Err(crate::error::value("unsafe competitor workspace directory"));
    }
    io(fs::create_dir_all(&directory_root))?;
    let mut rows = Vec::new();
    for candidate in candidates {
        let url = candidate["url"].as_str().unwrap_or_default();
        let slug = format!("{:x}", Sha256::digest(url.as_bytes()))[..16].to_owned();
        let child = directory_root.join(&slug);
        if !child.exists() {
            crate::workspace::create_project(
                &child,
                url,
                None,
                Some(&json!([
                    {
                        "name": "candidate_source",
                        "value": candidate["source"],
                        "provenance": "project preparation",
                        "observed_at": candidate["observed_at"],
                    },
                    {
                        "name": "entered_url",
                        "value": candidate["url"],
                        "provenance": "operator-entered competitor URL",
                        "observed_at": candidate["observed_at"],
                    },
                ])),
                None,
                None,
            )?;
        }
        let (_, child_project) = workspace_load(&child)?;
        if child_project["site"]["target"].as_str().unwrap_or_default() != competitor_identity(url)?
        {
            return Err(crate::error::value(
                "competitor workspace site identity mismatch",
            ));
        }
        crate::coverage::initialize_coverage(&child, None)?;
        rows.push(json!({
            "url": candidate["url"],
            "source": candidate["source"],
            "observed_at": candidate["observed_at"],
            "directory": child.strip_prefix(root).unwrap_or(&child).to_string_lossy(),
            "project_uuid": child_project["project_uuid"],
            "state": "candidate; audit not run",
        }));
    }
    Ok(rows)
}

pub(super) fn prepare_lock(root: &Path) -> ProjectsResult<PathBuf> {
    let lock = root.join(".prepare.lock");
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .private_mode(0o600)
        .open(&lock)
    {
        Ok(_) => Ok(lock),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(crate::error::value(
            "project preparation is already running",
        )),
        Err(e) => Err(ProjectsError::Io(e)),
    }
}

pub(super) fn read_preparation(root: &Path, project_uuid: &str) -> ProjectsResult<Option<Value>> {
    let current = read_document(root, "preparation.json")?;
    if let Some(state) = &current {
        if state.get("format").and_then(Value::as_str) != Some(PREPARATION_FORMAT)
            || state.get("project_uuid").and_then(Value::as_str) != Some(project_uuid)
            || !state.get("steps").is_some_and(Value::is_object)
            || !state.get("competitors").is_some_and(Value::is_array)
        {
            return Err(crate::error::value("invalid project preparation state"));
        }
    }
    Ok(current)
}

/// `add_competitors`: record operator-supplied candidates without a preparation crawl.
pub fn add_competitors(directory: &Path, competitors: &Value) -> ProjectsResult<Value> {
    let (root, project) = workspace_load(directory)?;
    let policy = project_policy(directory, None, false, None, None)?;
    let items = competitors.as_array().filter(|i| !i.is_empty());
    let Some(items) = items else {
        return Err(crate::error::value(
            "competitors must be a non-empty list of candidates",
        ));
    };
    let candidates: Vec<Value> = items.iter().map(candidate).collect::<ProjectsResult<_>>()?;
    let identities: Vec<String> = candidates
        .iter()
        .map(|row| competitor_identity(row["url"].as_str().unwrap_or_default()))
        .collect::<ProjectsResult<_>>()?;
    if identities.iter().collect::<BTreeSet<_>>().len() != identities.len() {
        return Err(crate::error::value("duplicate competitor candidates"));
    }
    if identities.contains(
        &project["site"]["target"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
    ) {
        return Err(crate::error::value(
            "the primary site cannot be its own competitor candidate",
        ));
    }
    let lock = prepare_lock(&root)?;
    let result = (|| -> ProjectsResult<Value> {
        let current =
            read_preparation(&root, project["project_uuid"].as_str().unwrap_or_default())?;
        let existing = current
            .as_ref()
            .and_then(|c| c["competitors"].as_array().cloned())
            .unwrap_or_default();
        let known: BTreeSet<String> = existing
            .iter()
            .map(|row| competitor_identity(row["url"].as_str().unwrap_or_default()))
            .collect::<ProjectsResult<_>>()?;
        let mut duplicates: Vec<&str> = candidates
            .iter()
            .zip(&identities)
            .filter(|(_, id)| known.contains(*id))
            .map(|(row, _)| row["url"].as_str().unwrap_or_default())
            .collect();
        duplicates.sort_unstable();
        if let Some(dup) = duplicates.first() {
            return Err(crate::error::value(format!(
                "competitor already recorded: {dup}"
            )));
        }
        let limit = policy["policy"]["competitor_limit"].as_i64().unwrap_or(0);
        if existing.len() as i64 + candidates.len() as i64 > limit {
            return Err(crate::error::value(
                "competitor candidate list exceeds the project limit",
            ));
        }
        let revision = current
            .as_ref()
            .and_then(|c| c["revision"].as_i64())
            .unwrap_or(0);
        let mut state = current.unwrap_or_else(|| {
            json!({
                "format": PREPARATION_FORMAT,
                "project_uuid": project["project_uuid"],
                "revision": 0,
                "started_at": crate::coverage::now(),
                "state": "partial",
                "steps": {},
                "competitors": [],
            })
        });
        let mut list = existing.clone();
        list.extend(competitor_workspaces(&root, &candidates)?);
        state["competitors"] = Value::Array(list.clone());
        state["steps"]["competitors"] = json!({
            "state": "run",
            "reason": "operator-supplied candidate shortlist; competitiveness is not verified",
            "count": list.len(),
        });
        state["revision"] = json!(revision + 1);
        write_document(&root, "preparation.json", &state, Some(revision))?;
        Ok(json!({"ok": true, "competitors": list, "preparation": state}))
    })();
    let _ = fs::remove_file(&lock);
    result
}

/// `list_competitors`: read the recorded competitor candidates.
pub fn list_competitors(directory: &Path) -> ProjectsResult<Value> {
    let status = preparation_status(directory)?;
    Ok(json!({
        "ok": true,
        "competitors": status.get("competitors").cloned().unwrap_or_else(|| json!([])),
    }))
}

/// `remove_competitor`: drop one candidate; its workspace stays on disk untouched.
pub fn remove_competitor(directory: &Path, url: &str) -> ProjectsResult<Value> {
    let (root, _project) = workspace_load(directory)?;
    let target = competitor_identity(&candidate_url(url)?)?;
    let lock = prepare_lock(&root)?;
    let result = (|| -> ProjectsResult<Value> {
        let Some(current) = read_document(&root, "preparation.json")? else {
            return Err(crate::error::value("no competitor candidates are recorded"));
        };
        if !current.get("competitors").is_some_and(Value::is_array) {
            return Err(crate::error::value("no competitor candidates are recorded"));
        }
        if !current.get("steps").is_some_and(Value::is_object) {
            return Err(crate::error::value("invalid project preparation state"));
        }
        let revision = current["revision"].as_i64().unwrap_or(0);
        let competitors = current["competitors"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let kept: Vec<Value> = competitors
            .iter()
            .filter(|row| {
                competitor_identity(row["url"].as_str().unwrap_or_default())
                    .map(|id| id != target)
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        if kept.len() == competitors.len() {
            return Err(crate::error::value(format!(
                "competitor is not recorded: {url}"
            )));
        }
        let removed = competitors
            .iter()
            .find(|row| {
                competitor_identity(row["url"].as_str().unwrap_or_default())
                    .map(|id| id == target)
                    .unwrap_or(false)
            })
            .cloned()
            .ok_or_else(|| crate::error::value(format!("competitor is not recorded: {url}")))?;
        let mut state = current.clone();
        state["competitors"] = Value::Array(kept.clone());
        state["steps"]["competitors"] = json!({
            "state": if kept.is_empty() { "not_run" } else { "run" },
            "reason": if kept.is_empty() {
                "no supplied or authorized competitor source; no competitors invented"
            } else {
                "operator-supplied candidate shortlist; competitiveness is not verified"
            },
            "count": kept.len(),
        });
        state["revision"] = json!(revision + 1);
        write_document(&root, "preparation.json", &state, Some(revision))?;
        Ok(json!({
            "ok": true,
            "removed": removed["url"],
            "workspace_kept": removed.get("directory"),
            "competitors": kept,
        }))
    })();
    let _ = fs::remove_file(&lock);
    result
}

// `_CAP_STOPS`; consumed by `reusable_scan`, wired in when the preparation runner lands.
pub(super) const CAP_STOPS: &[&str] = &["stopped_by_budget", "url_limit"];

pub(super) fn has_saved_audit(path: &Path) -> bool {
    let Ok(con) = spiderhead_store::scan_artifact::open_scan(path, false, None) else {
        return false;
    };
    con.query_row(
        "SELECT EXISTS(SELECT 1 FROM audit WHERE singleton=1)",
        [],
        |row| row.get::<_, i64>(0),
    )
    .map(|v| v == 1)
    .unwrap_or(false)
}

/// `_audited_scans`: finished, audited native scans of this site identity, newest first.
pub(super) fn audited_scans(root: &Path, target: &str) -> ProjectsResult<Vec<Value>> {
    let scans = spiderhead_store::history::list_scans(&root.join("scans"), 0, 1000)?;
    let mut out = Vec::new();
    for row in scans["items"].as_array().cloned().unwrap_or_default() {
        if row["lifecycle"].as_str() != Some("finished") {
            continue;
        }
        let same = row["start_url"]
            .as_str()
            .and_then(|u| workspace_target(u).ok())
            .map(|t| t == target)
            .unwrap_or(false);
        if same
            && (row["audit_bytes"].as_i64().unwrap_or(0) > 0
                || has_saved_audit(Path::new(row["path"].as_str().unwrap_or_default())))
        {
            out.push(row);
        }
    }
    Ok(out)
}

pub(super) fn scan_url_cap(path: &Path) -> Option<i64> {
    let con = spiderhead_store::scan_artifact::open_scan(path, false, None).ok()?;
    let config_text: String = con
        .query_row("SELECT config_json FROM scan WHERE singleton=1", [], |r| {
            r.get(0)
        })
        .ok()?;
    let config: Value = serde_json::from_str(&config_text).ok()?;
    config.pointer("/limits/max_urls")?.as_i64()
}

pub(super) fn is_complete(row: &Value) -> bool {
    row["finish_reason"].as_str() == Some("finished")
        && !row["crawl_partial"].as_bool().unwrap_or(false)
}

/// `latest_project_scan`: newest finished, audited scan of the project's own site.
pub fn latest_project_scan(directory: &Path) -> ProjectsResult<PathBuf> {
    let (root, project) = workspace_load(directory)?;
    let target = project["site"]["target"].as_str().unwrap_or_default();
    for row in audited_scans(&root, target)? {
        if is_complete(&row) || row["finish_reason"].as_str() == Some("stopped_by_budget") {
            return Ok(PathBuf::from(row["path"].as_str().unwrap_or_default()));
        }
    }
    Err(crate::error::value(
        "no finished, audited scan in this project; run project-prepare or a crawl first",
    ))
}

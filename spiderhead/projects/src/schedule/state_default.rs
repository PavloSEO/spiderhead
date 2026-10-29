use super::*;

pub(super) fn state_default(project_uuid: &str) -> Value {
    json!({
        "format": STATE_FORMAT,
        "project_uuid": project_uuid,
        "revision": 0,
        "last_window": null,
        "last_run_at": null,
        "last_result": null,
        "resume_artifact": null,
    })
}

/// Open the project root, project, settings and state together.
pub fn open(directory: &Path) -> ProjectsResult<(PathBuf, Value, Value, Value)> {
    let (root, project) = workspace_load(directory)?;
    let uuid_value = project["project_uuid"].clone();
    let settings = match read_document(&root, NAME)? {
        None => default_settings(uuid_value.as_str().unwrap_or_default()),
        Some(current) => {
            if current.get("format").and_then(Value::as_str) != Some(FORMAT)
                || current.get("project_uuid") != Some(&uuid_value)
            {
                return Err(value("schedule belongs to another project or format"));
            }
            current
        }
    };
    let state = read_document(&root, STATE_NAME)?
        .unwrap_or_else(|| state_default(uuid_value.as_str().unwrap_or_default()));
    if state.get("format").and_then(Value::as_str) != Some(STATE_FORMAT)
        || state.get("project_uuid") != Some(&uuid_value)
    {
        return Err(value("schedule state belongs to another project or format"));
    }
    Ok((root, project, settings, state))
}

pub(super) fn host_of(project: &Value) -> String {
    project["site"]["target"]
        .as_str()
        .and_then(|t| crate::urls::urlsplit(t).ok())
        .and_then(|p| p.hostname())
        .unwrap_or_default()
}

/// Truthiness of JSON values: null, false, zero and empty values are falsy.
pub(crate) fn falsy(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Bool(b) => !*b,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
    }
}

pub(super) fn error_name(error: &ProjectsError) -> &'static str {
    match error {
        ProjectsError::Io(_) => "OSError",
        ProjectsError::Store(_) => "sqlite3.Error",
        _ => "ValueError",
    }
}

/// Declared competitor workspaces that match their saved identity.
pub(super) fn competitor_targets(root: &Path, project: &Value) -> ProjectsResult<Vec<Value>> {
    let prep = read_document(root, "preparation.json")?.unwrap_or(json!({}));
    let rows = prep["competitors"].as_array().cloned().unwrap_or_default();
    if !prep["competitors"].is_array() && !prep["competitors"].is_null() {
        return Err(value("invalid project competitor list"));
    }
    let mut targets = Vec::new();
    let mut seen: BTreeSet<String> = [project["project_uuid"]
        .as_str()
        .unwrap_or_default()
        .to_owned()]
    .into_iter()
    .collect();
    for row in &rows {
        if !row.is_object()
            || !row.get("directory").is_some_and(|d| !d.is_null())
            || row.get("url").is_none()
        {
            continue;
        }
        let Some(relative) = row["directory"].as_str() else {
            continue;
        };
        let path_part = Path::new(relative);
        if path_part.is_absolute()
            || path_part.components().any(|c| c.as_os_str() == "..")
            || !relative.starts_with("competitors/")
        {
            return Err(value("unsafe competitor project reference"));
        }
        let child = root.join(path_part);
        if child.is_symlink() {
            return Err(value("unsafe competitor project reference"));
        }
        let resolved_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let resolved_child = child.canonicalize().unwrap_or(child.clone());
        if !resolved_child.starts_with(&resolved_root) {
            return Err(value("unsafe competitor project reference"));
        }
        let (child_root, child_project) = workspace_load(&child)?;
        if child_root != resolved_child
            || child_project["project_uuid"] != row["project_uuid"]
            || child_project["site"]["target"].as_str()
                != competitor_identity(row["url"].as_str().unwrap_or_default())
                    .ok()
                    .as_deref()
            || seen.contains(child_project["project_uuid"].as_str().unwrap_or_default())
        {
            return Err(value("competitor project identity mismatch"));
        }
        seen.insert(
            child_project["project_uuid"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        );
        targets.push(json!({
            "role": "competitor",
            "directory": child_root.to_string_lossy(),
            "directory_relative": relative,
            "url": row["url"],
            "project_uuid": child_project["project_uuid"],
            "root": child_root.to_string_lossy(),
            "project": child_project,
        }));
    }
    Ok(targets)
}

/// Show the effective schedule settings and state.
pub fn show(directory: &Path, now: Option<&Zoned>) -> ProjectsResult<Value> {
    let (root, project, settings, state) = open(directory)?;
    let stamp = match now {
        Some(z) => z.clone(),
        None => crate::schedule::now(),
    };
    let host = host_of(&project);
    let mut competitors = Vec::new();
    if settings["include_competitors"].as_bool() == Some(true) {
        for target in competitor_targets(&root, &project)? {
            let dir = PathBuf::from(target["directory"].as_str().unwrap_or_default());
            let (child_root, child_project, child_settings, child_state) = open(&dir)?;
            let child_host = host_of(&child_project);
            let configured = read_document(&child_root, NAME)?.is_some();
            let decision = due_state(
                &child_settings,
                &child_host,
                &stamp,
                child_state.get("last_window").and_then(Value::as_str),
            )?;
            competitors.push(json!({
                "url": target["url"],
                "directory": target["directory_relative"],
                "schedule_configured": configured,
                "revision": child_settings["revision"],
                "enabled": child_settings["enabled"],
                "due": decision,
                "next_due_at": next_due(&child_settings, &child_host, &stamp)?,
            }));
        }
    }
    Ok(json!({
        "ok": true,
        "revision": settings["revision"],
        "settings": settings,
        "state": {
            "last_window": state.get("last_window").cloned().unwrap_or(Value::Null),
            "last_run_at": state.get("last_run_at").cloned().unwrap_or(Value::Null),
            "last_result": state.get("last_result").cloned().unwrap_or(Value::Null),
        },
        "resume_artifact": state.get("resume_artifact").cloned().unwrap_or(Value::Null),
        "due": due_state(&settings, &host, &stamp, state.get("last_window").and_then(Value::as_str))?,
        "next_due_at": next_due(&settings, &host, &stamp)?,
        "competitors": competitors,
    }))
}

/// Merge a partial update into settings and publish the next revision.
pub fn configure(
    directory: &Path,
    expected_revision: i64,
    settings: Option<&Value>,
    fields: &Map<String, Value>,
) -> ProjectsResult<Value> {
    let mut patch = Map::new();
    if let Some(s) = settings.and_then(Value::as_object) {
        patch.extend(s.clone());
    }
    patch.extend(fields.clone());
    for managed in ["revision", "project_uuid", "format"] {
        if patch.contains_key(managed) {
            return Err(value(
                "revision, project_uuid and format are managed by the runtime",
            ));
        }
    }
    let (root, project, current, _state) = open(directory)?;
    if current["revision"].as_i64() != Some(expected_revision) {
        return Err(value("schedule revision conflict"));
    }
    let mut merged = current.as_object().unwrap().clone();
    for (k, v) in &patch {
        merged.insert(k.clone(), v.clone());
    }
    for nested in ["crawl", "window", "user_agent"] {
        let mut base = current[nested].as_object().cloned().unwrap_or_default();
        if let Some(update) = patch.get(nested).and_then(Value::as_object) {
            base.extend(update.clone());
        }
        merged.insert(nested.to_owned(), Value::Object(base));
    }
    merged.insert("format".into(), json!(FORMAT));
    merged.insert("project_uuid".into(), project["project_uuid"].clone());
    merged.insert(
        "revision".into(),
        json!(current["revision"].as_i64().unwrap_or(0) + 1),
    );
    let normalized = validate(&Value::Object(merged))?;
    write_document(&root, NAME, &normalized, Some(expected_revision))?;
    crate::journal::note(
        directory,
        "app",
        "schedule",
        &format!(
            "schedule revision {} saved",
            normalized["revision"].as_i64().unwrap_or(0)
        ),
    );
    Ok(json!({
        "ok": true,
        "revision": normalized["revision"],
        "settings": normalized,
    }))
}

pub(super) fn list_urls(root: &Path, settings: &Value) -> ProjectsResult<Vec<String>> {
    let urls = settings["crawl"]["urls"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if !urls.is_empty() {
        return Ok(urls
            .iter()
            .filter_map(|u| u.as_str().map(str::to_owned))
            .collect());
    }
    let monitor = read_document(root, "monitor.json")?.unwrap_or(json!({}));
    Ok(monitor["policy"]["urls"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|u| u.as_str().map(str::to_owned))
        .collect())
}

/// Keep project-owned scan references portable inside state.
pub(super) fn stored_scan_path(root: &Path, raw_value: &Value) -> Value {
    let Some(raw) = raw_value.as_str() else {
        return raw_value.clone();
    };
    if raw.is_empty() {
        return raw_value.clone();
    }
    let path = Path::new(raw);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let resolved_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let resolved = path.canonicalize().unwrap_or(path);
    match resolved.strip_prefix(&resolved_root) {
        Ok(rel) => json!(rel.to_string_lossy().replace('\\', "/")),
        Err(_) => raw_value.clone(),
    }
}

pub(super) fn windows_parts(value: &str) -> Vec<String> {
    value
        .split(['/', '\\'])
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect()
}

pub(super) fn is_windows_absolute(value: &str) -> bool {
    let bytes = value.as_bytes();
    (bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'/' || bytes[2] == b'\\'))
        || value.starts_with("\\\\")
}

/// Resolve a relative scan path in state; rebase legacy absolute refs.
pub(super) fn resume_scan_path(root: &Path, raw_value: &Value) -> ProjectsResult<Option<String>> {
    let Some(raw) = raw_value.as_str() else {
        return Ok(None);
    };
    if raw.is_empty() {
        return Ok(None);
    }
    let path = Path::new(raw);
    let scans_root = root
        .join("scans")
        .canonicalize()
        .unwrap_or_else(|_| root.join("scans"));
    if !path.is_absolute() && !is_windows_absolute(raw) {
        let candidate = root
            .join(path)
            .canonicalize()
            .unwrap_or_else(|_| root.join(path));
        if !candidate.starts_with(&scans_root) {
            return Err(value(
                "scheduled resume artifact must be inside project scans",
            ));
        }
        return Ok(Some(candidate.to_string_lossy().to_string()));
    }
    let parts: Vec<String> = if path.is_absolute() {
        let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if let Ok(rel) = resolved.strip_prefix(&scans_root) {
            let _ = rel;
            return Ok(Some(resolved.to_string_lossy().to_string()));
        }
        path.components()
            .filter_map(|c| c.as_os_str().to_str().map(str::to_owned))
            .collect()
    } else {
        windows_parts(raw)
    };
    let indexes: Vec<usize> = parts
        .iter()
        .enumerate()
        .filter(|(_, part)| part.to_lowercase() == "scans")
        .map(|(i, _)| i)
        .collect();
    if let Some(&index) = indexes.last() {
        let relative = &parts[index + 1..];
        if !relative.is_empty() && !relative.iter().any(|p| p == "..") {
            let rebased = root
                .join("scans")
                .join(relative.join(std::path::MAIN_SEPARATOR_STR));
            let resolved = rebased.canonicalize().unwrap_or(rebased.clone());
            if resolved.starts_with(&scans_root) && rebased.is_file() && !rebased.is_symlink() {
                return Ok(Some(rebased.to_string_lossy().to_string()));
            }
        }
    }
    Err(value(
        "scheduled resume artifact cannot be resolved inside project scans",
    ))
}

pub(super) fn request(
    settings: &Value,
    target: &Value,
    state: &Value,
    root_dir: &Path,
) -> ProjectsResult<Value> {
    let crawl = &settings["crawl"];
    let agent = &settings["user_agent"];
    let user_agent = if !agent["custom"].is_null() {
        agent["custom"].clone()
    } else {
        agent["preset"].clone()
    };
    let artifact = if crawl["slice"].as_bool() == Some(true) {
        resume_scan_path(root_dir, &state["resume_artifact"])?
    } else {
        None
    };
    if artifact.is_some() && crawl["mode"].as_str() == Some("spider") {
        return Ok(json!({
            "resume": artifact,
            "project": target["directory"],
            "approve_large_crawl": false,
        }));
    }
    let base = json!({
        "project": target["directory"],
        "user_agent": user_agent,
        "max_urls": crawl["max_urls"],
        "overrides": {"limits.max_requests": crawl["max_requests"]},
        "min_delay": crawl["min_delay_seconds"].as_f64().unwrap_or(0.0),
        "approve_large_crawl": false,
    });
    match crawl["mode"].as_str() {
        Some("spider") => {
            let mut out = base.as_object().unwrap().clone();
            out.insert("url".into(), target["url"].clone());
            Ok(Value::Object(out))
        }
        Some("sitemap") => {
            let mut out = base.as_object().unwrap().clone();
            out.insert("url".into(), crawl["sitemap_url"].clone());
            out.insert("sitemap".into(), crawl["sitemap_url"].clone());
            out.insert("sitemap_only".into(), json!(true));
            Ok(Value::Object(out))
        }
        _ => {
            let urls = list_urls(root_dir, settings)?;
            if urls.is_empty() {
                return Err(value(
                    "list mode has no URLs: set crawl.urls or monitor.json policy urls",
                ));
            }
            let mut out = base.as_object().unwrap().clone();
            out.insert("urls".into(), json!(urls));
            Ok(Value::Object(out))
        }
    }
}

/// Hold a process-scoped OS lock for the complete run.
pub(super) fn acquire(root: &Path) -> Option<std::fs::File> {
    let lock = root.join(LOCK_NAME);
    if lock.is_symlink() {
        return None;
    }
    let file = crate::locks::open_lock(&lock, true).ok()?;
    use spiderhead_core::filesystem::StatCompat;
    let mode = file.metadata().ok()?.mode();
    if !file.metadata().ok()?.is_file() || mode & 0o077 != 0 {
        return None;
    }
    if crate::locks::lock_exclusive(&file).is_err() {
        return None;
    }
    Some(file)
}

/// Whether a scheduler owns this project's persistent OS lock.
pub fn active(root: &Path) -> ProjectsResult<bool> {
    let path = root.join(LOCK_NAME);
    if !path.exists() && !path.is_symlink() {
        return Ok(false);
    }
    let file = crate::locks::open_lock(&path, false)
        .map_err(|_| value("schedule lock is unsafe or unavailable"))?;
    match crate::locks::lock_exclusive(&file) {
        Err(_) => Ok(true),
        Ok(()) => {
            let _ = crate::locks::unlock(&file);
            Ok(false)
        }
    }
}

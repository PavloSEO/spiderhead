use super::*;

/// `checked_pattern`: validate one operator scope-narrowing pattern (#1485).
///
/// Missing API: `spiderhead-crawl` has no `spider::checked_pattern` port yet — the
/// bounds and the compile check are reproduced here (fancy-regex, closest to `re`).
pub fn checked_pattern(pattern: &str) -> ProjectsResult<String> {
    if pattern.trim().is_empty() {
        return Err(value("scope pattern must be nonempty text"));
    }
    if pattern.chars().count() > 512 {
        return Err(value("scope pattern must stay under 512 characters"));
    }
    fancy_regex::Regex::new(pattern).map_err(|e| {
        value(format!(
            "scope pattern is not a valid regular expression: {e}"
        ))
    })?;
    Ok(pattern.to_owned())
}

/// `request_scope_narrow`: queue a live scope narrowing (narrowing only) (#1485).
pub fn request_scope_narrow(
    directory: &Path,
    run_id: &str,
    exclude: Option<&str>,
    include: Option<&str>,
) -> ProjectsResult<Value> {
    if exclude.is_none() && include.is_none() {
        return Err(value("narrow-scope requires exclude or include"));
    }
    let exclude = exclude.map(checked_pattern).transpose()?;
    let include = include.map(checked_pattern).transpose()?;
    let root = active_root(directory, run_id)?;
    let con = connect(&root)?;
    con.execute(
        "INSERT INTO scopes(run_id,exclude_pattern,include_pattern,requested_at,state) \
         VALUES(?1,?2,?3,?4,'pending') ON CONFLICT(run_id) DO UPDATE SET \
         exclude_pattern=excluded.exclude_pattern, include_pattern=excluded.include_pattern, \
         requested_at=excluded.requested_at, state='pending'",
        rusqlite::params![run_id, exclude, include, stamp()],
    )
    .map_err(|e| value(e.to_string()))?;
    Ok(json!({
        "ok": true,
        "run_id": run_id,
        "state": "narrow_requested",
        "exclude": exclude,
        "include": include,
    }))
}

/// `consume_scope`: consume the pending scope narrowing for `run_id`, if any.
pub fn consume_scope(directory: &Path, run_id: &str) -> ProjectsResult<Option<Value>> {
    let root = project_root(directory)?;
    if !root.join(NAME).exists() {
        return Ok(None);
    }
    let con = connect(&root)?;
    con.execute_batch("BEGIN IMMEDIATE")
        .map_err(|e| value(e.to_string()))?;
    let result = (|| -> ProjectsResult<Option<Value>> {
        let row: Option<(Option<String>, Option<String>)> = con
            .query_row(
                "SELECT exclude_pattern, include_pattern FROM scopes \
                 WHERE run_id=?1 AND state='pending'",
                rusqlite::params![run_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok();
        if row.is_some() {
            con.execute(
                "UPDATE scopes SET state='applied' WHERE run_id=?1",
                rusqlite::params![run_id],
            )
            .map_err(|e| value(e.to_string()))?;
        }
        Ok(row.map(|(exclude, include)| json!({"exclude": exclude, "include": include})))
    })();
    match result {
        Ok(row) => {
            con.execute_batch("COMMIT")
                .map_err(|e| value(e.to_string()))?;
            Ok(row)
        }
        Err(e) => {
            let _ = con.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// `request_urls`: queue operator URLs for the live crawl; pending URLs merge (#1485).
pub fn request_urls(directory: &Path, run_id: &str, urls: &Value) -> ProjectsResult<Value> {
    let cleaned = checked_operator_urls(urls)?;
    let root = active_root(directory, run_id)?;
    let con = connect(&root)?;
    con.execute_batch("BEGIN IMMEDIATE")
        .map_err(|e| value(e.to_string()))?;
    let result = (|| -> ProjectsResult<usize> {
        let row: Option<String> = con
            .query_row(
                "SELECT urls_json FROM urls WHERE run_id=?1 AND state='pending'",
                rusqlite::params![run_id],
                |row| row.get(0),
            )
            .ok();
        let prior: Vec<String> = row
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        let mut merged = prior;
        for url in &cleaned {
            if !merged.contains(url) {
                merged.push(url.clone());
            }
        }
        if merged.len() > MAX_STEER_URLS {
            return Err(value(format!(
                "a pending add-urls request is not yet applied; \
                 at most {MAX_STEER_URLS} URLs may wait per run"
            )));
        }
        con.execute(
            "INSERT INTO urls(run_id,urls_json,requested_at,state) VALUES(?1,?2,?3,'pending') \
             ON CONFLICT(run_id) DO UPDATE SET urls_json=excluded.urls_json, \
             requested_at=excluded.requested_at, state='pending'",
            rusqlite::params![
                run_id,
                serde_json::to_string(&merged).unwrap_or_default(),
                stamp()
            ],
        )
        .map_err(|e| value(e.to_string()))?;
        Ok(cleaned.len())
    })();
    match result {
        Ok(count) => {
            con.execute_batch("COMMIT")
                .map_err(|e| value(e.to_string()))?;
            Ok(json!({"ok": true, "run_id": run_id, "state": "urls_requested", "count": count}))
        }
        Err(e) => {
            let _ = con.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// `consume_urls`: consume the pending operator URL injection for `run_id`, if any.
pub fn consume_urls(directory: &Path, run_id: &str) -> ProjectsResult<Option<Vec<String>>> {
    let root = project_root(directory)?;
    if !root.join(NAME).exists() {
        return Ok(None);
    }
    let con = connect(&root)?;
    con.execute_batch("BEGIN IMMEDIATE")
        .map_err(|e| value(e.to_string()))?;
    let result = (|| -> ProjectsResult<Option<Vec<String>>> {
        let row: Option<String> = con
            .query_row(
                "SELECT urls_json FROM urls WHERE run_id=?1 AND state='pending'",
                rusqlite::params![run_id],
                |row| row.get(0),
            )
            .ok();
        if row.is_some() {
            con.execute(
                "UPDATE urls SET state='applied' WHERE run_id=?1",
                rusqlite::params![run_id],
            )
            .map_err(|e| value(e.to_string()))?;
        }
        Ok(row.and_then(|raw| serde_json::from_str(&raw).ok()))
    })();
    match result {
        Ok(row) => {
            con.execute_batch("COMMIT")
                .map_err(|e| value(e.to_string()))?;
            Ok(row)
        }
        Err(e) => {
            let _ = con.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// `request`: queue a one-shot pause/stop at the next durable crawl boundary.
pub fn request(directory: &Path, run_id: &str, action: &str) -> ProjectsResult<Value> {
    if !["pause", "stop"].contains(&action) {
        return Err(value("action must be pause or stop"));
    }
    let root = active_root(directory, run_id)?;
    let stamp = stamp();
    let con = connect(&root)?;
    con.execute_batch("BEGIN IMMEDIATE")
        .map_err(|e| value(e.to_string()))?;
    let result = (|| -> ProjectsResult<()> {
        let existing: Option<String> = con
            .query_row(
                "SELECT state FROM requests WHERE run_id=?1",
                rusqlite::params![run_id],
                |row| row.get(0),
            )
            .ok();
        if let Some(state) = existing {
            if state == "pending" {
                return Err(value("a control request is already pending for this run"));
            }
            return Err(value("a control request was already applied for this run"));
        }
        con.execute(
            "INSERT INTO requests(run_id,action,requested_at) VALUES(?1,?2,?3)",
            rusqlite::params![run_id, action, stamp],
        )
        .map_err(|e| value(e.to_string()))?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            con.execute_batch("COMMIT")
                .map_err(|e| value(e.to_string()))?;
        }
        Err(e) => {
            let _ = con.execute_batch("ROLLBACK");
            return Err(e);
        }
    }
    let latest = crate::run_observation::status(&root, crate::run_observation::MAX_RUNS as i64)?;
    let active = latest["items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|item| item["id"].as_str() == Some(run_id));
    let live = active.as_ref().is_some_and(|a| {
        a["state"].as_str() == Some("running") && a["pid_state"].as_str() == Some("live")
    });
    if !live {
        if inactive_request_receipt(&root, run_id, action)? {
            return Ok(json!({
                "ok": true,
                "run_id": run_id,
                "state": format!("{action}_applied"),
            }));
        }
        return Err(value("run_id is no longer an active native crawl"));
    }
    Ok(json!({
        "ok": true,
        "run_id": run_id,
        "state": format!("{action}_requested"),
    }))
}

/// `consume`: consume one pending action at progress boundaries.
pub fn consume(directory: &Path, run_id: &str) -> ProjectsResult<Option<String>> {
    let root = project_root(directory)?;
    if !root.join(NAME).exists() {
        return Ok(None);
    }
    let con = connect(&root)?;
    con.execute_batch("BEGIN IMMEDIATE")
        .map_err(|e| value(e.to_string()))?;
    let result = (|| -> ProjectsResult<Option<String>> {
        let row: Option<(String, String)> = con
            .query_row(
                "SELECT action, state FROM requests WHERE run_id=?1",
                rusqlite::params![run_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok();
        let Some((action, state)) = row else {
            return Ok(None);
        };
        if state != "pending" {
            return Ok(None);
        }
        con.execute(
            "UPDATE requests SET state='applied' WHERE run_id=?1 AND state='pending'",
            rusqlite::params![run_id],
        )
        .map_err(|e| value(e.to_string()))?;
        Ok(Some(action))
    })();
    match result {
        Ok(action) => {
            con.execute_batch("COMMIT")
                .map_err(|e| value(e.to_string()))?;
            Ok(action)
        }
        Err(e) => {
            let _ = con.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

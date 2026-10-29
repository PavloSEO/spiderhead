use super::*;

/// The failure kind a `scan` callable may report.
#[derive(Debug)]
pub struct ScanFailure {
    /// KeyboardInterrupt/SystemExit/CancelledError equivalent — re-raised after state write.
    pub cancelled: bool,
    /// `type(exc).__name__` for the failure.
    pub name: String,
    /// `str(exc)` when the failure is a `ValueError`-equivalent.
    pub message: Option<String>,
}

/// `run_due`: run scheduled scans for one project if they are due. Safe to call often.
///
/// `scan` receives the crawl keyword object and returns the outcome dict.
pub fn run_due(
    directory: &Path,
    scan: &dyn Fn(&Value) -> Result<Value, ScanFailure>,
    now: Option<&Zoned>,
) -> ProjectsResult<Value> {
    let stamp = now.cloned().unwrap_or_else(crate::schedule::now);
    let (root, project, settings, state) = open(directory)?;
    let configured = read_document(&root, NAME)?.is_some();
    let mut candidates: Vec<Value> = Vec::new();
    let mut unavailable: Vec<Value> = Vec::new();
    let primary_decision;
    if configured {
        let host = host_of(&project);
        primary_decision = due_state(
            &settings,
            &host,
            &stamp,
            state.get("last_window").and_then(Value::as_str),
        )?;
        if primary_decision["due"].as_bool() == Some(true) {
            candidates.push(json!({
                "role": "primary",
                "directory": root.to_string_lossy(),
                "url": project["site"]["target"],
                "root": root.to_string_lossy(),
                "project": project,
                "settings": settings,
                "state": state,
                "decision": primary_decision,
            }));
        }
    } else {
        primary_decision = json!({"due": false, "reason": "no schedule configured"});
    }
    if settings["include_competitors"].as_bool() == Some(true) {
        for target in competitor_targets(&root, &project)? {
            let dir = PathBuf::from(target["directory"].as_str().unwrap_or_default());
            let opened = (|| {
                let (child_root, child_project, child_settings, child_state) = open(&dir)?;
                if read_document(&child_root, NAME)?.is_none() {
                    return Ok(None);
                }
                let host = host_of(&child_project);
                let decision = due_state(
                    &child_settings,
                    &host,
                    &stamp,
                    child_state.get("last_window").and_then(Value::as_str),
                )?;
                Ok(Some((
                    child_root,
                    child_project,
                    child_settings,
                    child_state,
                    decision,
                )))
            })();
            let Some((child_root, _child_project, child_settings, child_state, decision)) =
                (match opened {
                    Ok(v) => v,
                    Err(exc) => {
                        unavailable.push(json!({
                            "role": "competitor",
                            "url": target["url"],
                            "ok": false,
                            "error": error_name(&exc),
                        }));
                        continue;
                    }
                })
            else {
                continue;
            };
            let _ = child_root;
            if decision["due"].as_bool() == Some(true) {
                let mut item = target.clone();
                item["settings"] = child_settings;
                item["state"] = child_state;
                item["decision"] = decision;
                candidates.push(item);
            }
        }
    }
    if candidates.is_empty() {
        if !unavailable.is_empty() {
            for item in &unavailable {
                crate::journal::note(
                    directory,
                    "scans",
                    "schedule",
                    &format!(
                        "competitor {} schedule unavailable: {}",
                        item["url"].as_str().unwrap_or_default(),
                        item["error"].as_str().unwrap_or_default()
                    ),
                );
            }
            return Ok(json!({
                "ok": false,
                "ran": false,
                "reason": "one or more competitor schedules are unavailable",
                "competitors_due": 0,
                "runs": unavailable,
            }));
        }
        return Ok(json!({
            "ok": true,
            "ran": false,
            "reason": primary_decision["reason"],
            "decision": primary_decision,
            "competitors_due": 0,
        }));
    }
    let token = {
        let _guard = crate::runtime::project_write_lock(&root, 5.0)?;
        let (current_root, current_project, _, _) = open(directory)?;
        if current_root != root || current_project["project_uuid"] != project["project_uuid"] {
            return Err(value("project changed before scheduled-run admission"));
        }
        acquire(&root)
    };
    let Some(token) = token else {
        crate::journal::note(
            directory,
            "scans",
            "schedule",
            "skipped: a scheduled run already holds the lock",
        );
        return Ok(json!({"ok": true, "ran": false, "reason": "overlap refused"}));
    };
    let _token = token;
    let mut results: Vec<Value> = unavailable;
    for item in &results.clone() {
        crate::journal::note(
            directory,
            "scans",
            "schedule",
            &format!(
                "competitor {} schedule unavailable: {}",
                item["url"].as_str().unwrap_or_default(),
                item["error"].as_str().unwrap_or_default()
            ),
        );
    }
    for target in &candidates {
        let target_root = PathBuf::from(target["root"].as_str().unwrap_or_default());
        let target_settings = target["settings"].clone();
        let mut target_state = target["state"].clone();
        let decision = target["decision"].clone();
        let mut child_token = None;
        if target_root != root {
            {
                let _guard = crate::runtime::project_write_lock(&target_root, 5.0)?;
                let dir = PathBuf::from(target["directory"].as_str().unwrap_or_default());
                let (child_root, child_project, _, _) = open(&dir)?;
                if child_root != target_root
                    || child_project["project_uuid"] != target["project_uuid"]
                {
                    return Err(value(
                        "competitor project changed before scheduled-run admission",
                    ));
                }
                child_token = acquire(&target_root);
            }
        }
        if target_root != root && child_token.is_none() {
            let entry = json!({
                "role": target["role"],
                "url": target["url"],
                "ok": false,
                "scan": null,
                "partial": false,
                "finish_reason": null,
                "error": "overlap refused",
                "window": decision["window"],
            });
            results.push(entry);
            crate::journal::note(
                directory,
                "scans",
                "schedule",
                &format!(
                    "competitor {} skipped: another scheduled run holds its lock",
                    target["url"].as_str().unwrap_or_default()
                ),
            );
            crate::journal::note(
                Path::new(target["directory"].as_str().unwrap_or_default()),
                "scans",
                "schedule",
                "competitor scan skipped: another scheduled run holds its lock",
            );
            continue;
        }
        crate::journal::note(
            directory,
            "scans",
            "schedule",
            &format!(
                "started {} window {} mode {}",
                target["role"].as_str().unwrap_or_default(),
                decision["window"].as_str().unwrap_or_default(),
                target_settings["crawl"]["mode"]
                    .as_str()
                    .unwrap_or_default()
            ),
        );
        if target_root != root {
            crate::journal::note(
                Path::new(target["directory"].as_str().unwrap_or_default()),
                "scans",
                "schedule",
                &format!(
                    "started competitor scan window {} mode {}",
                    decision["window"].as_str().unwrap_or_default(),
                    target_settings["crawl"]["mode"]
                        .as_str()
                        .unwrap_or_default()
                ),
            );
        }
        let kwargs = request(&target_settings, target, &target_state, &target_root);
        let outcome: Result<Value, ScanFailure> = match kwargs {
            Err(exc) => Err(ScanFailure {
                cancelled: false,
                name: error_name(&exc).to_owned(),
                message: Some(exc.to_string()),
            }),
            Ok(kwargs) => scan(&kwargs),
        };
        let outcome = match outcome {
            Err(failure) if failure.cancelled => {
                let entry = json!({
                    "role": target["role"],
                    "url": target["url"],
                    "ok": false,
                    "scan": null,
                    "partial": false,
                    "finish_reason": "cancelled",
                    "error": failure.name,
                    "cancelled": true,
                    "window": decision["window"],
                });
                target_state["last_window"] = decision["window"].clone();
                target_state["last_run_at"] = json!(crate::coverage::now());
                target_state["last_result"] = json!([entry]);
                let previous = target_state["revision"].as_i64().unwrap_or(0);
                target_state["revision"] = json!(previous + 1);
                write_document(&target_root, STATE_NAME, &target_state, Some(previous))?;
                let message = format!(
                    "{} cancelled: {}",
                    target["role"].as_str().unwrap_or_default(),
                    failure.name
                );
                crate::journal::note(directory, "scans", "schedule", &message);
                if target_root != root {
                    crate::journal::note(
                        Path::new(target["directory"].as_str().unwrap_or_default()),
                        "scans",
                        "schedule",
                        &message,
                    );
                }
                drop(child_token);
                drop(_token);
                return Err(value(format!("cancelled: {}", failure.name)));
            }
            Err(failure) => json!({
                "ok": false,
                "error": failure.message.unwrap_or(failure.name),
            }),
            Ok(outcome) => outcome,
        };
        let outcome_ok = outcome.get("ok").is_none_or(|v| !falsy(v));
        let entry = json!({
            "role": target["role"],
            "url": target["url"],
            "ok": outcome_ok,
            "scan": outcome.get("scan").cloned().unwrap_or(Value::Null),
            "partial": outcome.get("partial").is_some_and(|v| !falsy(v)),
            "finish_reason": outcome.get("finish_reason").cloned().unwrap_or(Value::Null),
            "error": if outcome_ok { Value::Null } else { outcome.get("error").cloned().unwrap_or(Value::Null) },
            "window": decision["window"],
        });
        results.push(entry.clone());
        if target_settings["crawl"]["slice"].as_bool() == Some(true)
            && !falsy(&entry["partial"])
            && !entry["scan"].is_null()
        {
            target_state["resume_artifact"] = stored_scan_path(&target_root, &entry["scan"]);
        } else {
            target_state["resume_artifact"] = Value::Null;
        }
        target_state["last_window"] = decision["window"].clone();
        target_state["last_run_at"] = json!(crate::coverage::now());
        let mut stored_entry = entry.clone();
        stored_entry["scan"] = stored_scan_path(&target_root, &entry["scan"]);
        target_state["last_result"] = json!([stored_entry]);
        let previous = target_state["revision"].as_i64().unwrap_or(0);
        target_state["revision"] = json!(previous + 1);
        write_document(&target_root, STATE_NAME, &target_state, Some(previous))?;
        let ok_flag = !falsy(&entry["ok"]);
        crate::journal::note(
            directory,
            "scans",
            "schedule",
            &format!(
                "{} {}{}",
                target["role"].as_str().unwrap_or_default(),
                if ok_flag { "finished" } else { "failed" },
                if ok_flag {
                    format!(" partial={}", !falsy(&entry["partial"]))
                } else {
                    format!(" error={}", entry["error"].as_str().unwrap_or_default())
                }
            ),
        );
        if target_root != root {
            crate::journal::note(
                Path::new(target["directory"].as_str().unwrap_or_default()),
                "scans",
                "schedule",
                &format!(
                    "competitor {}{}",
                    if ok_flag { "finished" } else { "failed" },
                    if ok_flag {
                        format!(" partial={}", !falsy(&entry["partial"]))
                    } else {
                        format!(" error={}", entry["error"].as_str().unwrap_or_default())
                    }
                ),
            );
        }
        drop(child_token);
    }
    drop(_token);
    let competitors_due = candidates
        .iter()
        .filter(|c| c["role"].as_str() == Some("competitor"))
        .count() as i64;
    Ok(json!({
        "ok": results.iter().all(|item| !falsy(&item["ok"])),
        "ran": true,
        "window": match primary_decision.get("window") {
            Some(w) if !falsy(w) => w.clone(),
            _ => candidates[0]["decision"]["window"].clone(),
        },
        "competitors_due": competitors_due,
        "runs": results,
    }))
}

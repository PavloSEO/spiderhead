use super::*;

/// `list_proposals`: newest-first page over the proposal queue.
pub fn list_proposals(directory: &Path, offset: i64, limit: i64) -> ProjectsResult<Value> {
    if offset < 0 || !(1..=100).contains(&limit) {
        return Err(crate::error::value(
            "offset must be nonnegative and limit must be 1..100",
        ));
    }
    let (_root, _project, current) = proposal_document(directory)?;
    let proposals = current
        .as_ref()
        .map(|c| c["proposals"].as_array().cloned().unwrap_or_default())
        .unwrap_or_default();
    let total = proposals.len() as i64;
    let items: Vec<Value> = proposals
        .iter()
        .rev()
        .skip(offset as usize)
        .take(limit as usize)
        .cloned()
        .collect();
    Ok(json!({
        "ok": true,
        "revision": current.as_ref().map_or(0, |c| c["revision"].as_i64().unwrap_or(0)),
        "total": total,
        "items": items,
        "pagination": {
            "total": total,
            "offset": offset,
            "limit": limit,
            "next_offset": if offset + limit < total { json!(offset + limit) } else { Value::Null },
        },
    }))
}

pub(super) fn review_detail(status: &str, kind: &str) -> String {
    let verb = match status {
        "applied" => "approved",
        "denied" => "denied",
        _ => "marked stale",
    };
    format!("Owner {verb} {kind} proposal.")
}

/// `review_proposal`: apply or deny one pending proposal through the owner workflow.
pub fn review_proposal(
    directory: &Path,
    proposal_id: &str,
    decision: &str,
    expected_revision: i64,
    client: &str,
) -> ProjectsResult<Value> {
    let (root, _project) = workspace_load(directory)?;
    let _guard = project_write_lock(&root, 5.0)?;
    if !crate::action_log::CLIENTS.contains(&client) {
        return Err(crate::error::value("client must be a supported interface"));
    }
    if !["approve", "deny"].contains(&decision) {
        return Err(crate::error::value("decision must be approve or deny"));
    }
    if expected_revision < 0 {
        return Err(crate::error::value("expected_revision must be nonnegative"));
    }
    let (root, project, current) = proposal_document(directory)?;
    let mut revision = current
        .as_ref()
        .map_or(0, |c| c["revision"].as_i64().unwrap_or(0));
    let mut proposals = current
        .as_ref()
        .map(|c| c["proposals"].as_array().cloned().unwrap_or_default())
        .unwrap_or_default();
    let index = proposals
        .iter()
        .position(|item| item["id"].as_str() == Some(proposal_id))
        .ok_or_else(|| crate::error::value("settings proposal does not exist"))?;
    let mut proposal = proposals[index].clone();
    if revision != expected_revision
        && !(matches!(
            proposal["status"].as_str(),
            Some("applying" | "applied" | "denied" | "stale")
        ) && proposal["decision"].as_str() == Some(decision))
    {
        return Err(crate::error::value(format!(
            "proposal revision conflict: current revision is {revision}"
        )));
    }
    if matches!(
        proposal["status"].as_str(),
        Some("applied" | "denied" | "stale")
    ) {
        if proposal["decision"].as_str() != Some(decision) {
            return Err(crate::error::value(
                "settings proposal already has a different terminal decision",
            ));
        }
        let mut result = json!({"ok": true, "revision": revision, "proposal": proposal});
        if let Some(sequence) = proposal["action_sequence"].as_i64() {
            if crate::action_log::resolve_pending_root(
                &root,
                sequence,
                proposal["status"].as_str().unwrap_or_default(),
                &review_detail(
                    proposal["status"].as_str().unwrap_or_default(),
                    proposal["kind"].as_str().unwrap_or_default(),
                ),
                Some(proposal_id),
                None,
            )
            .is_err()
            {
                result["audit_warning"] =
                    json!("Proposal status is saved; its action row remains pending.");
            }
        }
        return Ok(result);
    }
    let kind = proposal["kind"].as_str().unwrap_or_default().to_owned();
    let changes = proposal["changes"].clone();
    let (current_revision, current_value, current_operation, desired_value) =
        if kind == "project_settings" {
            let settings = read_settings(&root)?;
            (
                settings["revision"].as_i64().unwrap_or(0),
                settings["settings"]["label"].clone(),
                settings["operation_id"].clone(),
                changes["label"].clone(),
            )
        } else {
            let policy = project_policy(&root, None, false, None, None)?;
            (
                policy["revision"].as_i64().unwrap_or(0),
                policy["policy"].clone(),
                policy["operation_id"].clone(),
                changes["crawl_policy"].clone(),
            )
        };

    let status: &str = if current_operation.as_str() == Some(proposal_id) {
        "applied"
    } else if decision == "deny" {
        "denied"
    } else if current_value == desired_value
        && current_revision >= proposal["base_revision"].as_i64().unwrap_or(0)
    {
        "applied"
    } else if current_revision != proposal["base_revision"].as_i64().unwrap_or(0) {
        "stale"
    } else {
        if proposal["status"].as_str() != Some("applying") {
            proposal["status"] = json!("applying");
            proposal["decision"] = json!("approve");
            proposals[index] = proposal.clone();
            let marker = json!({
                "format": PROPOSALS_FORMAT,
                "version": 1,
                "project_uuid": project["project_uuid"],
                "revision": revision + 1,
                "proposals": proposals,
            });
            write_document(&root, PROPOSALS_FILE, &marker, Some(revision))?;
            revision += 1;
        }
        if kind == "project_settings" {
            update_label(
                &root,
                desired_value.as_str().unwrap_or_default(),
                proposal["base_revision"].as_i64().unwrap_or(0),
                client,
                Some(proposal_id),
                false,
            )?;
        } else {
            project_policy(
                &root,
                Some(&desired_value),
                true,
                proposal["base_revision"].as_i64(),
                Some(proposal_id),
            )?;
        }
        "applied"
    };
    proposal["status"] = json!(status);
    proposal["decision"] = json!(decision);
    proposal["decided_at"] = json!(utc());
    proposals[index] = proposal.clone();
    let document = json!({
        "format": PROPOSALS_FORMAT,
        "version": 1,
        "project_uuid": project["project_uuid"],
        "revision": revision + 1,
        "proposals": proposals,
    });
    write_document(&root, PROPOSALS_FILE, &document, Some(revision))?;
    let mut result = json!({"ok": true, "revision": document["revision"], "proposal": proposal});
    if let Some(sequence) = proposal["action_sequence"].as_i64() {
        if crate::action_log::resolve_pending_root(
            &root,
            sequence,
            status,
            &review_detail(status, &kind),
            Some(proposal_id),
            None,
        )
        .is_err()
        {
            result["audit_warning"] =
                json!("Proposal status is saved; its action row remains pending.");
        }
    }
    Ok(result)
}

/// `delete_project`: move a project into a sibling Trash directory.
pub fn delete_project(directory: &Path, client: &str) -> ProjectsResult<Value> {
    if !crate::action_log::CLIENTS.contains(&client) {
        return Err(crate::error::value("client must be a supported interface"));
    }
    let (root, project) = workspace_load(directory)?;
    let trash = root
        .parent()
        .ok_or_else(|| crate::error::value("project Trash path is unsafe"))?
        .join("Trash");
    if trash.is_symlink() {
        return Err(crate::error::value("project Trash path is unsafe"));
    }
    let mut destination: Option<PathBuf> = None;
    let pending_sequence: i64;
    {
        let _guard = project_write_lock(&root, 5.0)?;
        let pending = crate::action_log::record_root(
            &root,
            client,
            "project.delete",
            "pending",
            "Project deletion requested; terminal outcome will replace this row.",
            None,
            None,
        )?;
        let sequence = pending["action"]["sequence"].as_i64().unwrap_or(0);
        pending_sequence = sequence;
        let result = (|| -> ProjectsResult<()> {
            if let Some(owner) = crate::run_observation::live_holder(&root)? {
                let _ = crate::action_log::resolve_pending_root(
                    &root,
                    sequence,
                    "refused",
                    "Project delete refused: active run.",
                    None,
                    None,
                );
                return Err(crate::error::value(format!(
                    "project has an active run: {owner}"
                )));
            }
            let mut active_jobs = Vec::new();
            if crate::competitor_scans::has_active_run(&root)? {
                active_jobs.push("competitor scan");
            }
            if crate::exports::has_active_jobs(&root)? {
                active_jobs.push("export");
            }
            if crate::schedule::active(&root)? {
                active_jobs.push("scheduled scan");
            }
            if !active_jobs.is_empty() {
                let labels = active_jobs.join(", ");
                let _ = crate::action_log::resolve_pending_root(
                    &root,
                    sequence,
                    "refused",
                    &format!("Project delete refused: active {labels} job."),
                    None,
                    None,
                );
                return Err(crate::error::value(format!(
                    "project has active jobs: {labels}"
                )));
            }
            std::fs::create_dir_all(&trash).map_err(ProjectsError::Io)?;
            if !trash.is_dir() {
                let _ = crate::action_log::resolve_pending_root(
                    &root,
                    sequence,
                    "refused",
                    "Project delete refused: Trash path is not a directory.",
                    None,
                    None,
                );
                return Err(crate::error::value("project Trash path is not a directory"));
            }
            let name = format!(
                "{}-{}-{}",
                root.file_name().unwrap_or_default().to_string_lossy(),
                pytime::zulu_seconds().replace([':', '-'], ""),
                &Uuid::new_v4().simple().to_string()[..8]
            );
            let target = trash.join(name);
            std::fs::rename(&root, &target).map_err(ProjectsError::Io)?;
            destination = Some(target);
            Ok(())
        })();
        if let Err(e) = result {
            if root.exists() {
                let class = match &e {
                    ProjectsError::Io(_) => "io",
                    _ => "value",
                };
                let _ = crate::action_log::resolve_pending_root(
                    &root,
                    sequence,
                    "refused",
                    &format!("Project delete refused: {class}."),
                    None,
                    None,
                );
            }
            return Err(e);
        }
    }
    let destination = destination.expect("destination set on success");
    let mut result = json!({
        "ok": true,
        "deleted": true,
        "project_uuid": project["project_uuid"],
        "trash_path": destination.to_string_lossy(),
        "exports_retained": true,
    });
    if crate::action_log::resolve_pending_root(
        &destination,
        pending_sequence,
        "deleted",
        "Moved to sibling Trash; scans and exports retained.",
        None,
        None,
    )
    .is_err()
    {
        result["audit_warning"] = json!("Project was moved; its action row remains pending.");
    }
    Ok(result)
}

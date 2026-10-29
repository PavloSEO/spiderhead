use super::*;

/// `task_detail` (alias `checklist_detail`): bounded task/history/catalogue detail.
pub fn task_detail(directory: &Path, item_id: &str) -> ProjectsResult<Value> {
    let (scoped, local_id) = crate::runtime::resolve_item_scope(directory, item_id, false)?;
    let (root, project) = crate::workspace::load(Path::new(&scoped))?;
    let view = crate::coverage::coverage_status(&root, false)?;
    let items = view
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut found = None;
    for item in &items {
        if obj_get(item, "id")?.as_str() == Some(local_id.as_str()) {
            found = Some(item.clone());
            break;
        }
    }
    let Some(row) = found else {
        return Err(value(format!(
            "task '{item_id}' is unavailable in this checklist"
        )));
    };
    let document = crate::coverage::store::read(&root, &project)?;
    let saved = document
        .and_then(|doc| doc.get("items").and_then(|i| i.get(&local_id)).cloned())
        .unwrap_or_else(|| json!({}));
    let records = saved
        .get("records")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut item = row.as_object().cloned().unwrap_or_default();
    item.insert("id".into(), json!(item_id));
    item.insert("item_id".into(), json!(local_id));
    item.insert("display_state".into(), json!(item_state(&row)));
    item.insert(
        "definition".into(),
        saved.get("definition").cloned().unwrap_or(Value::Null),
    );
    let history: Vec<Value> = records.iter().rev().take(20).rev().cloned().collect();
    item.insert("history".into(), json!(history));
    item.insert("history_total".into(), json!(records.len()));
    item.insert("history_truncated".into(), json!(records.len() > 20));
    let catalogue = crate::catalogue::load_catalogue()?;
    item.insert(
        "catalogue".into(),
        catalogue.get(&local_id).cloned().unwrap_or(Value::Null),
    );
    let definition = saved
        .get("definition")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let definition = if definition.is_object() {
        definition
    } else {
        json!({})
    };
    let task_state = if definition.get("kind").and_then(Value::as_str) == Some("custom")
        && item.get("display_state").and_then(Value::as_str) == Some("not_run")
    {
        "remaining"
    } else {
        item.get("display_state")
            .and_then(Value::as_str)
            .unwrap_or_default()
    };
    let comments = saved
        .get("comments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let actor = records.iter().rev().find_map(|record| {
        let actor = record.get("actor").cloned().unwrap_or(Value::Null);
        let reviewer = record.get("reviewer").cloned().unwrap_or(Value::Null);
        if !falsy(&actor) {
            Some(actor)
        } else if !falsy(&reviewer) {
            Some(reviewer)
        } else {
            None
        }
    });
    let tail: Vec<Value> = comments.iter().rev().take(20).rev().cloned().collect();
    let mut task = json!({
        "state": task_state,
        "executor": definition.get("assignee").cloned().unwrap_or(Value::Null),
        "actor": actor.unwrap_or(Value::Null),
        "next_step": null,
        "comments": tail,
        "comments_total": comments.len(),
        "comments_truncated": comments.len() > 20,
        "revision": view.get("revision").cloned().unwrap_or(Value::Null),
    });
    for field in [
        "assignee",
        "description",
        "acceptance_criterion",
        "finding_ref",
        "method_ref",
        "competitors",
    ] {
        task[field] = definition.get(field).cloned().unwrap_or(Value::Null);
    }
    item.insert("task".into(), task);
    if definition.get("kind").and_then(Value::as_str) == Some("custom") {
        item["task"]["competitor_run"] = task_competitor_run_reference(&root, &local_id, &saved)?;
    }
    Ok(json!({
        "ok": true,
        "revision": view.get("revision").cloned().unwrap_or(Value::Null),
        "item": item,
    }))
}

/// `checklist_detail` alias.
pub fn checklist_detail(directory: &Path, item_id: &str) -> ProjectsResult<Value> {
    task_detail(directory, item_id)
}

/// `scans_page`: browse every retained scan without recomputing coverage.
pub fn scans_page(directory: &Path, offset: i64, limit: i64, full: bool) -> ProjectsResult<Value> {
    pagination(0, offset, limit)?;
    let (root, _project) = crate::workspace::load(directory)?;
    let page = spiderhead_store::history::list_scans(&root.join("scans"), offset, limit)?;
    let keep: BTreeSet<&str> = [
        "uuid",
        "lifecycle",
        "finish_reason",
        "crawl_partial",
        "corpus_partial",
        "pinned",
        "start_url",
        "host",
        "bytes",
        "disk_bytes",
        "created_at",
        "finished_at",
        "source_kind",
        "parent_scan_uuid",
        "evidence_revision",
    ]
    .into_iter()
    .collect();
    let mut items = Vec::new();
    for row in page
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let Some(map) = row.as_object() else {
            continue;
        };
        let mut item = map.clone();
        item.insert("evidence".into(), scan_evidence(map, None)?);
        let mut item = Value::Object(item);
        if !full {
            // Compact mode keeps only the evidence state; the audit projection is heavy.
            if let Some(evidence) = item.get("evidence") {
                if evidence.is_object() {
                    item["evidence"] = json!({
                        "state": evidence.get("state").cloned().unwrap_or(Value::Null)
                    });
                }
            }
            if let Some(obj) = item.as_object_mut() {
                obj.retain(|key, _| keep.contains(key.as_str()) || key == "evidence");
            }
        }
        items.push(item);
    }
    Ok(json!({
        "ok": true,
        "total": page.get("total").cloned().unwrap_or(Value::Null),
        "errors": page.get("errors").cloned().unwrap_or_else(|| json!([])),
        "items": items,
        "pagination": pagination(
            page.get("total").and_then(Value::as_i64).unwrap_or_default(),
            offset,
            limit,
        )?,
    }))
}

/// `observe_activity`: cheap collector telemetry; never opens retained scans.
pub fn observe_activity(directory: &Path, since_revision: Option<&str>) -> ProjectsResult<Value> {
    if let Some(token) = since_revision {
        if !is_hex64(token) {
            return Err(value(
                "since_revision must be a 64-character lowercase SHA-256 token",
            ));
        }
    }
    let (root, project) = crate::workspace::load(directory)?;
    let mut sites = vec![json!({
        "project_uuid": obj_get(&project, "project_uuid")?,
        "site": obj_get(&project, "site")?,
        "role": "primary",
        "directory": ".",
        "runs": crate::run_observation::status_page(&root, 20, 0)?,
    })];
    for candidate in crate::runtime::preparation_status(&root)?
        .get("competitors")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let relative = candidate.get("directory").cloned().unwrap_or(Value::Null);
        let attempt = (|| -> ProjectsResult<Value> {
            let rel = relative
                .as_str()
                .ok_or_else(|| value("unsafe competitor project reference"))?;
            if !rel.starts_with("competitors/")
                || Path::new(rel).components().any(|c| c.as_os_str() == "..")
            {
                return Err(value("unsafe competitor project reference"));
            }
            let child = root.join(rel);
            if child.is_symlink() || !resolve_lenient(&child).starts_with(&root) {
                return Err(value("unsafe competitor project reference"));
            }
            let (child_root, metadata) = crate::workspace::load(&child)?;
            if obj_get(&metadata, "project_uuid")?
                != candidate.get("project_uuid").unwrap_or(&Value::Null)
                || obj_get(obj_get(&metadata, "site")?, "target")?.as_str()
                    != Some(
                        crate::workspace::target(
                            candidate
                                .get("url")
                                .and_then(Value::as_str)
                                .unwrap_or_default(),
                        )?
                        .as_str(),
                    )
            {
                return Err(value("competitor project identity mismatch"));
            }
            Ok(json!({
                "project_uuid": obj_get(&metadata, "project_uuid")?,
                "site": obj_get(&metadata, "site")?,
                "role": "competitor",
                "directory": rel,
                "runs": crate::run_observation::status_page(&child_root, 20, 0)?,
            }))
        })();
        match attempt {
            Ok(site) => sites.push(site),
            Err(err) if site_error_caught(&err) => {
                sites.push(json!({
                    "project_uuid": candidate.get("project_uuid").cloned().unwrap_or(Value::Null),
                    "site": {"target": candidate.get("url").cloned().unwrap_or(Value::Null)},
                    "role": "competitor",
                    "directory": relative,
                    "runs": {
                        "state": "unavailable",
                        "reason": reason(&err),
                        "items": [],
                    },
                }));
            }
            Err(err) => return Err(err),
        }
    }
    revisioned(
        &json!({
            "ok": true,
            "observed_at": utc_now_iso(),
            "sites": {"total": sites.len(), "items": sites},
        }),
        since_revision,
    )
}

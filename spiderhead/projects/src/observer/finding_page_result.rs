use super::*;

/// `_finding_page_result`: shared pagination envelope for both finding paths.
#[allow(clippy::too_many_arguments)]
pub(super) fn finding_page_result(
    row: &Map<String, Value>,
    query: &str,
    sort: &str,
    descending: bool,
    source_count: i64,
    matched_count: i64,
    selected: &[(i64, Value)],
    offset: i64,
    limit: i64,
    group_sizes: Option<&BTreeMap<String, i64>>,
    via_group_ids: &BTreeSet<String>,
) -> ProjectsResult<Value> {
    let mut items = Vec::new();
    for (ordinal, issue) in selected {
        let group_id = issue.get("group_id").and_then(Value::as_str);
        items.push(finding_summary(
            issue,
            *ordinal,
            group_id.and_then(|gid| group_sizes.and_then(|sizes| sizes.get(gid).copied())),
            group_id
                .map(|gid| via_group_ids.contains(gid))
                .unwrap_or(false),
        )?);
    }
    let mut scan = Map::new();
    for key in ["uuid", "source_kind", "lifecycle", "finish_reason", "path"] {
        scan.insert(
            key.to_string(),
            row.get(key).cloned().unwrap_or(Value::Null),
        );
    }
    let returned = selected.len() as i64;
    Ok(json!({
        "ok": true,
        "scan": scan,
        "query": query,
        "sort": {"field": sort, "direction": if descending { "desc" } else { "asc" }},
        "counts": {"source": source_count, "matched": matched_count, "returned": returned},
        "pagination": {
            "offset": offset,
            "limit": limit,
            "next_offset": if offset + returned < matched_count {
                json!(offset + returned)
            } else {
                Value::Null
            },
            "previous_offset": if offset != 0 {
                json!((offset - limit).max(0))
            } else {
                Value::Null
            },
            "truncated": offset + returned < matched_count,
        },
        "items": items,
    }))
}

/// `finding_detail`: one retained finding and its bounded evidence projection.
pub fn finding_detail(
    directory: &Path,
    scan_uuid: Option<&str>,
    ordinal: i64,
) -> ProjectsResult<Value> {
    if ordinal < 0 {
        return Err(value("finding ordinal must be non-negative"));
    }
    let row = scan_row(directory, scan_uuid)?;
    let row_value = Value::Object(row.clone());
    let path = obj_get(&row_value, "path")?.as_str().unwrap_or_default();
    let issue;
    let retained;
    if spiderhead_store::audit_v2::audit_v2_path(Path::new(path)).exists() {
        let reader =
            spiderhead_store::audit_v2::AuditV2Reader::new(Path::new(path), true, false, None)
                .map_err(ProjectsError::Store)?;
        let con = open_companion(&reader.path)?;
        use rusqlite::OptionalExtension;
        let saved: Option<String> = con
            .query_row(
                "SELECT value_json FROM items WHERE pointer='/issues' AND ordinal=?",
                rusqlite::params![ordinal],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| value(e.to_string()))?;
        let Some(text) = saved else {
            return Err(value("retained finding ordinal is unavailable"));
        };
        issue = serde_json::from_str(&text).map_err(|e| value(e.to_string()))?;
        retained = json!({
            "schema_version": reader.header.get("schema_version").cloned().unwrap_or(Value::Null),
            "generated_at": reader
                .header
                .get("run")
                .and_then(|r| r.get("generated_at"))
                .cloned()
                .unwrap_or(Value::Null),
        });
    } else {
        let kept = retained_findings(&row)?;
        let issues = kept["issues"].as_array().cloned().unwrap_or_default();
        if ordinal as usize >= issues.len() {
            return Err(value("retained finding ordinal is unavailable"));
        }
        issue = issues[ordinal as usize].clone();
        retained = kept;
    }
    Ok(json!({
        "ok": true,
        "scan_uuid": obj_get(&row_value, "uuid")?,
        "finding": finding_summary(&issue, ordinal, None, false)?,
        "evidence": bounded_value(&issue, 0),
        "projection_limits": {
            "string_chars": 4096,
            "list_items": 20,
            "object_fields": 40,
            "max_depth": 3,
            "source_path": obj_get(&row_value, "path")?,
        },
        "evidence_truncated": projection_truncated(&issue, 0),
        "audit": {
            "schema_version": obj_get(&retained, "schema_version")?,
            "generated_at": obj_get(&retained, "generated_at")?,
        },
    }))
}

/// `saved_view_page`: apply a saved local finding view to an in-project scan.
pub fn saved_view_page(
    directory: &Path,
    name: &str,
    scan_uuid: Option<&str>,
    offset: i64,
) -> ProjectsResult<Value> {
    let row = scan_row(directory, scan_uuid)?;
    let row_value = Value::Object(row.clone());
    let path = obj_get(&row_value, "path")?.as_str().unwrap_or_default();
    let audit = spiderhead_store::scan_artifact::read_audit(Path::new(path))
        .map_err(ProjectsError::Store)?;
    let result = crate::finding_views::apply_view_to_audit(directory, name, &audit, offset)?;
    Ok(json!({
        "ok": true,
        "scan_uuid": obj_get(&Value::Object(row.clone()), "uuid")?,
        "result": result,
    }))
}

/// `observe`: bounded observer snapshot; never changes project evidence.
pub fn observe(
    directory: &Path,
    consumer: Option<&str>,
    scan_limit: i64,
    run_offset: i64,
    run_limit: i64,
    since_revision: Option<&str>,
) -> ProjectsResult<Value> {
    if let Some(token) = since_revision {
        if !is_hex64(token) {
            return Err(value(
                "since_revision must be a 64-character lowercase SHA-256 token",
            ));
        }
    }
    if !(1..=100).contains(&scan_limit) {
        return Err(value("scan_limit must be from 1 to 100"));
    }
    pagination(0, run_offset, run_limit)?;
    let (root, project) = crate::workspace::load(directory)?;
    let mut coverage_views = Map::new();
    let status = crate::workspace::project_status(&root, Some(&mut coverage_views), false)?;
    let progress = project_progress(&root, 100, 0, Some(status.clone()), Some(&coverage_views))?;
    let root_key = root.to_string_lossy().to_string();
    let execution = crate::execution::status(&root, coverage_views.get(&root_key), false)?;
    let monitor = crate::monitoring::status(&root)?;
    let preparation = obj_get(&status, "preparation")?.clone();
    let coverage = coverage_views
        .get(&root_key)
        .cloned()
        .unwrap_or(Value::Null);
    let mut sites = vec![site_projection(
        &root,
        &project,
        &coverage,
        obj_get(&status, "scans")?,
        "primary",
        &Value::Null,
        &Value::Null,
        &Value::Null,
        ".",
        scan_limit,
        run_offset,
        run_limit,
    )?];
    sites.extend(competitor_sites(
        &root,
        &preparation,
        scan_limit,
        Some(&coverage_views),
        run_offset,
        run_limit,
    )?);
    let mut site_identities = Map::new();
    for item in &sites {
        if let Some(uuid) = item.get("project_uuid").and_then(Value::as_str) {
            site_identities.insert(
                uuid.to_string(),
                json!({
                    "role": item.get("role").cloned().unwrap_or(Value::Null),
                    "target": item
                        .get("site")
                        .and_then(|s| s.get("target"))
                        .cloned()
                        .unwrap_or(Value::Null),
                }),
            );
        }
    }
    let checks = status
        .get("checklist")
        .and_then(|c| c.get("items"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut methods = Vec::new();
    for item in &checks {
        if !["scenario", "skill"]
            .contains(&item.get("kind").and_then(Value::as_str).unwrap_or_default())
        {
            continue;
        }
        let site_key = item
            .get("project_uuid")
            .and_then(Value::as_str)
            .unwrap_or_else(|| project["project_uuid"].as_str().unwrap_or_default());
        let site = site_identities.get(site_key).cloned().unwrap_or_else(|| {
            json!({
                "role": "unknown",
                "target": item
                    .get("scope")
                    .and_then(|s| s.get("site"))
                    .cloned()
                    .unwrap_or(Value::Null),
            })
        });
        methods.push(json!({
            "id": obj_get(item, "id")?,
            "item_id": item.get("item_id").cloned().unwrap_or_else(|| obj_get(item, "id").unwrap_or(&Value::Null).clone()),
            "title": obj_get(item, "title")?,
            "kind": obj_get(item, "kind")?,
            "state": obj_get(item, "state")?,
            "attempt_status": obj_get(item, "attempt_status")?,
            "stale": obj_get(item, "stale")?,
            "reason": obj_get(item, "reason")?,
            "site": site,
        }));
    }
    let scans = obj_get(&status, "scans")?.clone();
    let mut scan_rows = Vec::new();
    for row in scans
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .take(scan_limit as usize)
    {
        let mut merged = row.as_object().cloned().unwrap_or_default();
        let row_map = row.as_object().cloned().unwrap_or_default();
        merged.insert("evidence".into(), scan_evidence(&row_map, None)?);
        scan_rows.push(Value::Object(merged));
    }
    let consumer_name = consumer.unwrap_or("observer/local");
    let inbox = crate::inbox::list_entries(&root, consumer_name, 0, 100, true)?;
    let views_block = coverage.get("views").cloned().unwrap_or(Value::Null);
    let mut snapshot = json!({
        "ok": true,
        "project": obj_get(&status, "project")?,
        "progress": progress,
        "execution": execution,
        "monitor": monitor,
        "runs": sites[0]["runs"],
        "observed_at": utc_now_iso(),
        "policy": crate::runtime::project_policy(&root, None, false, None, None)?,
        "sites": {
            "total": sites.len(),
            "scan_limit_per_site": scan_limit,
            "items": sites,
        },
        "methods": methods,
        "preparation": {
            "state": obj_get(&preparation, "state")?,
            "reason": preparation.get("reason").cloned().unwrap_or(Value::Null),
            "steps": preparation.get("steps").cloned().unwrap_or_else(|| json!({})),
            "competitors": preparation.get("competitors").cloned().unwrap_or_else(|| json!([])),
        },
        "scans": {
            "total": scans.get("total").cloned().unwrap_or(Value::Null),
            "shown": scan_rows.len(),
            "errors": scans.get("errors").cloned().unwrap_or_else(|| json!([])),
            "items": scan_rows,
        },
        "log": evidence_log(&root)?,
        "inbox": inbox,
        "review": {
            "state": coverage.get("state").cloned().unwrap_or(Value::Null),
            "manual_waiting": views_block.get("waiting_for_manual_review").cloned().unwrap_or_else(|| json!([])),
            "deliverable_ready": views_block.get("deliverable_ready").cloned().unwrap_or_else(|| json!([])),
            "plan": coverage.get("plan").cloned().unwrap_or(Value::Null),
        },
        "saved_views": crate::finding_views::list_views(&root)?,
    });
    let mut active: Vec<Value> = Vec::new();
    for item in &checks {
        let running = item.get("attempt_status").and_then(Value::as_str) == Some("running");
        let custom_open = item.get("kind").and_then(Value::as_str) == Some("custom")
            && item.get("applicability").and_then(Value::as_str) == Some("applicable")
            && !item.get("complete").map(|c| !falsy(c)).unwrap_or(false);
        if running || custom_open {
            let mut merged = item.as_object().cloned().unwrap_or_default();
            merged.insert("display_state".into(), json!(item_state(item)));
            active.push(Value::Object(merged));
        }
    }
    active
        .sort_by_key(|item| item.get("attempt_status").and_then(Value::as_str) != Some("running"));
    snapshot["active_tasks"] = json!({
        "items": active.iter().take(100).cloned().collect::<Vec<_>>(),
        "total": active.len(),
        "has_more": active.len() > 100,
    });
    let total_inbox = snapshot["inbox"]["pagination"]["total"]
        .as_i64()
        .unwrap_or_default();
    snapshot["latest_inbox"] =
        crate::inbox::list_entries(&root, consumer_name, (total_inbox - 100).max(0), 100, true)?;
    if let Some(name) = consumer {
        snapshot["inbox_unread"] = crate::inbox::unread_summary(&root, name, 10)?;
    }
    revisioned(&snapshot, since_revision)
}

/// `checklist_page`: filter before paging; keep rich checklist evidence.
#[allow(clippy::too_many_arguments)]
pub fn checklist_page(
    directory: &Path,
    offset: i64,
    limit: i64,
    query: &str,
    kind: Option<&str>,
    state: Option<&str>,
    sort: &str,
    descending: bool,
    states: Option<&[String]>,
) -> ProjectsResult<Value> {
    pagination(0, offset, limit)?;
    if query.chars().count() > 256 {
        return Err(value(
            "checklist query must be text of at most 256 characters",
        ));
    }
    if !matches!(
        kind,
        None | Some("method")
            | Some("schema")
            | Some("check")
            | Some("skill")
            | Some("scenario")
            | Some("custom")
    ) {
        return Err(value("unsupported checklist kind"));
    }
    if !CHECKLIST_SORTS.contains(&sort) {
        return Err(value(
            "checklist sort must be id, priority, state or updated",
        ));
    }
    if let Some(list) = states {
        if list.len() > 8
            || list
                .iter()
                .any(|entry| !CHECKLIST_STATES.contains(&entry.as_str()))
        {
            return Err(value(
                "checklist states must be up to 8 known display states",
            ));
        }
    }
    let (root, project) = crate::workspace::load(directory)?;
    let saved_items = crate::coverage::store::read(&root, &project)?
        .and_then(|doc| doc.get("items").cloned())
        .unwrap_or_else(|| json!({}));
    let snapshot = crate::workspace::project_status(directory, None, false)?;
    let checklist = &snapshot["checklist"];
    let needle = icu_casemap::CaseMapper::new()
        .fold_string(query)
        .to_string()
        .trim()
        .to_string();
    let mut rows = Vec::new();
    for item in checklist
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let text = ["id", "title", "reason"]
            .iter()
            .map(|key| {
                item.get(*key)
                    .map(|v| if falsy(v) { String::new() } else { py_str(v) })
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" ");
        let text = icu_casemap::CaseMapper::new()
            .fold_string(&text)
            .to_string();
        let item_kind = item.get("kind").and_then(Value::as_str).unwrap_or_default();
        if kind == Some("method") && !["scenario", "skill"].contains(&item_kind) {
            continue;
        }
        if kind == Some("schema")
            && !["schema", "structured data", "json-ld"]
                .iter()
                .any(|word| text.contains(word))
        {
            continue;
        }
        if !matches!(kind, None | Some("method") | Some("schema")) && Some(item_kind) != kind {
            continue;
        }
        let display = item_state(&item);
        let task_state = if item_kind == "custom" && display == "not_run" {
            "remaining"
        } else {
            display
        };
        if !needle.is_empty() && !text.contains(needle.as_str()) {
            continue;
        }
        if let Some(want) = state {
            if want != display {
                continue;
            }
        }
        if let Some(list) = states {
            if !list.iter().any(|entry| entry == display) {
                continue;
            }
        }
        let saved = saved_items
            .get(item.get("id").and_then(Value::as_str).unwrap_or_default())
            .cloned()
            .unwrap_or_else(|| json!({}));
        let records = saved
            .get("records")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let stamps: Vec<String> = records
            .iter()
            .filter(|record| record.is_object())
            .map(|record| {
                record
                    .get("recorded_at")
                    .map(|v| if falsy(v) { String::new() } else { py_str(v) })
                    .unwrap_or_default()
            })
            .collect();
        let definition = saved
            .get("definition")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let definition = if definition.is_object() {
            definition
        } else {
            json!({})
        };
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
        let updated = stamps.iter().max().cloned().filter(|s| !s.is_empty());
        let mut row = item.as_object().cloned().unwrap_or_default();
        row.insert("display_state".into(), json!(display));
        row.insert(
            "updated".into(),
            updated.map(Value::String).unwrap_or(Value::Null),
        );
        let mut task = json!({
            "state": task_state,
            "executor": definition.get("assignee").cloned().unwrap_or(Value::Null),
            "actor": actor.clone().unwrap_or(Value::Null),
            "next_step": null,
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
        row.insert("task".into(), task);
        row.insert(
            "executor".into(),
            definition.get("assignee").cloned().unwrap_or(Value::Null),
        );
        row.insert("actor".into(), actor.unwrap_or(Value::Null));
        row.insert("next_step".into(), Value::Null);
        rows.push(Value::Object(row));
    }
    if sort == "id" {
        if descending {
            rows.reverse();
        }
    } else {
        let field = match sort {
            "priority" => "priority",
            "state" => "display_state",
            _ => "updated",
        };
        rows.sort_by(|a, b| {
            let key = |row: &Value| {
                (
                    row.get(field)
                        .map(|v| if falsy(v) { String::new() } else { py_str(v) })
                        .unwrap_or_default(),
                    row.get("id")
                        .map(|v| if falsy(v) { String::new() } else { py_str(v) })
                        .unwrap_or_default(),
                )
            };
            let ord = key(a).cmp(&key(b));
            if descending {
                ord.reverse()
            } else {
                ord
            }
        });
    }
    let total = rows.len() as i64;
    let page: Vec<Value> = rows
        .into_iter()
        .skip(offset.max(0) as usize)
        .take(limit as usize)
        .collect();
    Ok(json!({
        "ok": true,
        "revision": checklist.get("revision").cloned().unwrap_or(Value::Null),
        "sort": {"field": sort, "direction": if descending { "desc" } else { "asc" }},
        "items": page,
        "pagination": pagination(total, offset, limit)?,
    }))
}

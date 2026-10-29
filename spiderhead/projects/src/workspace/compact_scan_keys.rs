use super::*;

pub(super) const COMPACT_SCAN_KEYS: &[&str] = &[
    "uuid",
    "lifecycle",
    "finish_reason",
    "crawl_partial",
    "corpus_partial",
    "pinned",
    "host",
    "bytes",
    "disk_bytes",
    "created_at",
    "finished_at",
    "source_kind",
    "parent_scan_uuid",
    "evidence_revision",
    "path",
    "mtime_ns",
    "config_fingerprint",
];

pub(super) fn compact_scan(item: &Value) -> Value {
    let mut out = Map::new();
    if let Some(object) = item.as_object() {
        for key in COMPACT_SCAN_KEYS {
            if let Some(v) = object.get(*key) {
                out.insert((*key).to_owned(), v.clone());
            }
        }
    }
    Value::Object(out)
}

/// Scan history, per-site coverage and bounded operational status.
pub fn project_status(
    directory: &Path,
    coverage_views: Option<&mut Map<String, Value>>,
    verify_evidence: bool,
) -> ProjectsResult<Value> {
    let (root, document) = load(directory)?;
    let mut views = Map::new();
    let key = root.to_string_lossy().to_string();
    if let Some(ref v) = coverage_views {
        views = (**v).clone();
    }
    if !views.contains_key(&key) {
        let status = crate::coverage::coverage_status(&root, verify_evidence)?;
        views.insert(key.clone(), status);
    }
    let scans = spiderhead_store::history::list_scans(&root.join("scans"), 0, 100)?;
    let running_scan = running_scan(&root, &scans);
    let items = scans["items"].as_array().cloned().unwrap_or_default();
    let latest = items.first().cloned();
    let latest_completeness = latest.as_ref().map(|latest| {
        let partial = latest["lifecycle"].as_str() != Some("finished")
            || latest["crawl_partial"].as_bool().unwrap_or(false)
            || latest["corpus_partial"].as_bool().unwrap_or(false);
        json!({
            "state": if partial { "partial" } else { "complete" },
            "crawl_partial": latest["crawl_partial"],
            "corpus_partial": latest["corpus_partial"],
            "reason": if partial {
                latest["finish_reason"].as_str().map(str::to_owned).or_else(|| {
                    Some(format!(
                        "scan lifecycle is {}",
                        latest["lifecycle"].as_str().unwrap_or_default()
                    ))
                })
            } else {
                None
            },
        })
    });
    let scan_errors = scans["errors"].as_array().cloned().unwrap_or_default();
    let checklist = crate::runtime::aggregate_coverage(
        &key,
        views[&key].clone(),
        Some(&mut views),
        verify_evidence,
    )?;
    let agent_state = agent_state(&root);
    let storage = storage_measurement(&root);
    let presentation = project_presentation(&root, &document)?;
    let mut scans_out = scans.clone();
    scans_out.insert(
        "items".into(),
        Value::Array(items.iter().map(compact_scan).collect()),
    );
    let result = Ok(json!({
        "ok": true,
        "project": {
            "site": presentation["site"],
            "settings_revision": presentation["settings_revision"],
            "template_references": document["template_references"],
            "profile_references": document["profile_references"],
        },
        "scans": scans_out,
        "summary": {
            "running_scan": running_scan,
            "open_tasks": open_tasks(&views[&key]),
            "latest_scan": latest.map(|latest| json!({
                "uuid": latest["uuid"],
                "state": latest["lifecycle"],
                "completeness": latest_completeness,
            })).unwrap_or_else(|| json!({
                "uuid": null,
                "state": null,
                "completeness": null,
                "reason": "no retained scan is available",
            })),
            "folder_health": {
                "state": if scan_errors.is_empty() { "ok" } else { "degraded" },
                "reason": if scan_errors.is_empty() { None } else {
                    Some(format!("{} retained scan item(s) could not be read", scan_errors.len()))
                },
            },
        },
        "agent_state": agent_state,
        "storage": storage,
        "checklist": checklist,
        "preparation": crate::runtime::preparation_status(&root)?,
    }));
    if let Some(v) = coverage_views {
        // Coverage views are shared with the caller, so the clone must be written back.
        *v = views;
    }
    result
}

pub(super) fn settings(root: &Path) -> ProjectsResult<Value> {
    crate::project_settings::read_settings(root)
}

pub(super) fn project_presentation(root: &Path, document: &Value) -> ProjectsResult<Value> {
    let settings = settings(root)?;
    let mut out = document.clone();
    if let Some(site) = out.get_mut("site").and_then(Value::as_object_mut) {
        site.insert(
            "label".into(),
            settings["settings"]
                .get("label")
                .cloned()
                .unwrap_or(Value::Null),
        );
    }
    if let Some(object) = out.as_object_mut() {
        object.insert("settings_revision".into(), settings["revision"].clone());
    }
    Ok(out)
}

pub(super) fn agent_state(root: &Path) -> Value {
    match crate::action_log::latest_agent_activity(root) {
        Ok(activity) => {
            let last_seen = activity.get("last_seen_at").cloned().unwrap_or(Value::Null);
            let recorded = !last_seen.is_null();
            json!({
                "last_seen_at": last_seen,
                "connected": null,
                "heartbeat_source": if recorded { activity.get("heartbeat_source").cloned().unwrap_or(Value::Null) } else { Value::Null },
                "activity_source": if recorded { activity.get("source").cloned().unwrap_or(Value::Null) } else { Value::Null },
                "reason": if recorded {
                    "last activity is recorded; current connection is not established"
                } else {
                    "no agent activity is recorded"
                },
            })
        }
        Err(e) => json!({
            "last_seen_at": null,
            "connected": null,
            "heartbeat_source": null,
            "activity_source": null,
            "reason": format!("agent activity is unavailable ({})", kind_name(&e)),
        }),
    }
}

pub(super) fn kind_name(e: &crate::error::ProjectsError) -> &'static str {
    match e {
        crate::error::ProjectsError::Value(_) => "ValueError",
        crate::error::ProjectsError::Io(_) => "OSError",
        crate::error::ProjectsError::Store(_) => "OSError",
        crate::error::ProjectsError::Json(_) => "ValueError",
        crate::error::ProjectsError::Py { name, .. } => name,
    }
}

pub(super) fn storage_measurement(root: &Path) -> Value {
    match crate::status_measurement::read_storage_measurement(root) {
        Ok(v) => v,
        Err(e) => json!({
            "relative_path": "scans",
            "total_bytes": null,
            "scan_count": null,
            "measured_at": null,
            "state": "unavailable",
            "reason": format!("project storage measurement is unavailable ({})", kind_name(&e)),
        }),
    }
}

/// Bounded open-task summary derived from an aggregate checklist.
pub fn open_tasks(checklist: &Value) -> Value {
    if checklist.get("state").and_then(Value::as_str) != Some("initialized")
        || !checklist.get("items").is_some_and(Value::is_array)
    {
        return json!({
            "count": null,
            "state": "unavailable",
            "scope": "primary_project_custom_checklist",
            "reason": "the project checklist is not initialized",
        });
    }
    let count = checklist["items"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter(|item| {
            item.get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| id.starts_with("custom:"))
                && !item
                    .get("complete")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                && !item.get("stale").and_then(|v| v.as_bool()).unwrap_or(false)
                && !matches!(
                    item.get("applicability").and_then(Value::as_str),
                    Some("excluded") | Some("not_agreed")
                )
        })
        .count();
    json!({
        "count": count,
        "state": if count > 0 { "measured" } else { "no_active_tasks" },
        "scope": "primary_project_custom_checklist",
        "reason": null,
    })
}

pub(super) fn running_scan(root: &Path, scans: &Map<String, Value>) -> Value {
    let items = scans["items"].as_array().cloned().unwrap_or_default();
    let running_item = || {
        items
            .iter()
            .find(|i| i.get("lifecycle").and_then(Value::as_str) == Some("running"))
            .cloned()
    };
    if !root.join(crate::run_observation::NAME).is_file() {
        if let Some(active) = running_item() {
            return json!({
                "running": true,
                "id": active["uuid"],
                "progress": null,
                "reason": "collector progress has not been recorded",
            });
        }
        return json!({
            "running": null,
            "id": null,
            "progress": null,
            "reason": "no collector observation is recorded",
        });
    }
    let observed = match crate::run_observation::status(root, 1) {
        Ok(o) => o,
        Err(e) => {
            return json!({
                "running": null,
                "id": null,
                "progress": null,
                "reason": format!("collector status is unavailable: {}", kind_name(&e)),
            })
        }
    };
    if observed["active_total"].as_i64() == Some(0) {
        if let Some(active) = running_item() {
            return json!({
                "running": true,
                "id": active["uuid"],
                "progress": null,
                "reason": "collector progress has not been recorded",
            });
        }
        return json!({"running": false, "id": null, "progress": null, "reason": null});
    }
    let observed_items = observed["items"].as_array().cloned().unwrap_or_default();
    let active = observed_items
        .iter()
        .find(|i| i.get("state").and_then(Value::as_str) == Some("running"));
    let Some(active) = active else {
        return json!({
            "running": null,
            "id": null,
            "progress": null,
            "reason": "active collector details are unavailable",
        });
    };
    let counters = active.get("counters").cloned().unwrap_or(json!({}));
    json!({
        "running": true,
        "id": active.get("id"),
        "progress": {
            "fetched": counters.get("fetched"),
            "queued": counters.get("queued"),
            "inflight": counters.get("inflight"),
            "excluded": counters.get("excluded"),
            "sampled_at": active.get("telemetry").and_then(|t| t.get("sampled_at")),
        },
        "reason": null,
    })
}

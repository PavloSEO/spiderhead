//! Handlers of the project group beyond workspace creation: settings, progress,
//! the event journal, the checklist, saved views, priorities and policy, competitor
//! candidates, archives, the observer pages and the scheduled-scan settings.
//!
//! Every handler answers the same document on both surfaces; the CLI-only variants
//! (an archive failure printed as `ok: false`) live in the CLI crate.

use std::path::Path;

use serde_json::{json, Map, Value};

use spiderhead_projects::error::{py_exception_name, ProjectsError};

use crate::error::ToolError;

use super::{kwargs, missing_option, need_str, opt, opt_bool, opt_int, opt_str};

fn done(result: Result<Value, ProjectsError>) -> Result<Value, ToolError> {
    result.map_err(|error| ToolError::Failed(error.to_string()))
}

/// The handler wrapper's fire-and-forget action-log row: failures never block the work.
fn record_action(directory: &Path, action: &str, outcome: &str, detail: &str) {
    let _ = spiderhead_projects::action_log::record_interface_action(
        directory, action, outcome, detail, None, None,
    );
}

/// `str(value)` of the JSON scalars a formatted audit detail embeds.
fn py_str(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "None".to_owned(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Bool(true)) => "True".to_owned(),
        Some(Value::Bool(false)) => "False".to_owned(),
        Some(other) => other.to_string(),
    }
}

fn required_i64(
    args: &Map<String, Value>,
    name: &str,
    command: &str,
    accepted: &[&str],
) -> Result<i64, ToolError> {
    opt_int(args, name).ok_or_else(|| missing_option(command, name, accepted))
}

fn required_value<'a>(
    args: &'a Map<String, Value>,
    name: &str,
    command: &str,
    accepted: &[&str],
) -> Result<&'a Value, ToolError> {
    opt(args, name).ok_or_else(|| missing_option(command, name, accepted))
}

/// Adds only this project's unread summary without consuming it.
pub(super) fn with_inbox_notice(
    mut result: Value,
    directory: &Path,
    consumer: Option<&str>,
) -> Result<Value, ToolError> {
    let Some(consumer) = consumer else {
        return Ok(result);
    };
    let unread = done(spiderhead_projects::inbox::unread_summary(
        directory, consumer, 10,
    ))?;
    let tasks = done(spiderhead_projects::inbox::unread_goal_tasks(
        directory, consumer, 10,
    ))?;
    if let Some(object) = result.as_object_mut() {
        object.insert("inbox_unread".into(), unread);
        object.insert("owner_tasks_unread".into(), tasks);
    }
    Ok(result)
}

// ── Settings ─────────────────────────────────────────────────────────────

/// `project_settings`: the revisioned settings document.
pub fn project_settings(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let directory = need_str(&args, "directory", "project-settings", &["directory"])?;
    done(spiderhead_projects::project_settings::read_settings(
        Path::new(directory),
    ))
}

/// `project_settings_propose`: record a settings proposal for the owner to review.
pub fn project_settings_propose(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 3] = ["directory", "changes", "expected_revision"];
    let directory = need_str(&args, "directory", "project-settings-propose", &ACCEPTED)?;
    let changes = required_value(&args, "changes", "project-settings-propose", &ACCEPTED)?;
    let expected = required_i64(
        &args,
        "expected_revision",
        "project-settings-propose",
        &ACCEPTED,
    )?;
    done(spiderhead_projects::project_settings::propose_settings(
        Path::new(directory),
        changes,
        spiderhead_projects::action_log::current_interface(),
        expected,
    ))
}

/// `project_settings_proposals`: a bounded page of proposals.
pub fn project_settings_proposals(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 3] = ["directory", "offset", "limit"];
    let directory = need_str(&args, "directory", "project-settings-proposals", &ACCEPTED)?;
    done(spiderhead_projects::project_settings::list_proposals(
        Path::new(directory),
        opt_int(&args, "offset").unwrap_or_default(),
        opt_int(&args, "limit").unwrap_or(50),
    ))
}

/// `project_settings_review`: approve or deny one proposal.
pub fn project_settings_review(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 4] = ["directory", "proposal_id", "decision", "expected_revision"];
    let directory = need_str(&args, "directory", "project-settings-review", &ACCEPTED)?;
    let proposal_id = need_str(&args, "proposal_id", "project-settings-review", &ACCEPTED)?;
    let decision = need_str(&args, "decision", "project-settings-review", &ACCEPTED)?;
    let expected = required_i64(
        &args,
        "expected_revision",
        "project-settings-review",
        &ACCEPTED,
    )?;
    done(spiderhead_projects::project_settings::review_proposal(
        Path::new(directory),
        proposal_id,
        decision,
        expected,
        "cli",
    ))
}

/// `project_agent_heartbeat`: record that an agent client is active.
pub fn project_agent_heartbeat(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let directory = need_str(
        &args,
        "directory",
        "project-agent-heartbeat",
        &["directory"],
    )?;
    let result = done(spiderhead_projects::action_log::record_interface_action(
        Path::new(directory),
        "agent.heartbeat",
        "applied",
        "MCP client reported activity",
        None,
        None,
    ))?;
    Ok(json!({
        "ok": true,
        "last_seen_at": result["action"]["occurred_at"],
        "connected": null,
    }))
}

// ── Progress, observer and journal ───────────────────────────────────────

/// `project_progress`: a bounded progress page without running or changing work.
pub fn project_progress(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 4] = ["directory", "limit", "offset", "consumer"];
    let directory = need_str(&args, "directory", "project-progress", &ACCEPTED)?;
    let result = done(spiderhead_projects::progress::project_progress(
        Path::new(directory),
        opt_int(&args, "limit").unwrap_or(20),
        opt_int(&args, "offset").unwrap_or_default(),
        None,
        None,
    ))?;
    with_inbox_notice(result, Path::new(directory), opt_str(&args, "consumer"))
}

/// `project_event_append`: append one attributed journal event.
pub fn project_event_append(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 4] = ["directory", "source", "actor", "text"];
    let directory = need_str(&args, "directory", "project-event-append", &ACCEPTED)?;
    let source = need_str(&args, "source", "project-event-append", &ACCEPTED)?;
    let actor = need_str(&args, "actor", "project-event-append", &ACCEPTED)?;
    let text = need_str(&args, "text", "project-event-append", &ACCEPTED)?;
    let result = done(spiderhead_projects::journal::record(
        Path::new(directory),
        source,
        actor,
        text,
    ))?;
    record_action(
        Path::new(directory),
        "project.event.append",
        "applied",
        &format!(
            "Recorded {source}/{actor} event {}",
            py_str(result["event"].get("sequence"))
        ),
    );
    Ok(result)
}

/// `project_event_page`: a bounded page of the event journal.
pub fn project_event_page(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 8] = [
        "directory",
        "offset",
        "limit",
        "source",
        "query",
        "since_revision",
        "follow",
        "wait_seconds",
    ];
    let directory = need_str(&args, "directory", "project-event-page", &ACCEPTED)?;
    done(spiderhead_projects::event_log::page(
        Path::new(directory),
        opt_int(&args, "offset").unwrap_or_default(),
        opt_int(&args, "limit").unwrap_or(50),
        opt_str(&args, "source"),
        opt_str(&args, "query").unwrap_or_default(),
        opt_int(&args, "since_revision"),
        opt_bool(&args, "follow").unwrap_or(false),
        opt_int(&args, "wait_seconds").unwrap_or_default(),
    ))
}

/// `project_observe`: the read-only observer document.
pub fn project_observe(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 6] = [
        "directory",
        "consumer",
        "scan_limit",
        "run_offset",
        "run_limit",
        "since_revision",
    ];
    let directory = need_str(&args, "directory", "project-observe", &ACCEPTED)?;
    done(spiderhead_projects::observer::observe(
        Path::new(directory),
        opt_str(&args, "consumer"),
        opt_int(&args, "scan_limit").unwrap_or(20),
        opt_int(&args, "run_offset").unwrap_or_default(),
        opt_int(&args, "run_limit").unwrap_or(20),
        opt_str(&args, "since_revision"),
    ))
}

/// `project_activity`: lightweight current activity of a project and its sites.
pub fn project_activity(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 2] = ["directory", "since_revision"];
    let directory = need_str(&args, "directory", "project-activity", &ACCEPTED)?;
    done(spiderhead_projects::observer::observe_activity(
        Path::new(directory),
        opt_str(&args, "since_revision"),
    ))
}

/// `project_checklist_page`: a bounded searchable page of checklist evidence.
pub fn project_checklist_page(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 9] = [
        "directory",
        "offset",
        "limit",
        "query",
        "kind",
        "state",
        "sort",
        "descending",
        "states",
    ];
    let directory = need_str(&args, "directory", "project-checklist-page", &ACCEPTED)?;
    let states: Option<Vec<String>> = opt(&args, "states").and_then(Value::as_array).map(|rows| {
        rows.iter()
            .map(|row| row.as_str().unwrap_or_default().to_owned())
            .collect()
    });
    done(spiderhead_projects::observer::checklist_page(
        Path::new(directory),
        opt_int(&args, "offset").unwrap_or_default(),
        opt_int(&args, "limit").unwrap_or(50),
        opt_str(&args, "query").unwrap_or_default(),
        opt_str(&args, "kind"),
        opt_str(&args, "state"),
        opt_str(&args, "sort").unwrap_or("id"),
        opt_bool(&args, "descending").unwrap_or(false),
        states.as_deref(),
    ))
}

/// `project_task_detail`: one task definition, its evidence and bounded history.
pub fn project_task_detail(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 3] = ["directory", "item_id", "task_id"];
    let directory = need_str(&args, "directory", "project-task-detail", &ACCEPTED)?;
    let chosen = opt_str(&args, "task_id")
        .filter(|text| !text.is_empty())
        .or_else(|| opt_str(&args, "item_id").filter(|text| !text.is_empty()))
        .ok_or_else(|| {
            ToolError::Failed(
                "pass the task identifier as task_id (item_id is the accepted alias)".to_owned(),
            )
        })?;
    done(spiderhead_projects::observer::task_detail(
        Path::new(directory),
        chosen,
    ))
}

/// `project_scans`: a bounded page of retained project scans.
pub fn project_scans(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 4] = ["directory", "offset", "limit", "full"];
    let directory = need_str(&args, "directory", "project-scans", &ACCEPTED)?;
    done(spiderhead_projects::observer::scans_page(
        Path::new(directory),
        opt_int(&args, "offset").unwrap_or_default(),
        opt_int(&args, "limit").unwrap_or(20),
        opt_bool(&args, "full").unwrap_or(false),
    ))
}

// ── Facts, checklist, priorities and policy ──────────────────────────────

/// Detection tools of `project-facts --detect`: the wired single-page detector;
/// the live robots probe is not carried by this build and reports itself unavailable.
struct Detection;

impl spiderhead_projects::facts::DetectTools for Detection {
    fn robots_check(&self, _url: &str, _paths: &[String]) -> Value {
        json!({"ok": false, "error": "robots-check is not yet available in this build"})
    }

    fn tech_detect(&self, url: &str) -> Value {
        match super::checks::tech_detect(&json!({"url": url})) {
            Ok(document) => document,
            Err(error) => json!({"ok": false, "error": error.to_string()}),
        }
    }
}

/// `project_facts`: preview or record stack facts.
pub fn project_facts(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 4] = ["directory", "facts", "detect", "apply"];
    let directory = need_str(&args, "directory", "project-facts", &ACCEPTED)?;
    done(spiderhead_projects::facts::project_facts(
        Path::new(directory),
        opt(&args, "facts"),
        opt_bool(&args, "detect").unwrap_or(false),
        opt_bool(&args, "apply").unwrap_or(false),
        Some(&Detection),
    ))
}

/// `project_checklist_init`: create or reconcile the local checklist, running nothing.
pub fn project_checklist_init(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 5] = ["directory", "template", "expected_revision", "plan", "full"];
    let directory = need_str(&args, "directory", "project-checklist-init", &ACCEPTED)?;
    let result = spiderhead_projects::coverage::initialize_coverage_full(
        Path::new(directory),
        opt(&args, "template"),
        opt_int(&args, "expected_revision"),
        opt(&args, "plan"),
    );
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            let (name, message) = py_exception_name(&error);
            record_action(Path::new(directory), "checklist-init", "denied", name);
            return Err(ToolError::Failed(message));
        }
    };
    record_action(
        Path::new(directory),
        "checklist-init",
        "applied",
        &format!("revision={}", py_str(result.get("revision"))),
    );
    if !opt_bool(&args, "full").unwrap_or(false) {
        if let Some(items) = result.get("items").and_then(Value::as_array) {
            // The default answer is bounded: status and counts, not the item list.
            let mut summary: Map<String, Value> = result
                .as_object()
                .map(|object| {
                    object
                        .iter()
                        .filter(|(key, _)| key.as_str() != "items")
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect()
                })
                .unwrap_or_default();
            summary.insert("item_count".into(), json!(items.len()));
            return Ok(Value::Object(summary));
        }
    }
    Ok(result)
}

/// `project_checklist_update`: update one checklist definition without executing it.
pub fn project_checklist_update(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 3] = ["directory", "item", "expected_revision"];
    let directory = need_str(&args, "directory", "project-checklist-update", &ACCEPTED)?;
    let item = required_value(&args, "item", "project-checklist-update", &ACCEPTED)?;
    let expected = required_i64(
        &args,
        "expected_revision",
        "project-checklist-update",
        &ACCEPTED,
    )?;
    let Some(object) = item.as_object() else {
        return Err(ToolError::Failed("item must be an object".to_owned()));
    };
    let (target, local_id) = match object.get("id").and_then(Value::as_str) {
        Some(id) => done_scope(spiderhead_projects::runtime::resolve_item_scope(
            Path::new(directory),
            id,
            true,
        ))?,
        None => (directory.to_owned(), String::new()),
    };
    let mut merged = object.clone();
    if object.get("id").and_then(Value::as_str).is_some() {
        merged.insert("id".into(), json!(local_id));
    }
    let result = spiderhead_projects::coverage::update_item(
        Path::new(&target),
        &Value::Object(merged),
        expected,
    );
    match result {
        Ok(result) => {
            record_action(
                Path::new(&target),
                "checklist-update",
                "applied",
                &format!("item_id={local_id}"),
            );
            record_action(
                Path::new(directory),
                "project.checklist.update",
                "applied",
                "Updated checklist item definition",
            );
            Ok(result)
        }
        Err(error) => {
            let (name, message) = py_exception_name(&error);
            record_action(Path::new(&target), "checklist-update", "denied", name);
            Err(ToolError::Failed(message))
        }
    }
}

fn done_scope(
    result: Result<(String, String), ProjectsError>,
) -> Result<(String, String), ToolError> {
    result.map_err(|error| ToolError::Failed(error.to_string()))
}

/// `project_checklist_record`: record supplied evidence for one item.
pub fn project_checklist_record(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 4] = ["directory", "item_id", "record", "expected_revision"];
    let directory = need_str(&args, "directory", "project-checklist-record", &ACCEPTED)?;
    let item_id = need_str(&args, "item_id", "project-checklist-record", &ACCEPTED)?;
    let record = required_value(&args, "record", "project-checklist-record", &ACCEPTED)?;
    let expected = required_i64(
        &args,
        "expected_revision",
        "project-checklist-record",
        &ACCEPTED,
    )?;
    let (target, local_id) = done_scope(spiderhead_projects::runtime::resolve_item_scope(
        Path::new(directory),
        item_id,
        true,
    ))?;
    let result = spiderhead_projects::coverage::record_execution(
        Path::new(&target),
        &local_id,
        record,
        expected,
    );
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            let (name, message) = py_exception_name(&error);
            record_action(Path::new(&target), "checklist-record", "denied", name);
            return Err(ToolError::Failed(message));
        }
    };
    record_action(
        Path::new(&target),
        "checklist-record",
        "applied",
        &format!(
            "item_id={local_id}; status={}",
            py_str(record.get("status"))
        ),
    );
    let action = if record.get("signoff") == Some(&Value::Bool(true)) {
        "project.checklist.signoff"
    } else if record.get("review").and_then(Value::as_str) == Some("approved") {
        "project.checklist.review"
    } else {
        "project.checklist.record"
    };
    let status = record
        .get("status")
        .map_or("unknown".to_owned(), |value| py_str(Some(value)));
    record_action(
        Path::new(directory),
        action,
        "applied",
        &format!("Recorded checklist item {item_id} as {status}"),
    );
    Ok(result)
}

/// `project_task_comment`: an attributed note with an optional typed identity.
pub fn project_task_comment(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 6] = [
        "directory",
        "item_id",
        "text",
        "actor",
        "expected_revision",
        "reference",
    ];
    let directory = need_str(&args, "directory", "project-task-comment", &ACCEPTED)?;
    let item_id = need_str(&args, "item_id", "project-task-comment", &ACCEPTED)?;
    let text = need_str(&args, "text", "project-task-comment", &ACCEPTED)?;
    let actor = need_str(&args, "actor", "project-task-comment", &ACCEPTED)?;
    let expected = required_i64(
        &args,
        "expected_revision",
        "project-task-comment",
        &ACCEPTED,
    )?;
    let (target, local_id) = done_scope(spiderhead_projects::runtime::resolve_item_scope(
        Path::new(directory),
        item_id,
        true,
    ))?;
    let result = spiderhead_projects::coverage::append_task_comment(
        Path::new(&target),
        &local_id,
        text,
        actor,
        expected,
        opt(&args, "reference"),
    );
    match result {
        Ok(result) => {
            record_action(
                Path::new(&target),
                "task-comment",
                "applied",
                &format!("item_id={local_id}"),
            );
            Ok(result)
        }
        Err(error) => {
            let (name, message) = py_exception_name(&error);
            record_action(Path::new(&target), "task-comment", "denied", name);
            Err(ToolError::Failed(message))
        }
    }
}

/// `project_priorities`: preview or explicitly apply an offline priority policy.
pub fn project_priorities(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 5] = ["directory", "policy", "apply", "expected_revision", "full"];
    let directory = need_str(&args, "directory", "project-priorities", &ACCEPTED)?;
    done(spiderhead_projects::priorities::project_priorities(
        Path::new(directory),
        opt(&args, "policy"),
        opt_bool(&args, "apply").unwrap_or(false),
        opt_int(&args, "expected_revision"),
        opt_bool(&args, "full").unwrap_or(false),
    ))
}

/// `project_policy`: read the crawl policy, or apply a replacement under a pending action row.
pub fn project_policy(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 4] = ["directory", "policy", "apply", "expected_revision"];
    let directory = need_str(&args, "directory", "project-policy", &ACCEPTED)?;
    let dir = Path::new(directory);
    let policy = opt(&args, "policy");
    let apply = opt_bool(&args, "apply").unwrap_or(false);
    let expected = opt_int(&args, "expected_revision");
    let (root, _project) = spiderhead_projects::workspace::load(dir)
        .map_err(|error| ToolError::Failed(error.to_string()))?;
    let _guard = spiderhead_projects::runtime::project_write_lock(&root, 5.0)
        .map_err(|error| ToolError::Failed(error.to_string()))?;
    if !apply {
        return done(spiderhead_projects::runtime::project_policy(
            dir, policy, false, expected, None,
        ));
    }
    let operation_id = uuid::Uuid::new_v4().simple().to_string();
    let pending = done(spiderhead_projects::action_log::record(
        &root,
        "cli",
        "project.policy.apply",
        "pending",
        "Crawl policy update pending; terminal result will replace this row.",
        None,
        Some(&operation_id),
    ));
    let pending = pending?;
    let sequence = pending["action"]["sequence"].as_i64().unwrap_or_default();
    let applied = spiderhead_projects::runtime::project_policy(
        dir,
        policy,
        true,
        expected,
        Some(&operation_id),
    );
    match applied {
        Err(error) => {
            let message = error.to_string();
            let outcome = if message.to_lowercase().contains("revision") {
                "stale"
            } else {
                "refused"
            };
            let _ = spiderhead_projects::action_log::resolve_pending_root(
                &root,
                sequence,
                outcome,
                "Crawl policy update did not apply.",
                None,
                Some(&operation_id),
            );
            Err(ToolError::Failed(message))
        }
        Ok(mut result) => {
            let resolved = spiderhead_projects::action_log::resolve_pending_root(
                &root,
                sequence,
                "applied",
                &format!(
                    "Applied crawl policy revision {}.",
                    py_str(result.get("revision"))
                ),
                None,
                Some(&operation_id),
            );
            if resolved.is_err() {
                if let Some(object) = result.as_object_mut() {
                    object.insert(
                        "audit_warning".into(),
                        json!("Policy was saved; its action row remains pending."),
                    );
                }
            }
            Ok(result)
        }
    }
}

// ── Saved views ──────────────────────────────────────────────────────────

/// `project_view_list`: the saved finding views.
pub fn project_view_list(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let directory = need_str(&args, "directory", "project-view-list", &["directory"])?;
    done(spiderhead_projects::finding_views::list_views(Path::new(
        directory,
    )))
}

/// `project_view_show`: one saved view.
pub fn project_view_show(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 2] = ["directory", "name"];
    let directory = need_str(&args, "directory", "project-view-show", &ACCEPTED)?;
    let name = need_str(&args, "name", "project-view-show", &ACCEPTED)?;
    done(spiderhead_projects::finding_views::show_view(
        Path::new(directory),
        name,
    ))
}

/// `project_view_save`: store a view definition, revision-gated.
pub fn project_view_save(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 3] = ["directory", "view", "expected_revision"];
    let directory = need_str(&args, "directory", "project-view-save", &ACCEPTED)?;
    let view = required_value(&args, "view", "project-view-save", &ACCEPTED)?;
    let expected = required_i64(&args, "expected_revision", "project-view-save", &ACCEPTED)?;
    done(spiderhead_projects::finding_views::save_view(
        Path::new(directory),
        view,
        expected,
    ))
}

/// `project_view_delete`: remove a saved view, revision-gated.
pub fn project_view_delete(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 3] = ["directory", "name", "expected_revision"];
    let directory = need_str(&args, "directory", "project-view-delete", &ACCEPTED)?;
    let name = need_str(&args, "name", "project-view-delete", &ACCEPTED)?;
    let expected = required_i64(&args, "expected_revision", "project-view-delete", &ACCEPTED)?;
    done(spiderhead_projects::finding_views::delete_view(
        Path::new(directory),
        name,
        expected,
    ))
}

/// `project_view_rename`: rename a saved view, revision-gated.
pub fn project_view_rename(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 4] = ["directory", "name", "new_name", "expected_revision"];
    let directory = need_str(&args, "directory", "project-view-rename", &ACCEPTED)?;
    let name = need_str(&args, "name", "project-view-rename", &ACCEPTED)?;
    let new_name = need_str(&args, "new_name", "project-view-rename", &ACCEPTED)?;
    let expected = required_i64(&args, "expected_revision", "project-view-rename", &ACCEPTED)?;
    done(spiderhead_projects::finding_views::rename_view(
        Path::new(directory),
        name,
        new_name,
        expected,
    ))
}

/// `findings_view`: apply a saved view to an audit document or scan.
pub fn findings_view(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 4] = ["directory", "name", "audit", "offset"];
    let directory = need_str(&args, "directory", "findings-view", &ACCEPTED)?;
    let name = need_str(&args, "name", "findings-view", &ACCEPTED)?;
    let audit = required_value(&args, "audit", "findings-view", &ACCEPTED)?;
    let (document, diagnostics) =
        spiderhead_store::inputs::resolve_audit_input(audit).map_err(super::failed)?;
    let mut result = done(spiderhead_projects::finding_views::apply_view_to_audit(
        Path::new(directory),
        name,
        &document,
        opt_int(&args, "offset").unwrap_or_default(),
    ))?;
    if let Some(object) = result.as_object_mut() {
        object.insert(
            "input_diagnostics".into(),
            Value::Array(diagnostics.into_iter().map(Value::Object).collect()),
        );
    }
    Ok(result)
}

// ── Competitor candidates ────────────────────────────────────────────────

/// `project_competitors_add`: record candidate workspaces from a list or one URL; crawls nothing.
pub fn project_competitors_add(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 3] = ["directory", "competitors", "url"];
    let directory = need_str(&args, "directory", "project-competitors-add", &ACCEPTED)?;
    let mut competitors = opt(&args, "competitors").cloned();
    if let Some(url) = opt(&args, "url") {
        if competitors.is_some() {
            return Err(ToolError::Failed(
                "pass either competitors or url, not both".to_owned(),
            ));
        }
        competitors = Some(json!([url]));
    }
    let list = match competitors {
        Some(list) if list.as_array().is_some_and(|rows| !rows.is_empty()) => list,
        _ => {
            return Err(ToolError::Failed(
                "competitors required: --input {\"competitors\": [...]} or one --url / {\"url\": \"...\"}"
                    .to_owned(),
            ))
        }
    };
    done(spiderhead_projects::runtime::add_competitors(
        Path::new(directory),
        &list,
    ))
}

/// `project_competitors_list`: the recorded candidate workspaces.
pub fn project_competitors_list(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let directory = need_str(
        &args,
        "directory",
        "project-competitors-list",
        &["directory"],
    )?;
    done(spiderhead_projects::runtime::list_competitors(Path::new(
        directory,
    )))
}

/// `project_competitors_remove`: drop one candidate by URL.
pub fn project_competitors_remove(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 2] = ["directory", "url"];
    let directory = need_str(&args, "directory", "project-competitors-remove", &ACCEPTED)?;
    let url = need_str(&args, "url", "project-competitors-remove", &ACCEPTED)?;
    done(spiderhead_projects::runtime::remove_competitor(
        Path::new(directory),
        url,
    ))
}

// ── Archives ─────────────────────────────────────────────────────────────

/// `project_archive`: write a portable project archive.
pub fn project_archive(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 6] = [
        "project",
        "out",
        "dry_run",
        "exclude_bodies",
        "exclude_bodies_older_than_days",
        "keep_newest_scans",
    ];
    let project = need_str(&args, "project", "project-archive", &ACCEPTED)?;
    let out = need_str(&args, "out", "project-archive", &ACCEPTED)?;
    done(spiderhead_projects::archive::archive_project(
        Path::new(project),
        Path::new(out),
        opt_bool(&args, "dry_run").unwrap_or(false),
        opt_bool(&args, "exclude_bodies").unwrap_or(false),
        opt_int(&args, "exclude_bodies_older_than_days"),
        opt_int(&args, "keep_newest_scans"),
    ))
}

/// `project_restore`: restore an archive into a new directory.
pub fn project_restore(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 2] = ["archive", "to"];
    let archive = need_str(&args, "archive", "project-restore", &ACCEPTED)?;
    let to = need_str(&args, "to", "project-restore", &ACCEPTED)?;
    done(spiderhead_projects::archive::restore_project(
        Path::new(archive),
        Path::new(to),
    ))
}

/// `project_archive_list`: describe and verify an archive without unpacking it.
pub fn project_archive_list(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let archive = need_str(&args, "archive", "project-archive-list", &["archive"])?;
    done(spiderhead_projects::archive::archive_list(Path::new(
        archive,
    )))
}

// ── Scheduled scans ──────────────────────────────────────────────────────

/// `schedule_show`: settings, last run and the next due time.
pub fn schedule_show(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let directory = need_str(&args, "directory", "schedule-show", &["directory"])?;
    done(spiderhead_projects::schedule::show(
        Path::new(directory),
        None,
    ))
}

/// `schedule_set`: merge a partial update into the schedule settings, revision-gated.
/// Any key besides the named ones is a settings field, like the reference `**fields`.
pub fn schedule_set(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 4] = ["directory", "expected_revision", "settings", "fields"];
    let directory = need_str(&args, "directory", "schedule-set", &ACCEPTED)?;
    let expected = required_i64(&args, "expected_revision", "schedule-set", &ACCEPTED)?;
    let fields: Map<String, Value> = args
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), "directory" | "expected_revision" | "settings"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    done(spiderhead_projects::schedule::configure(
        Path::new(directory),
        expected,
        opt(&args, "settings"),
        &fields,
    ))
}

/// `schedule_install`: write a launchd plist, cron line or Task Scheduler command.
pub fn schedule_install(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 4] = ["directory", "kind", "dest", "python"];
    let directory = need_str(&args, "directory", "schedule-install", &ACCEPTED)?;
    let kind = need_str(&args, "kind", "schedule-install", &ACCEPTED)?;
    done(spiderhead_projects::schedule_install::install(
        directory,
        kind,
        opt_str(&args, "dest").map(Path::new),
        opt_str(&args, "python"),
    ))
}

/// `schedule_uninstall`: remove the written trigger for one kind.
pub fn schedule_uninstall(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 3] = ["directory", "kind", "dest"];
    let directory = need_str(&args, "directory", "schedule-uninstall", &ACCEPTED)?;
    let kind = need_str(&args, "kind", "schedule-uninstall", &ACCEPTED)?;
    done(spiderhead_projects::schedule_install::uninstall(
        directory,
        kind,
        opt_str(&args, "dest").map(Path::new),
    ))
}

/// `schedule_run_due`: run this project's scheduled scans if they are due; refuses
/// overlap and out-of-window runs.
pub fn schedule_run_due(args: &Value) -> Result<Value, ToolError> {
    use spiderhead_projects::schedule::ScanFailure;
    let args = kwargs(args);
    let directory = need_str(&args, "directory", "schedule-run-due", &["directory"])?;
    let scan = |request: &Value| -> Result<Value, ScanFailure> {
        super::crawl::crawl_site(request).map_err(|error| ScanFailure {
            cancelled: false,
            name: "ValueError".to_owned(),
            message: Some(error.to_string()),
        })
    };
    done(spiderhead_projects::schedule::run_due(
        Path::new(directory),
        &scan,
        None,
    ))
}

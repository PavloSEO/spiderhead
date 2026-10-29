//! Handlers of the project group (the reference suite) whose domain is
//! the `spiderhead-projects` crate: workspace creation and lifecycle, the
//! sources bindings, the inbox, and the export jobs.

use std::path::Path;

use serde_json::Value;

use crate::error::ToolError;

use super::{kwargs, missing_option, need_str, opt, opt_int, opt_str};

fn done(
    result: Result<Value, spiderhead_projects::error::ProjectsError>,
) -> Result<Value, ToolError> {
    result.map_err(|error| ToolError::Failed(error.to_string()))
}

/// The handler wrapper's fire-and-forget action-log row: reference
/// `_record_project_action` swallows `OSError`/`ValueError`.
fn record_action(directory: &Path, action: &str, outcome: &str, detail: &str) {
    let _ = spiderhead_projects::action_log::record_interface_action(
        directory, action, outcome, detail, None, None,
    );
}

/// `project_new`: create a project workspace for a target site.
pub fn project_new(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 6] = [
        "directory",
        "target",
        "label",
        "facts",
        "template_references",
        "profile_references",
    ];
    let directory = need_str(&args, "directory", "project-new", &ACCEPTED)?;
    let target = need_str(&args, "target", "project-new", &ACCEPTED)?;
    done(spiderhead_projects::workspace::create_project(
        Path::new(directory),
        target,
        args.get("label").and_then(Value::as_str),
        args.get("facts").filter(|value| !value.is_null()),
        args.get("template_references")
            .filter(|value| !value.is_null()),
        args.get("profile_references")
            .filter(|value| !value.is_null()),
    ))
}

/// `project_open`: open (and validate) an existing workspace.
pub fn project_open(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 2] = ["directory", "expected_site"];
    let directory = need_str(&args, "directory", "project-open", &ACCEPTED)?;
    done(spiderhead_projects::workspace::open_project(
        Path::new(directory),
        args.get("expected_site").and_then(Value::as_str),
    ))
}

/// `project_status`: the read-only workspace document.
pub fn project_status(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 2] = ["directory", "consumer"];
    let directory = need_str(&args, "directory", "project-status", &ACCEPTED)?;
    let result = done(spiderhead_projects::workspace::project_status(
        Path::new(directory),
        None,
        true,
    ))?;
    super::project_ops::with_inbox_notice(result, Path::new(directory), opt_str(&args, "consumer"))
}

/// `project_update`: rename a workspace label, revision-gated.
pub fn project_update(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 3] = ["directory", "label", "expected_revision"];
    let directory = need_str(&args, "directory", "project-update", &ACCEPTED)?;
    let label = need_str(&args, "label", "project-update", &ACCEPTED)?;
    let expected_revision = args
        .get("expected_revision")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    done(spiderhead_projects::project_settings::update_label(
        Path::new(directory),
        label,
        expected_revision,
        "cli",
        None,
        true,
    ))
}

/// `project_delete`: remove a workspace and its exports.
pub fn project_delete(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let directory = need_str(&args, "directory", "project-delete", &["directory"])?;
    done(spiderhead_projects::project_settings::delete_project(
        Path::new(directory),
        "cli",
    ))
}

/// `project_sources_link`: bind one provider resource to the workspace.
pub fn project_sources_link(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 4] = ["directory", "service", "resource", "label"];
    let directory = need_str(&args, "directory", "project-sources-link", &ACCEPTED)?;
    let service = need_str(&args, "service", "project-sources-link", &ACCEPTED)?;
    let resource = need_str(&args, "resource", "project-sources-link", &ACCEPTED)?;
    done(spiderhead_projects::bindings::link(
        Path::new(directory),
        service,
        resource,
        args.get("label").and_then(Value::as_str),
    ))
}

/// `project_sources_unlink`: remove one binding.
pub fn project_sources_unlink(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 3] = ["directory", "service", "resource"];
    let directory = need_str(&args, "directory", "project-sources-unlink", &ACCEPTED)?;
    let service = need_str(&args, "service", "project-sources-unlink", &ACCEPTED)?;
    let resource = need_str(&args, "resource", "project-sources-unlink", &ACCEPTED)?;
    done(spiderhead_projects::bindings::unlink(
        Path::new(directory),
        service,
        resource,
    ))
}

/// `project_sources_list`: the bindings page.
pub fn project_sources_list(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let directory = need_str(&args, "directory", "project-sources-list", &["directory"])?;
    done(spiderhead_projects::bindings::list_bindings(
        Path::new(directory),
        None,
    ))
}

// ── Inbox ────────────────────────────────────────────────────────────────

/// `project_action_log`: a bounded newest-first page of the interface actions.
pub fn project_action_log(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 6] = ["directory", "offset", "limit", "client", "outcome", "query"];
    let directory = need_str(&args, "directory", "project-action-log", &ACCEPTED)?;
    done(spiderhead_projects::action_log::page(
        Path::new(directory),
        opt_int(&args, "offset").unwrap_or_default(),
        opt_int(&args, "limit").unwrap_or(50),
        opt_str(&args, "client"),
        opt_str(&args, "outcome"),
        opt_str(&args, "query").unwrap_or_default(),
    ))
}

/// `project_inbox_submit`: store a note, question or proposed goal; the wrapper
/// records the interface action after the entry lands.
pub fn project_inbox_submit(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 6] = [
        "directory",
        "text",
        "kind",
        "references",
        "author_role",
        "expected_revision",
    ];
    let directory = need_str(&args, "directory", "project-inbox-submit", &ACCEPTED)?;
    let text = need_str(&args, "text", "project-inbox-submit", &ACCEPTED)?;
    let kind = opt_str(&args, "kind").unwrap_or("note");
    let author_role = opt_str(&args, "author_role").unwrap_or("specialist");
    let expected_revision = opt_int(&args, "expected_revision");
    let result = done(spiderhead_projects::inbox::submit(
        Path::new(directory),
        text,
        kind,
        opt(&args, "references"),
        author_role,
        expected_revision,
    ))?;
    let outcome = if kind == "proposed_goal" {
        "pending"
    } else {
        "applied"
    };
    record_action(
        Path::new(directory),
        "project.inbox.submit",
        outcome,
        &format!("Recorded project inbox {kind}"),
    );
    Ok(result)
}

/// `project_inbox_list`: a bounded page of entries for one consumer.
pub fn project_inbox_list(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 5] = [
        "directory",
        "consumer",
        "offset",
        "limit",
        "include_acknowledged",
    ];
    let directory = need_str(&args, "directory", "project-inbox-list", &ACCEPTED)?;
    let consumer = need_str(&args, "consumer", "project-inbox-list", &ACCEPTED)?;
    done(spiderhead_projects::inbox::list_entries(
        Path::new(directory),
        consumer,
        opt_int(&args, "offset").unwrap_or_default(),
        opt_int(&args, "limit").unwrap_or(20),
        opt(&args, "include_acknowledged")
            .and_then(Value::as_bool)
            .unwrap_or(true),
    ))
}

/// `project_inbox_read`: mark entries as inspected by one consumer.
pub fn project_inbox_read(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 4] = ["directory", "consumer", "entry_ids", "expected_revision"];
    let directory = need_str(&args, "directory", "project-inbox-read", &ACCEPTED)?;
    let consumer = need_str(&args, "consumer", "project-inbox-read", &ACCEPTED)?;
    let Some(entry_ids) = opt(&args, "entry_ids") else {
        return Err(missing_option("project-inbox-read", "entry_ids", &ACCEPTED));
    };
    done(spiderhead_projects::inbox::mark_read(
        Path::new(directory),
        consumer,
        entry_ids,
        opt_int(&args, "expected_revision"),
    ))
}

/// `project_inbox_acknowledge`: consume entries so they stop counting as unread.
pub fn project_inbox_acknowledge(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 4] = ["directory", "consumer", "entry_ids", "expected_revision"];
    let directory = need_str(&args, "directory", "project-inbox-acknowledge", &ACCEPTED)?;
    let consumer = need_str(&args, "consumer", "project-inbox-acknowledge", &ACCEPTED)?;
    let Some(entry_ids) = opt(&args, "entry_ids") else {
        return Err(missing_option(
            "project-inbox-acknowledge",
            "entry_ids",
            &ACCEPTED,
        ));
    };
    done(spiderhead_projects::inbox::acknowledge(
        Path::new(directory),
        consumer,
        entry_ids,
        opt_int(&args, "expected_revision"),
    ))
}

/// `project_inbox_goal`: move an accepted goal's state. Both wrapper layers
/// record an interface action — the inner `entry_id=…` row (denied on failure),
/// then the outer `Updated owner goal state to …` row.
pub fn project_inbox_goal(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 4] = ["directory", "entry_id", "state", "expected_revision"];
    let directory = need_str(&args, "directory", "project-inbox-goal", &ACCEPTED)?;
    let entry_id = need_str(&args, "entry_id", "project-inbox-goal", &ACCEPTED)?;
    let state = need_str(&args, "state", "project-inbox-goal", &ACCEPTED)?;
    let result = spiderhead_projects::inbox::set_goal_state(
        Path::new(directory),
        entry_id,
        state,
        opt_int(&args, "expected_revision"),
    );
    match result {
        Ok(document) => {
            record_action(
                Path::new(directory),
                &format!("project.goal.{state}"),
                "applied",
                &format!("entry_id={entry_id}"),
            );
            record_action(
                Path::new(directory),
                &format!("project.goal.{state}"),
                "applied",
                &format!("Updated owner goal state to {state}"),
            );
            Ok(document)
        }
        Err(error) => {
            let (name, message) = spiderhead_projects::error::py_exception_name(&error);
            record_action(
                Path::new(directory),
                &format!("project.goal.{state}"),
                "denied",
                name,
            );
            Err(ToolError::Failed(message))
        }
    }
}

/// `project_inbox_triage`: append an explicit note outcome without accepting it.
pub fn project_inbox_triage(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 5] = [
        "directory",
        "entry_id",
        "outcome",
        "actor",
        "expected_revision",
    ];
    let directory = need_str(&args, "directory", "project-inbox-triage", &ACCEPTED)?;
    let entry_id = need_str(&args, "entry_id", "project-inbox-triage", &ACCEPTED)?;
    let actor = need_str(&args, "actor", "project-inbox-triage", &ACCEPTED)?;
    let Some(outcome) = opt(&args, "outcome") else {
        return Err(missing_option("project-inbox-triage", "outcome", &ACCEPTED));
    };
    done(spiderhead_projects::inbox::triage(
        Path::new(directory),
        entry_id,
        outcome,
        actor,
        opt_int(&args, "expected_revision"),
    ))
}

/// `project_inbox_unread`: bounded unread references; never consumes messages.
pub fn project_inbox_unread(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 3] = ["directory", "consumer", "limit"];
    let directory = need_str(&args, "directory", "project-inbox-unread", &ACCEPTED)?;
    let consumer = need_str(&args, "consumer", "project-inbox-unread", &ACCEPTED)?;
    done(spiderhead_projects::inbox::unread_summary(
        Path::new(directory),
        consumer,
        opt_int(&args, "limit").unwrap_or(10),
    ))
}

// ── Export jobs ──────────────────────────────────────────────────────────

/// `project_export`: start (and by default await) one dataset export job; the
/// wrapper records the denied/applied interface action around the engine call.
pub fn project_export(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 7] = [
        "directory",
        "dataset",
        "fmt",
        "output",
        "compare_manifest",
        "job_id",
        "wait",
    ];
    let directory = need_str(&args, "directory", "project-export", &ACCEPTED)?;
    let dataset = need_str(&args, "dataset", "project-export", &ACCEPTED)?;
    let fmt = need_str(&args, "fmt", "project-export", &ACCEPTED)?;
    let output = need_str(&args, "output", "project-export", &ACCEPTED)?;
    let wait = opt(&args, "wait").and_then(Value::as_bool).unwrap_or(true);
    let started = spiderhead_projects::exports::start_export(
        Path::new(directory),
        dataset,
        fmt,
        output,
        opt_str(&args, "compare_manifest"),
        opt_str(&args, "job_id"),
        wait,
    );
    match started {
        Ok(result) => {
            record_action(
                Path::new(directory),
                "project-export",
                "applied",
                &format!(
                    "dataset={dataset}; format={fmt}; job_id={}; state={}",
                    result
                        .get("job_id")
                        .and_then(Value::as_str)
                        .unwrap_or("None"),
                    result
                        .get("state")
                        .and_then(Value::as_str)
                        .unwrap_or("None"),
                ),
            );
            Ok(result)
        }
        Err(error) => {
            let (name, message) = spiderhead_projects::error::py_exception_name(&error);
            record_action(Path::new(directory), "project-export", "denied", name);
            Err(ToolError::Failed(message))
        }
    }
}

/// `project_export_status`: one job's state; the wrapper demands a UUID up front.
pub fn project_export_status(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 2] = ["directory", "job_id"];
    let directory = need_str(&args, "directory", "project-export-status", &ACCEPTED)?;
    let job_id = need_str(&args, "job_id", "project-export-status", &ACCEPTED)?;
    if uuid::Uuid::parse_str(job_id).is_err() {
        return Err(ToolError::Failed(format!(
            "job_id must be a UUID, as returned by project-export; got '{job_id}'"
        )));
    }
    done(spiderhead_projects::exports::export_status(
        Path::new(directory),
        job_id,
    ))
}

/// `project_export_cancel`: leave a `.cancel` marker for a live worker.
pub fn project_export_cancel(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 2] = ["directory", "job_id"];
    let directory = need_str(&args, "directory", "project-export-cancel", &ACCEPTED)?;
    let job_id = need_str(&args, "job_id", "project-export-cancel", &ACCEPTED)?;
    done(spiderhead_projects::exports::cancel_export(
        Path::new(directory),
        job_id,
    ))
}

/// `project_exports_page`: a bounded newest-first page of export jobs.
pub fn project_exports_page(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 3] = ["directory", "offset", "limit"];
    let directory = need_str(&args, "directory", "project-exports-page", &ACCEPTED)?;
    done(spiderhead_projects::exports::exports_page(
        Path::new(directory),
        opt_int(&args, "offset").unwrap_or_default(),
        opt_int(&args, "limit").unwrap_or(50),
    ))
}

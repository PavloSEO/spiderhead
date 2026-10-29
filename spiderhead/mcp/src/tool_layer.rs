//! What the typed MCP tool layer adds around a shared handler: owner-only actions
//! refused for lack of a trusted caller, and the optional inbox notice a `consumer`
//! argument attaches to a project tool's answer.

use std::path::Path;

use serde_json::{Map, Value};

use crate::error::ToolError;

/// Tools whose answer gains `inbox_unread` when the caller names a `consumer`.
const NOTICE_TOOLS: &[&str] = &[
    "seo_project_open",
    "seo_project_inbox_triage",
    "seo_project_facts",
    "seo_project_checklist_init",
    "seo_project_checklist_update",
    "seo_project_checklist_record",
    "seo_project_view_list",
    "seo_project_view_show",
    "seo_project_view_save",
    "seo_project_view_delete",
    "seo_project_view_rename",
    "seo_findings_view",
    "seo_project_priorities",
    "seo_project_policy",
    "seo_project_competitors_add",
    "seo_project_competitors_scan",
    "seo_project_prepare",
    "seo_project_start",
    "seo_project_task_comment",
    "seo_schedule_show",
    "seo_schedule_set",
    "seo_schedule_run_due",
    "seo_schedule_install",
    "seo_schedule_uninstall",
];

/// The owner-only action a call would perform, if any.
fn owner_action(tool: &str, args: &Map<String, Value>) -> Option<&'static str> {
    let truthy = |name: &str| match args.get(name) {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    };
    match tool {
        "seo_project_update" => Some("project.settings.update"),
        "seo_project_delete" => Some("project.delete"),
        "seo_project_settings_review" => Some("project.settings.review"),
        "seo_project_policy" if truthy("apply") => Some("project.policy.apply"),
        "seo_project_inbox_goal"
            if args.get("state").and_then(Value::as_str) == Some("accepted") =>
        {
            Some("project.goal.accept")
        }
        "seo_project_checklist_record" => {
            let record = args.get("record")?.as_object()?;
            if record.get("signoff") == Some(&Value::Bool(true)) {
                Some("project.checklist.signoff")
            } else if record.get("review").and_then(Value::as_str) == Some("approved") {
                Some("project.checklist.review")
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Log and reject an owner-only call: this server has no trusted principal.
pub fn refuse_owner_action(tool: &str, args: &Value) -> Result<(), ToolError> {
    let Some(object) = args.as_object() else {
        return Ok(());
    };
    let Some(action) = owner_action(tool, object) else {
        return Ok(());
    };
    let directory = object
        .get("directory")
        .and_then(Value::as_str)
        .unwrap_or_default();
    spiderhead_projects::action_log::record(
        Path::new(directory),
        "mcp",
        action,
        "refused",
        "Owner-only action is unavailable without a trusted MCP principal",
        None,
        None,
    )
    .map_err(|error| ToolError::Failed(error.to_string()))?;
    spiderhead_projects::permissions::require_mcp_allowed(action)
        .map_err(|error| ToolError::Failed(error.to_string()))
}

/// Remove the tool-layer `consumer` from the handler's arguments when the tool only
/// uses it for the notice; the consumer is returned for [`attach_notice`].
pub fn take_notice_consumer(tool: &str, args: &mut Value) -> Option<String> {
    if !NOTICE_TOOLS.contains(&tool) {
        return None;
    }
    let consumer = args.as_object_mut()?.remove("consumer")?;
    consumer.as_str().map(str::to_owned)
}

/// Attach a scoped inbox notice to a project tool's answer.
pub fn attach_notice(
    args: &Value,
    consumer: Option<String>,
    result: Value,
) -> Result<Value, ToolError> {
    let Some(consumer) = consumer else {
        return Ok(result);
    };
    let directory = args
        .get("directory")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let unread = crate::handlers::dispatch(
        "project_inbox_unread",
        &serde_json::json!({"directory": directory, "consumer": consumer}),
    )?;
    let mut result = result;
    if let Some(object) = result.as_object_mut() {
        object.insert("inbox_unread".into(), unread);
    }
    Ok(result)
}

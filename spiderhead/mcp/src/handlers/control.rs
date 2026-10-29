//! Handlers of the catalog and workflow-run commands: packaged skills and
//! scenarios, guided workflow runs over a project directory, and the closed
//! `audit_workflow` action set that composes the project and report handlers.

use std::path::Path;

use serde_json::{json, Value};

use crate::error::ToolError;

use super::{dispatch, kwargs, missing_option, need_str, opt, opt_bool, opt_int, opt_str};

/// A projects-domain result through the shared error mapping.
fn projects(
    result: Result<Value, spiderhead_projects::error::ProjectsError>,
) -> Result<Value, ToolError> {
    result.map_err(|error| ToolError::Failed(error.to_string()))
}

// ── Environment ──────────────────────────────────────────────────────────

/// `doctor`: local prerequisites, with no request and no write.
pub fn doctor(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let options = spiderhead_core::doctor::DoctorOptions {
        projects_directory: opt_str(&args, "projects_directory").map(str::to_owned),
        config_path: opt_str(&args, "config_path").map(str::to_owned),
        sf_cli: opt_str(&args, "sf_cli").map(str::to_owned),
        mcp_http_port: opt_int(&args, "mcp_http_port").and_then(|p| u16::try_from(p).ok()),
    };
    Ok(spiderhead_core::doctor::doctor(
        &spiderhead_core::host::SystemHost,
        &options,
    ))
}

// ── Tool catalog ─────────────────────────────────────────────────────────

/// The source-derived reference of every tool, generated from the tool definitions.
const TOOL_REFERENCE: &str = include_str!("../../data/tool_reference.json");

fn tool_reference() -> &'static [Value] {
    static TOOLS: std::sync::LazyLock<Vec<Value>> =
        std::sync::LazyLock::new(|| serde_json::from_str(TOOL_REFERENCE).unwrap_or_default());
    &TOOLS
}

/// `tool_catalog`: discover tool metadata without advertising every schema up front.
pub fn tool_catalog(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let query = match opt(&args, "query") {
        None => "",
        Some(value) => value.as_str().unwrap_or("\u{0}invalid"),
    };
    let int_arg = |name: &str, default: i64| match opt(&args, name) {
        None => Some(default),
        Some(value) => value.as_i64(),
    };
    let (limit, offset) = (int_arg("limit", 10), int_arg("offset", 0));
    let include_arguments = opt_bool(&args, "include_arguments").unwrap_or(false);
    let (Some(limit), Some(offset)) = (limit, offset) else {
        return Err(bounds_error());
    };
    if query.chars().count() > 500
        || query == "\u{0}invalid"
        || !(1..=50).contains(&limit)
        || offset < 0
    {
        return Err(bounds_error());
    }
    let folded = query.to_lowercase();
    let words: Vec<&str> = folded.split_whitespace().collect();
    let mut matches: Vec<Value> = Vec::new();
    for tool in tool_reference() {
        let text = format!(
            "{} {} {}",
            tool["name"].as_str().unwrap_or_default(),
            tool["summary"].as_str().unwrap_or_default(),
            tool["notes"].as_str().unwrap_or_default()
        )
        .to_lowercase();
        if !words.iter().all(|word| text.contains(word)) {
            continue;
        }
        let mut row = tool.clone();
        if !include_arguments {
            if let Some(object) = row.as_object_mut() {
                object.shift_remove("arguments");
                object.shift_remove("notes");
            }
        }
        matches.push(row);
    }
    let total = matches.len();
    let (offset, limit) = (offset as usize, limit as usize);
    let items: Vec<Value> = matches.into_iter().skip(offset).take(limit).collect();
    Ok(json!({
        "ok": true,
        "total": total,
        "items": items,
        "offset": offset,
        "limit": limit,
        "has_more": offset + limit < total,
        "access": "Use the matching startup profile or full profile for direct low-level calls; high-level workflows invoke their bounded steps internally.",
    }))
}

fn bounds_error() -> ToolError {
    ToolError::Failed(
        "query must be bounded text, limit must be 1..50, and offset must be non-negative"
            .to_owned(),
    )
}

// ── Packaged playbooks ───────────────────────────────────────────────────

/// `skill_list`: every packaged skill with its outline and declared metadata.
pub fn skill_list(_args: &Value) -> Result<Value, ToolError> {
    projects(spiderhead_projects::playbooks::playbook_list(Some("skill")))
}

/// `scenario_list`: every packaged scenario with its outline and declared metadata.
pub fn scenario_list(_args: &Value) -> Result<Value, ToolError> {
    projects(spiderhead_projects::playbooks::playbook_list(Some(
        "scenario",
    )))
}

/// `skill_show`: one skill by full identifier or unambiguous name.
pub fn skill_show(args: &Value) -> Result<Value, ToolError> {
    show(args, "skill-show", "skill")
}

/// `scenario_show`: one scenario by full identifier or unambiguous name.
pub fn scenario_show(args: &Value) -> Result<Value, ToolError> {
    show(args, "scenario-show", "scenario")
}

fn show(args: &Value, command: &str, kind: &str) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let name = need_str(&args, "name", command, &["name"])?;
    projects(spiderhead_projects::playbooks::playbook_show(name, kind))
}

// ── Workflow runs ────────────────────────────────────────────────────────

/// `workflow_start`: register one guided workflow run over checklist steps.
pub fn workflow_start(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 5] = [
        "directory",
        "scenario_id",
        "steps",
        "expected_revision",
        "context",
    ];
    let directory = need_str(&args, "directory", "workflow-start", &ACCEPTED)?;
    let Some(scenario_id) = opt(&args, "scenario_id") else {
        return Err(missing_option("workflow-start", "scenario_id", &ACCEPTED));
    };
    let Some(steps) = opt(&args, "steps") else {
        return Err(missing_option("workflow-start", "steps", &ACCEPTED));
    };
    projects(spiderhead_projects::execution::start(
        Path::new(directory),
        scenario_id,
        steps,
        opt_int(&args, "expected_revision").unwrap_or_default(),
        opt(&args, "context").unwrap_or(&Value::Null),
    ))
}

/// `workflow_checkpoint`: record one step outcome in registered order.
pub fn workflow_checkpoint(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 8] = [
        "directory",
        "run_id",
        "step_id",
        "state",
        "evidence",
        "expected_revision",
        "review",
        "phase",
    ];
    let directory = need_str(&args, "directory", "workflow-checkpoint", &ACCEPTED)?;
    let run_id = need_str(&args, "run_id", "workflow-checkpoint", &ACCEPTED)?;
    let step_id = need_str(&args, "step_id", "workflow-checkpoint", &ACCEPTED)?;
    let state = need_str(&args, "state", "workflow-checkpoint", &ACCEPTED)?;
    let Some(expected_revision) = opt_int(&args, "expected_revision") else {
        return Err(missing_option(
            "workflow-checkpoint",
            "expected_revision",
            &ACCEPTED,
        ));
    };
    projects(spiderhead_projects::execution::checkpoint(
        Path::new(directory),
        run_id,
        step_id,
        state,
        opt(&args, "evidence").unwrap_or(&Value::Null),
        expected_revision,
        opt(&args, "review").unwrap_or(&Value::Null),
        opt_str(&args, "phase"),
    ))
}

/// `workflow_status`: every run with live dependency staleness.
pub fn workflow_status(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let directory = need_str(&args, "directory", "workflow-status", &["directory"])?;
    projects(spiderhead_projects::execution::status(
        Path::new(directory),
        None,
        true,
    ))
}

/// `workflow_execute`: checkpoint supplied local outcomes in one call.
pub fn workflow_execute(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 5] = ["directory", "scenario_id", "steps", "outcomes", "context"];
    let directory = need_str(&args, "directory", "workflow-execute", &ACCEPTED)?;
    let Some(scenario_id) = opt(&args, "scenario_id") else {
        return Err(missing_option("workflow-execute", "scenario_id", &ACCEPTED));
    };
    let Some(steps) = opt(&args, "steps") else {
        return Err(missing_option("workflow-execute", "steps", &ACCEPTED));
    };
    let Some(outcomes) = opt(&args, "outcomes") else {
        return Err(missing_option("workflow-execute", "outcomes", &ACCEPTED));
    };
    projects(spiderhead_projects::execution::execute(
        Path::new(directory),
        scenario_id,
        steps,
        outcomes,
        opt(&args, "context").unwrap_or(&Value::Null),
    ))
}

/// `workflow_resume`: deliberately reopen the first non-successful step.
pub fn workflow_resume(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 3] = ["directory", "run_id", "expected_revision"];
    let directory = need_str(&args, "directory", "workflow-resume", &ACCEPTED)?;
    let run_id = need_str(&args, "run_id", "workflow-resume", &ACCEPTED)?;
    let Some(expected_revision) = opt_int(&args, "expected_revision") else {
        return Err(missing_option(
            "workflow-resume",
            "expected_revision",
            &ACCEPTED,
        ));
    };
    projects(spiderhead_projects::execution::resume(
        Path::new(directory),
        run_id,
        expected_revision,
    ))
}

// ── Audit workflow ───────────────────────────────────────────────────────

/// `audit_workflow`: the closed status/start/prepare/report action set, each
/// action answered by the handler it names.
pub fn audit_workflow(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 10] = [
        "directory",
        "action",
        "target",
        "competitors",
        "template",
        "audit",
        "fmt",
        "out",
        "approve_large_crawl",
        "max_urls",
    ];
    let directory = need_str(&args, "directory", "audit-workflow", &ACCEPTED)?;
    match opt_str(&args, "action").unwrap_or("status") {
        "status" => dispatch("project_status", &json!({"directory": directory})),
        "start" => {
            let Some(target) = opt_str(&args, "target").filter(|target| !target.is_empty()) else {
                return Err(ToolError::Failed("start requires a target URL".to_owned()));
            };
            let mut call = json!({"directory": directory, "target": target});
            carry(&args, &mut call, &["template", "competitors"]);
            carry_bool(&args, &mut call, "approve_large_crawl");
            dispatch("project_start", &call)
        }
        "prepare" => {
            let mut call = json!({"directory": directory});
            carry(&args, &mut call, &["template", "competitors", "max_urls"]);
            carry_bool(&args, &mut call, "approve_large_crawl");
            dispatch("project_prepare", &call)
        }
        "report" => {
            let audit = match opt(&args, "audit") {
                Some(audit) => audit.clone(),
                None => json!(latest_project_scan(directory)?),
            };
            let mut call = json!({
                "audit": audit,
                "fmt": opt_str(&args, "fmt").unwrap_or("md"),
                "project": directory,
            });
            carry(&args, &mut call, &["out"]);
            dispatch("report_build", &call)
        }
        _ => Err(ToolError::Failed(
            "action must be status, start, prepare, or report".to_owned(),
        )),
    }
}

/// The newest retained scan of the project, as the path the report reads.
fn latest_project_scan(directory: &str) -> Result<String, ToolError> {
    spiderhead_projects::runtime::latest_project_scan(Path::new(directory))
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|error| ToolError::Failed(error.to_string()))
}

fn carry(from: &serde_json::Map<String, Value>, to: &mut Value, names: &[&str]) {
    for name in names {
        if let Some(value) = opt(from, name) {
            to[*name] = value.clone();
        }
    }
}

fn carry_bool(from: &serde_json::Map<String, Value>, to: &mut Value, name: &str) {
    if let Some(value) = opt_bool(from, name) {
        to[name] = Value::Bool(value);
    }
}

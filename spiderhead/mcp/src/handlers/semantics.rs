//! Handlers of the semantic core MCP tools (the reference suite): each one
//! is `runner.run_quiet` under a fixed stage allow-list, and the shared
//! `_checked` boundary turns an `ok: false` document into a tool error.

use serde_json::{json, Value};

use spiderhead_semantics::runner::{self, Options};

use crate::error::ToolError;

use super::kwargs;

type Opts = Options;

/// `seo_semantics_status`: read-only counters and the next stage.
pub fn semantics_status(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(project) = args.get("project").and_then(Value::as_str) else {
        return Err(ToolError::Failed("project is required".to_owned()));
    };
    checked(run_quiet_value(
        "status",
        project,
        &["status"],
        false,
        &Opts::defaults(),
    ))
}

/// `seo_semantics_run`: one free stage, or an import/export preview.
pub fn semantics_run(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(stage) = args.get("stage").and_then(Value::as_str) else {
        return Err(ToolError::Failed("stage is required".to_owned()));
    };
    let Some(project) = args.get("project").and_then(Value::as_str) else {
        return Err(ToolError::Failed("project is required".to_owned()));
    };
    // reference: LOCAL_STAGES minus the read models and status (each has its own
    // tool or is refused); `mine` is the separate web tool. The printing stages
    // (clean/graph/competitors/report/import) would corrupt a stdio session
    // with their progress lines, so this build refuses them here.
    const ALLOWED: [&str; 5] = ["init", "export", "excel", "sitematch", "relevance"];
    let mut options = Opts::defaults();
    options.out = opt_str(&args, "out").map(str::to_owned);
    options.file = opt_str(&args, "file").map(str::to_owned);
    options.sheet = opt_str(&args, "sheet").map(str::to_owned);
    options.mapping = args.get("mapping").and_then(Value::as_object).cloned();
    options.merge = opt_str(&args, "merge").unwrap_or("add").to_owned();
    options.scope = opt_str(&args, "scope").unwrap_or("all").to_owned();
    options.dry_run = opt_bool(&args, "dry_run").unwrap_or(false);
    checked(run_quiet_value(stage, project, &ALLOWED, false, &options))
}

/// `seo_semantics_paid`: the offline estimate, or a refusal on a real run.
pub fn semantics_paid(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(stage) = args.get("stage").and_then(Value::as_str) else {
        return Err(ToolError::Failed("stage is required".to_owned()));
    };
    let Some(project) = args.get("project").and_then(Value::as_str) else {
        return Err(ToolError::Failed("project is required".to_owned()));
    };
    let mut options = Opts::defaults();
    options.dry_run = opt_bool(&args, "dry_run").unwrap_or(false);
    options.resume = opt_bool(&args, "resume").unwrap_or(false);
    options.max_seeds = args.get("max_seeds").and_then(Value::as_i64).unwrap_or(0);
    options.max_phrases = args
        .get("max_phrases")
        .and_then(Value::as_i64)
        .unwrap_or(100_000);
    options.limit = args.get("limit").and_then(Value::as_i64).unwrap_or(0);
    options.yes = opt_bool(&args, "yes").unwrap_or(false);
    options.method = opt_str(&args, "method").unwrap_or("serp").to_owned();
    let confirm_paid = opt_bool(&args, "confirm_paid").unwrap_or(false);
    checked(run_quiet_value(
        stage,
        project,
        &runner::PAID_STAGES,
        confirm_paid,
        &options,
    ))
}

fn run_quiet_value(
    stage: &str,
    project: &str,
    allowed: &[&str],
    confirm_paid: bool,
    options: &Options,
) -> Value {
    let allowed_static: Vec<&str> = allowed.to_vec();
    runner::run_quiet(stage, project, &allowed_static, confirm_paid, options)
}

/// `mcp_server._checked`: an `ok: false` reply becomes an isError result whose
/// text is the compact document.
fn checked(document: Value) -> Result<Value, ToolError> {
    if document.get("ok") == Some(&json!(false)) {
        return Err(ToolError::Failed(
            spiderhead_core::common::python_json_insertion(&document),
        ));
    }
    Ok(document)
}

fn opt_str<'a>(args: &'a serde_json::Map<String, Value>, name: &str) -> Option<&'a str> {
    args.get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn opt_bool(args: &serde_json::Map<String, Value>, name: &str) -> Option<bool> {
    args.get(name).and_then(Value::as_bool)
}

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

/// The read-only read-model stages answer as JSON documents.
fn read_options(args: &serde_json::Map<String, Value>) -> Opts {
    let mut options = Opts::defaults();
    options.json = true;
    options.offset = args.get("offset").and_then(Value::as_i64).unwrap_or(0);
    options.limit = args.get("limit").and_then(Value::as_i64).unwrap_or(50);
    options.cluster = opt_str(args, "cluster").map(str::to_owned);
    options.url = args.get("url").and_then(Value::as_str).map(str::to_owned);
    options
}

/// `seo_semantics_clusters`: paginated clusters and nullable frequency aggregates.
pub fn semantics_clusters(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(project) = args.get("project").and_then(Value::as_str) else {
        return Err(ToolError::Failed("project is required".to_owned()));
    };
    let options = read_options(&args);
    checked(run_quiet_value(
        "clusters",
        project,
        &["clusters"],
        false,
        &options,
    ))
}

/// `seo_semantics_cluster_show`: one cluster, its phrases aggregate and landings.
pub fn semantics_cluster_show(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(project) = args.get("project").and_then(Value::as_str) else {
        return Err(ToolError::Failed("project is required".to_owned()));
    };
    if opt_str(&args, "cluster").is_none() {
        return Err(ToolError::Failed("cluster is required".to_owned()));
    }
    let options = read_options(&args);
    checked(run_quiet_value(
        "cluster-show",
        project,
        &["cluster-show"],
        false,
        &options,
    ))
}

/// `seo_semantics_cluster_queries`: one bounded page of a cluster's phrases.
pub fn semantics_cluster_queries(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(project) = args.get("project").and_then(Value::as_str) else {
        return Err(ToolError::Failed("project is required".to_owned()));
    };
    if opt_str(&args, "cluster").is_none() {
        return Err(ToolError::Failed("cluster is required".to_owned()));
    }
    let options = read_options(&args);
    checked(run_quiet_value(
        "cluster-queries",
        project,
        &["cluster-queries"],
        false,
        &options,
    ))
}

/// `seo_semantics_set_landing`: set or clear one cluster landing URL.
pub fn semantics_set_landing(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(project) = args.get("project").and_then(Value::as_str) else {
        return Err(ToolError::Failed("project is required".to_owned()));
    };
    if opt_str(&args, "cluster").is_none() || args.get("url").and_then(Value::as_str).is_none() {
        return Err(ToolError::Failed("cluster and url are required".to_owned()));
    }
    let options = read_options(&args);
    checked(run_quiet_value(
        "set-landing",
        project,
        &["set-landing"],
        false,
        &options,
    ))
}

/// `seo_semantics_mine`: competitor-page n-grams into synonyms.txt.
pub fn semantics_mine(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(project) = args.get("project").and_then(Value::as_str) else {
        return Err(ToolError::Failed("project is required".to_owned()));
    };
    let mut options = Opts::defaults();
    options.out = opt_str(&args, "out").map(str::to_owned);
    checked(run_quiet_value("mine", project, &["mine"], false, &options))
}

/// The comma-joined evidence sources of the insight tools.
fn insight_sources(args: &serde_json::Map<String, Value>) -> String {
    let names: Vec<&str> = args
        .get("sources")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if names.is_empty() {
        "gsc,metrika,webmaster".to_owned()
    } else {
        names.join(",")
    }
}

/// `seo_semantics_insights`: period-scoped rules and source evidence, offline.
pub fn semantics_insights(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(project) = args.get("project").and_then(Value::as_str) else {
        return Err(ToolError::Failed("project is required".to_owned()));
    };
    let Some(period) = opt_str(&args, "period") else {
        return Err(ToolError::Failed("period is required".to_owned()));
    };
    let mut options = read_options(&args);
    options.period = Some(period.to_owned());
    options.sources = Some(insight_sources(&args));
    checked(run_quiet_value(
        "insights",
        project,
        &["insights"],
        false,
        &options,
    ))
}

/// `seo_semantics_insights_tasks`: preview or write selected insight tasks.
pub fn semantics_insights_tasks(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(project) = args.get("project").and_then(Value::as_str) else {
        return Err(ToolError::Failed("project is required".to_owned()));
    };
    let Some(period) = opt_str(&args, "period") else {
        return Err(ToolError::Failed("period is required".to_owned()));
    };
    let Some(selections) = args.get("selections").and_then(Value::as_array).cloned() else {
        return Err(ToolError::Failed("selections is required".to_owned()));
    };
    let Some(expected_revision) = args.get("expected_revision").and_then(Value::as_i64) else {
        return Err(ToolError::Failed(
            "expected_revision is required".to_owned(),
        ));
    };
    let mut options = read_options(&args);
    options.period = Some(period.to_owned());
    options.sources = Some(insight_sources(&args));
    options.selections = Some(selections);
    options.expected_revision = Some(expected_revision);
    options.confirm = opt_bool(&args, "confirm").unwrap_or(false);
    options.dry_run = opt_bool(&args, "dry_run").unwrap_or(false);
    checked(run_quiet_value(
        "insights-tasks",
        project,
        &["insights-tasks"],
        false,
        &options,
    ))
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

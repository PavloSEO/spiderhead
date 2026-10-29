//! Handlers that steer or follow project crawls: live run control, project
//! preparation and the competitor scan runs.

use std::path::Path;

use serde_json::{json, Value};

use spiderhead_projects::error::{py_exception_name, ProjectsError};
use spiderhead_projects::run_control::{
    checked_operator_urls, request, request_rate, request_scope_narrow, request_urls,
};

use crate::error::ToolError;

use super::{kwargs, missing_option, need_str, opt, opt_bool, opt_float, opt_int, opt_str};

fn done(result: Result<Value, ProjectsError>) -> Result<Value, ToolError> {
    result.map_err(|error| ToolError::Failed(error.to_string()))
}

/// `crawl_control`: pause, stop, re-rate or steer an active project crawl.
///
/// A saved (paused) artifact only takes the two offline steering actions: the
/// resume inputs it repeats are read from the artifact itself (`crawl_resume`).
pub fn crawl_control(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let action = opt_str(&args, "action").unwrap_or_default();
    let run_id = opt_str(&args, "run_id").filter(|text| !text.is_empty());
    if opt(&args, "scan").is_some() {
        let scan = opt_str(&args, "scan").unwrap_or_default();
        if run_id.is_some() {
            return Err(ToolError::Failed(
                "steer either a live run by run_id or a paused artifact by scan".to_owned(),
            ));
        }
        if action == "narrow-scope" {
            return super::crawl_resume::paused_narrow_scope(
                scan,
                opt_str(&args, "exclude").map(str::to_owned),
                opt_str(&args, "include").map(str::to_owned),
            );
        }
        if action == "add-urls" {
            return super::crawl_resume::paused_add_urls(scan, opt(&args, "urls"));
        }
        return Err(ToolError::Failed(
            "pause and stop apply to a live run, not a saved artifact".to_owned(),
        ));
    }
    let project = opt_str(&args, "project").filter(|text| !text.is_empty());
    let (Some(project), Some(run_id)) = (project, run_id) else {
        return Err(ToolError::Failed(
            "crawl-control needs a project and run_id for a live run, or scan for a paused artifact"
                .to_owned(),
        ));
    };
    let directory = Path::new(project);
    let rate = opt_float(&args, "rate");
    let concurrency = opt_int(&args, "concurrency");
    if action == "set_rate" {
        let Some(rate) = rate else {
            return Err(ToolError::Failed(
                "set_rate requires rate (URLs per second)".to_owned(),
            ));
        };
        return done(request_rate(directory, run_id, rate, concurrency));
    }
    if rate.is_some() || concurrency.is_some() {
        return Err(ToolError::Failed(
            "rate and concurrency apply only to action=set_rate".to_owned(),
        ));
    }
    match action {
        "narrow-scope" => done(request_scope_narrow(
            directory,
            run_id,
            opt_str(&args, "exclude"),
            opt_str(&args, "include"),
        )),
        "add-urls" => {
            let urls = opt(&args, "urls").cloned().unwrap_or(Value::Null);
            done(checked_operator_urls(&urls).and_then(|_| request_urls(directory, run_id, &urls)))
        }
        other => done(request(directory, run_id, other)),
    }
}

/// The handler wrapper's fire-and-forget action-log row.
fn record_action(directory: &Path, action: &str, outcome: &str, detail: &str) {
    let _ = spiderhead_projects::action_log::record_interface_action(
        directory, action, outcome, detail, None, None,
    );
}

/// The crawl handler the preparation and scan runs inject: a project crawl through
/// the shared `crawl_site` handler; its error text is the expected-failure reason.
fn project_crawl(
    project: &str,
    overrides: &Value,
    approve_large_crawl: bool,
    producer_build: Option<&str>,
) -> Result<Value, String> {
    let mut call = serde_json::Map::new();
    call.insert("project".into(), json!(project));
    call.insert("overrides".into(), overrides.clone());
    call.insert("approve_large_crawl".into(), json!(approve_large_crawl));
    if let Some(build) = producer_build {
        call.insert("producer_build".into(), json!(build));
    }
    match super::crawl::crawl_site(&Value::Object(call)) {
        Ok(result) => Ok(result),
        Err(ToolError::Failed(message)) => Err(message),
        Err(ToolError::NotYetPorted(name)) => Err(format!("{name} is not yet available")),
    }
}

/// `project_prepare`: the bounded preparation path (crawl, checklist, priorities).
pub fn project_prepare(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 6] = [
        "directory",
        "template",
        "competitors",
        "approve_large_crawl",
        "producer_build",
        "max_urls",
    ];
    let directory = need_str(&args, "directory", "project-prepare", &ACCEPTED)?;
    done(prepare(Path::new(directory), &args))
}

fn prepare(
    directory: &Path,
    args: &serde_json::Map<String, Value>,
) -> Result<Value, ProjectsError> {
    spiderhead_projects::runtime::prepare_project(
        directory,
        &project_crawl,
        opt(args, "template"),
        opt(args, "competitors"),
        opt_bool(args, "approve_large_crawl").unwrap_or(false),
        opt_str(args, "producer_build"),
        opt_int(args, "max_urls"),
    )
}

/// `project_start`: create a workspace, then prepare it; a preparation failure keeps
/// the created project and says how to continue.
pub fn project_start(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 7] = [
        "directory",
        "target",
        "facts",
        "template",
        "competitors",
        "approve_large_crawl",
        "producer_build",
    ];
    let directory = need_str(&args, "directory", "project-start", &ACCEPTED)?;
    let target = need_str(&args, "target", "project-start", &ACCEPTED)?;
    let created = done(spiderhead_projects::workspace::create_project(
        Path::new(directory),
        target,
        None,
        opt(&args, "facts"),
        None,
        None,
    ))?;
    match prepare(Path::new(directory), &args) {
        Ok(result) => Ok(result),
        Err(error) => Ok(json!({
            "ok": false,
            "error": error.to_string(),
            "project": created,
            "next": "Use project-prepare to continue the inspectable project",
        })),
    }
}

/// `project_competitors_scan`: run or resume a durable bounded competitor scan.
pub fn project_competitors_scan(args: &Value) -> Result<Value, ToolError> {
    use spiderhead_projects::competitor_scans::{
        scan_task_competitors, CompetitorTools, CrawlFault,
    };
    let args = kwargs(args);
    const ACCEPTED: [&str; 8] = [
        "directory",
        "task_id",
        "expected_revision",
        "confirm",
        "concurrency",
        "approve_large_crawl",
        "producer_build",
        "run_id",
    ];
    let directory = need_str(&args, "directory", "project-competitors-scan", &ACCEPTED)?;
    let task_id = need_str(&args, "task_id", "project-competitors-scan", &ACCEPTED)?;
    let expected = opt_int(&args, "expected_revision").ok_or_else(|| {
        missing_option("project-competitors-scan", "expected_revision", &ACCEPTED)
    })?;
    let confirm = opt(&args, "confirm")
        .and_then(Value::as_bool)
        .ok_or_else(|| missing_option("project-competitors-scan", "confirm", &ACCEPTED))?;
    let crawl = |kwargs: &serde_json::Map<String, Value>,
                 _progress: &spiderhead_projects::competitor_scans::SiteProgress|
     -> Result<Value, CrawlFault> {
        match super::crawl::crawl_site(&Value::Object(kwargs.clone())) {
            Ok(result) => Ok(result),
            Err(ToolError::Failed(message)) => Err(CrawlFault::Failed(message)),
            Err(ToolError::NotYetPorted(name)) => {
                Err(CrawlFault::Failed(format!("{name} is not yet available")))
            }
        }
    };
    let tools = CompetitorTools {
        crawl_site: &crawl,
        progress: None,
        cancel_requested: None,
    };
    let result = scan_task_competitors(
        Path::new(directory),
        task_id,
        expected,
        confirm,
        opt_int(&args, "concurrency").unwrap_or(3),
        opt_bool(&args, "approve_large_crawl").unwrap_or(false),
        opt_str(&args, "producer_build"),
        &tools,
        opt_str(&args, "run_id"),
    );
    match result {
        Ok(result) => {
            let outcome = if result
                .get("ok")
                .is_some_and(|ok| ok != &Value::Bool(false) && ok != &Value::Null)
            {
                "applied"
            } else {
                "refused"
            };
            let run = result
                .get("run_id")
                .and_then(Value::as_str)
                .unwrap_or("None");
            record_action(
                Path::new(directory),
                "competitor-scan",
                outcome,
                &format!("task_id={task_id}; run_id={run}"),
            );
            Ok(result)
        }
        Err(error) => {
            let (name, message) = py_exception_name(&error);
            record_action(Path::new(directory), "competitor-scan", "denied", name);
            Err(ToolError::Failed(message))
        }
    }
}

/// `project_competitors_scan_status`: one durable run's manifest.
pub fn project_competitors_scan_status(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 2] = ["directory", "run_id"];
    let directory = need_str(
        &args,
        "directory",
        "project-competitors-scan-status",
        &ACCEPTED,
    )?;
    let run_id = need_str(
        &args,
        "run_id",
        "project-competitors-scan-status",
        &ACCEPTED,
    )?;
    done(
        spiderhead_projects::competitor_scans::get_competitor_scan_run(
            Path::new(directory),
            run_id,
        ),
    )
}

/// `project_competitors_scan_cancel`: leave a cancel marker for a live run.
pub fn project_competitors_scan_cancel(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 2] = ["directory", "run_id"];
    let directory = need_str(
        &args,
        "directory",
        "project-competitors-scan-cancel",
        &ACCEPTED,
    )?;
    let run_id = need_str(
        &args,
        "run_id",
        "project-competitors-scan-cancel",
        &ACCEPTED,
    )?;
    match spiderhead_projects::competitor_scans::cancel_competitor_scan_run(
        Path::new(directory),
        run_id,
    ) {
        Ok(result) => {
            let outcome = if result.get("cancelled") == Some(&Value::Bool(true)) {
                "applied"
            } else {
                "refused"
            };
            record_action(
                Path::new(directory),
                "competitor-scan-cancel",
                outcome,
                &format!("run_id={run_id}"),
            );
            Ok(result)
        }
        Err(error) => {
            let (name, message) = py_exception_name(&error);
            record_action(
                Path::new(directory),
                "competitor-scan-cancel",
                "denied",
                name,
            );
            Err(ToolError::Failed(message))
        }
    }
}

/// `crawl_enrich`: join an existing crawl's pages to an offline traffic or search CSV.
///
/// The original audit is never changed. Every non-match direction is explicit, and a
/// list of reliable same-origin external-only URLs can be written as list-mode input;
/// a partial crawl cannot prove an orphan, so that list is refused.
pub fn crawl_enrich(args: &Value) -> Result<Value, ToolError> {
    use spiderhead_sources::analytics_findings::{analytics_findings, FindingOptions};
    use spiderhead_sources::external_join::{
        join_external_data, load_csv_rows, normalize_join_key, orphan_urls,
    };

    let args = kwargs(args);
    let Some(external_csv) = opt_str(&args, "external_csv").filter(|text| !text.is_empty()) else {
        return Err(ToolError::Failed("external_csv required".to_owned()));
    };
    let url_column = opt_str(&args, "url_column").unwrap_or("url");
    let failed =
        |error: spiderhead_sources::cloudflare::CfError| ToolError::Failed(error.to_string());
    let mut diagnostics = Vec::new();
    let audit = opt(&args, "audit").cloned().unwrap_or(Value::Null);
    let document =
        spiderhead_store::inputs::load_audit_document(&audit, "audit", Some(&mut diagnostics))
            .map_err(super::failed)?;
    let rows = load_csv_rows(external_csv, url_column).map_err(failed)?;
    let visits = opt_str(&args, "visits_column").filter(|text| !text.is_empty());
    let bounce = opt_str(&args, "bounce_column").filter(|text| !text.is_empty());
    for column in [visits, bounce].into_iter().flatten() {
        if rows.first().is_some_and(|row| row.get(column).is_none()) {
            return Err(ToolError::Failed(format!(
                "column '{column}' not found in '{external_csv}'"
            )));
        }
    }
    let (ignore_query, ignore_scheme, casefold) = (
        opt_bool(&args, "ignore_query").unwrap_or(false),
        opt_bool(&args, "ignore_scheme").unwrap_or(false),
        opt_bool(&args, "casefold_path").unwrap_or(false),
    );
    let key = |url: Option<&Value>| normalize_join_key(url, ignore_query, ignore_scheme, casefold);
    let pages: Vec<Value> = document
        .get("pages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let joined = join_external_data(&pages, &rows, "url", url_column, &key).map_err(failed)?;
    let partial = document["run"]
        .get("crawl_partial")
        .is_some_and(|value| value.as_bool().unwrap_or(false));
    let candidates = if partial {
        Vec::new()
    } else {
        orphan_urls(&joined, url_column, None).map_err(failed)?
    };
    let reason = if partial {
        "crawl is partial; external-only URLs are not labelled as orphans"
    } else {
        "crawl completed; external-only same-origin URLs can enter list mode"
    };
    let out_urls = opt_str(&args, "out_urls").filter(|text| !text.is_empty());
    if let Some(target) = out_urls {
        if partial {
            return Err(ToolError::Failed(
                "cannot write an orphan list from a partial crawl".to_owned(),
            ));
        }
        let target = Path::new(target);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| ToolError::Failed(error.to_string()))?;
        }
        let text: String = candidates.iter().map(|url| format!("{url}\n")).collect();
        spiderhead_core::filesystem::atomic_write_bytes(target, text.as_bytes(), None, None, true)
            .map_err(|error| ToolError::Failed(error.to_string()))?;
    }
    let findings = analytics_findings(
        &pages,
        &joined,
        &FindingOptions {
            partial,
            evidence_complete: None,
            evidence_reason: None,
            empty_is_complete: false,
            visits_column: visits.map(str::to_owned),
            bounce_column: bounce.map(str::to_owned),
            bounce_reason: None,
        },
    )
    .map_err(failed)?;
    let mut result = json!({
        "schema_version": "crawl_enrich.v1",
        "join": joined,
        "orphan_detection": {
            "state": if partial { "partial" } else { "complete" },
            "reason": reason,
            "urls": candidates,
        },
        "out_urls": out_urls,
        "analytics_findings": findings,
    });
    if !diagnostics.is_empty() {
        result["input_diagnostics"] =
            Value::Array(diagnostics.into_iter().map(Value::Object).collect());
    }
    Ok(result)
}

fn diagnose_request(
    args: &serde_json::Map<String, Value>,
) -> spiderhead_crawl::diagnostics::DiagnoseRequest {
    spiderhead_crawl::diagnostics::DiagnoseRequest {
        scan: opt_str(args, "scan").map(str::to_owned),
        run: opt_str(args, "run").map(str::to_owned),
        max_decisions: opt_int(args, "max_decisions"),
        export: opt_str(args, "export").map(str::to_owned),
    }
}

/// `crawl_diagnose`: explain a native crawl from retained evidence without fetching.
pub fn crawl_diagnose(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let mut request = diagnose_request(&args);
    request.export = None;
    spiderhead_crawl::diagnostics::diagnose(&request)
        .map_err(|error| ToolError::Failed(error.to_string()))
}

/// `crawl_diagnose_export`: as `crawl_diagnose`, also writing a new redacted file at an
/// explicit path.
pub fn crawl_diagnose_export(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let request = diagnose_request(&args);
    if request.export.as_deref().is_none_or(str::is_empty) {
        return Err(ToolError::Failed("export path is required".to_owned()));
    }
    spiderhead_crawl::diagnostics::diagnose(&request)
        .map_err(|error| ToolError::Failed(error.to_string()))
}

/// The stderr summary the CLI prints for `crawl-diagnose` and its export variant.
pub fn diagnose_summary_lines(result: &Value) -> Vec<String> {
    let text = |value: &Value| match value {
        Value::String(text) => text.clone(),
        Value::Null => "None".to_owned(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        other => other.to_string(),
    };
    let observed = &result["observed"];
    let shown = match observed["page_records"].as_i64() {
        Some(pages) => format!("{pages} URL records"),
        None => "unknown URL record count".to_owned(),
    };
    let duration = match observed["elapsed_seconds"].as_f64() {
        Some(elapsed) => format!("{elapsed:.1} s"),
        None => "unknown duration".to_owned(),
    };
    let finish = result["source"]["finish_reason"]
        .as_str()
        .filter(|text| !text.is_empty())
        .unwrap_or("unknown");
    let mut lines = vec![format!(
        "crawl-diagnose: {shown}; site total unknown; elapsed={duration}; finish={finish}"
    )];
    for finding in result["diagnoses"].as_array().into_iter().flatten() {
        lines.push(format!(
            "  {}: {} Next: {}",
            text(&finding["code"]),
            text(&finding["conclusion"]),
            text(&finding["next_step"])
        ));
    }
    for decision in result["decisions"]["sample"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let url = match decision["url"].as_str() {
            Some(url) => spiderhead_sources::pystr::repr_str(url),
            None => text(&decision["url"]),
        };
        lines.push(format!(
            "  decision #{}: {} url={url} depth={}",
            text(&decision["decision_id"]),
            text(&decision["reason"]),
            text(&decision["depth"])
        ));
    }
    lines
}

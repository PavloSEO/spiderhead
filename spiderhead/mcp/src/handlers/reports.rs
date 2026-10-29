//! Handlers of the reporting group (the reference suite,
//! `facts_export`) over the `spiderhead-reports` crate, and the crawl settings
//! descriptor (the reference suite).

use std::path::Path;

use serde_json::{json, Value};

use crate::error::ToolError;

use super::kwargs;

/// `crawl_describe_settings`: every crawl-site setting with type, default and
/// description — the same `describe_settings` the CLI's `--config-help` prints.
pub fn crawl_describe_settings(_args: &Value) -> Result<Value, ToolError> {
    Ok(json!({
        "settings": spiderhead_core::crawl_settings::describe_settings(),
        "capabilities": {"sitemap_only_retained": true, "full_site_native_sqlite": true},
    }))
}

/// `report_build`: one report document from an audit (a JSON object, or a path
/// to its JSON representation, native scan, or audit.v2 companion).
pub fn report_build(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 8] = [
        "audit",
        "fmt",
        "out",
        "project",
        "view",
        "offset",
        "lang",
        "pdf_policy",
    ];
    // reference passes `audit` through unchanged: an object stays a document, a
    // string goes to the loader, which resolves scans and audit.v2 companions.
    // The CLI's --report chain hands the audit over as serialized JSON text, so
    // a string that parses as JSON becomes the document; anything else keeps
    // the loader's path semantics.
    let data =
        args.get("audit")
            .filter(|value| !value.is_null())
            .map(|value| match value {
                Value::String(text) => serde_json::from_str::<Value>(text)
                    .unwrap_or_else(|_| Value::String(text.clone())),
                other => other.clone(),
            })
            .ok_or_else(|| {
                ToolError::Failed(format!(
                    "audit required: audit document or path to its JSON representation; \
                 accepted keys: {ACCEPTED:?}"
                ))
            })?;
    // The CLI flag's dest is `format`, the MCP tool argument is `fmt`.
    let fmt = args
        .get("fmt")
        .or_else(|| args.get("format"))
        .and_then(Value::as_str)
        .unwrap_or("xlsx")
        .to_owned();
    let out = args.get("out").and_then(Value::as_str).map(str::to_owned);
    let project = args
        .get("project")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let view = args.get("view").and_then(Value::as_str).map(str::to_owned);
    let offset = args.get("offset").and_then(Value::as_i64).unwrap_or(0);
    let lang = args
        .get("lang")
        .and_then(Value::as_str)
        .unwrap_or("en")
        .to_owned();
    let pdf_policy = args
        .get("pdf_policy")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let open_project = |directory: &str| -> Result<Value, String> {
        spiderhead_projects::workspace::open_project(Path::new(directory), None)
            .map_err(|e| e.to_string())
    };
    let project_status = |directory: &str| -> Result<Value, String> {
        spiderhead_projects::workspace::project_status(Path::new(directory), None, true)
            .map_err(|e| e.to_string())
    };
    let apply_view_details = |directory: &str, view: &str, document: &Value, offset: i64| {
        spiderhead_projects::finding_views::apply_view_details(
            Path::new(directory),
            view,
            document,
            offset,
        )
        .map(|applied| (applied.result, applied.selected_rows))
        .map_err(|e| e.to_string())
    };
    let host = spiderhead_reports::project_coverage::ProjectsHost {
        open_project: &open_project,
        project_status: &project_status,
        apply_view_details: &apply_view_details,
    };
    let mut result = spiderhead_reports::build_report::build_report(
        &data,
        &fmt,
        out.as_deref(),
        project.as_deref(),
        view.as_deref(),
        offset,
        &lang,
        pdf_policy.as_deref(),
        Some(&host),
    );
    if let (Some(project), true) = (project.as_deref(), result.get("ok") == Some(&json!(true))) {
        record_project_build(&mut result, project, &fmt, view.as_deref(), out.as_deref());
    }
    Ok(result)
}

/// The bookkeeping `build_report` performs once a project-bound build succeeds:
/// journal the run, record the export with its checksum, then log the action.
/// `export_history` reports the export row's own state, never hides a failure.
fn record_project_build(
    result: &mut Value,
    project: &str,
    fmt: &str,
    view: Option<&str>,
    requested: Option<&str>,
) {
    let root = Path::new(project);
    let target = result
        .get("path")
        .or_else(|| result.get("outputs"))
        .and_then(|v| {
            v.as_str()
                .map(str::to_owned)
                .or_else(|| Some(v.to_string()))
        })
        .or_else(|| requested.map(str::to_owned))
        .unwrap_or_default();
    let output = result
        .get("path")
        .and_then(Value::as_str)
        .map(str::to_owned);
    spiderhead_projects::journal::note(
        root,
        "agent",
        "agent",
        &format!("report {fmt} built: {target}"),
    );
    // `f"{type(exc).__name__}: {exc}"` — the reference class name of the failure.
    let classed = |e: spiderhead_projects::error::ProjectsError| {
        let (class, message) = spiderhead_projects::error::py_exception_name(&e);
        format!("{class}: {message}")
    };
    let export_state = (|| -> Result<(), String> {
        let digest = output
            .as_deref()
            .map(Path::new)
            .filter(|p| p.is_file() && !p.is_symlink())
            .map(sha256_file)
            .transpose()
            .map_err(|e| format!("OSError: {e}"))?;
        spiderhead_projects::exports::record_export(
            root,
            "audit",
            fmt,
            output.as_deref().unwrap_or(&target),
            result.get("findings").and_then(Value::as_i64).unwrap_or(0),
            result.get("bytes").and_then(Value::as_i64).unwrap_or(0),
            digest.as_deref(),
            Some(&json!({
                "view": view,
                "finding_count": result.get("findings").cloned().unwrap_or(Value::Null),
                "page_count": result.get("pages").cloned().unwrap_or(Value::Null),
                "path_state": if output.is_some() { "local_output" } else { "unknown" },
            })),
            None,
        )
        .map(|_| ())
        .map_err(classed)
    })();
    if let Value::Object(map) = result {
        map.insert(
            "export_history".to_owned(),
            match export_state {
                Ok(()) => json!({"state": "recorded"}),
                Err(reason) => json!({"state": "unavailable", "reason": reason}),
            },
        );
    }
    let detail = format!(
        "format={fmt}; output={}",
        output.unwrap_or("unavailable".into())
    );
    let _ = spiderhead_projects::action_log::record_interface_action(
        root,
        "report-build",
        "applied",
        &detail,
        None,
        None,
    );
}

/// sha256 hex of a local report artifact, streamed in bounded blocks.
fn sha256_file(path: &Path) -> Result<String, String> {
    use sha2::Digest;
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = sha2::Sha256::new();
    let mut block = [0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut block).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&block[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// `facts_export`: a facts.v1 multi-site export from already-produced audits;
/// zero network, zero winner-picking.
pub fn facts_export(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(sites) = args.get("sites").filter(|value| !value.is_null()) else {
        return Err(ToolError::Failed(
            "sites required: a list of {label, crawl_audit, site_audit} descriptors".to_owned(),
        ));
    };
    let sites: Vec<Value> = sites
        .as_array()
        .cloned()
        .unwrap_or_else(|| vec![sites.clone()]);
    match spiderhead_reports::facts::build_facts_export(&sites) {
        Ok(result) => {
            let mut document = json!({"ok": true});
            if let (Some(out), Some(object)) = (document.as_object_mut(), result.as_object()) {
                for (key, value) in object {
                    out.insert(key.clone(), value.clone());
                }
            }
            Ok(document)
        }
        Err(error) => Ok(json!({"ok": false, "error": error.to_string()})),
    }
}

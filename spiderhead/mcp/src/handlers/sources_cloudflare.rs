//! Cloudflare handlers (the reference suite): edge analytics by zone or
//! selected project binding, and the retained aggregate snapshot history.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use spiderhead_core::host::SystemHost;
use spiderhead_log_analyzer::import::{
    import_logs as logstore_import, ImportContext, ImportOptions,
};
use spiderhead_log_analyzer::system::free_space;
use spiderhead_sources::cloudflare::CfError;
use spiderhead_sources::cloudflare_history;
use spiderhead_sources::cloudflare_logs::{import_logpush_file, instant_logs_capture, LogsEnv};
use spiderhead_sources::cloudflare_traffic::{traffic, TrafficEnv};
use spiderhead_sources::http::Sleeper;
use spiderhead_sources::pystr;

use crate::error::ToolError;

use super::logs::{now, Workspaces};
use super::sources_net::{fail, present, run, transport};
use super::{kwargs, opt};

/// reference `~` expansion for the default token path.
fn home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => {
            let base = std::env::var("HOME").unwrap_or_default();
            PathBuf::from(base).join(rest)
        }
        None => PathBuf::from(path),
    }
}

/// `cloudflare_traffic`: bot and human edge traffic over an inclusive UTC range.
pub fn cloudflare_traffic(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let zone = present(&args, "zone");
    let project = present(&args, "project");
    if zone.is_some() == project.is_some() {
        return Err(fail("pass exactly one of zone or project"));
    }
    let host = SystemHost;
    let transport = transport()?;
    let bindings = |project: &str| {
        spiderhead_projects::bindings::list_bindings(Path::new(project), None)
            .map_err(|error| CfError::Python("ValueError", error.to_string()))
    };
    let history = |result: &Value, project: &str| {
        cloudflare_history::save_snapshot(result, &json!(project), None)
            .map_err(|error| CfError::Python(error.python_class(), error.to_string()))
    };
    let env = TrafficEnv {
        host: &host,
        home: &home,
        bindings: Some(&bindings),
        history: Some(&history),
        spend: None,
        today: None,
        page_size: None,
        max_url_facts: None,
        max_tracked_paths: None,
    };
    let zone_value = zone.map(|value| json!(value));
    let project_value = project.map(|value| json!(value));
    let include = opt(&args, "include_configuration")
        .cloned()
        .unwrap_or(json!(false));
    let since = opt(&args, "since").filter(|v| pystr::truthy(Some(v)));
    let until = opt(&args, "until").filter(|v| pystr::truthy(Some(v)));
    run(traffic(
        &env,
        &transport,
        &Sleeper::Real,
        zone_value.as_ref(),
        project_value.as_ref(),
        Some(&include),
        since,
        until,
        None,
        None,
    ))?
    .map_err(fail)
}

/// `cloudflare_logpush_import`: convert one supplied Logpush JSON Lines file
/// into the project's local logstore.
pub fn cloudflare_logpush_import(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let host = SystemHost;
    let seams = log_seams();
    let env = logs_env(&host, &seams);
    let input_path = opt(&args, "input_path").cloned().unwrap_or(Value::Null);
    let project = opt(&args, "project").cloned().unwrap_or(Value::Null);
    let max_events = opt(&args, "max_events").cloned();
    import_logpush_file(&env, &input_path, &project, max_events.as_ref()).map_err(fail)
}

/// `cloudflare_instant_logs`: one explicitly confirmed, bounded Instant Logs
/// session. The optional WebSocket dependency is not wired in this build, so a
/// confirmed request returns the reference `unavailable` envelope rather than a
/// fabricated capture.
pub fn cloudflare_instant_logs(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let host = SystemHost;
    let seams = log_seams();
    let env = logs_env(&host, &seams);
    let transport = transport()?;
    let project = opt(&args, "project").cloned().unwrap_or(Value::Null);
    run(instant_logs_capture(
        &env,
        &transport,
        &Sleeper::Real,
        &project,
        opt(&args, "confirm_live_capture"),
        opt(&args, "max_seconds"),
        opt(&args, "max_events"),
        opt(&args, "max_bytes"),
        opt(&args, "sample"),
        None, // token — read from the configured store like the Python default
        None, // send — live Cloudflare transport
        None, // websocket_factory — not available in this build
        None, // cancel_event — no live cancel channel over JSON arguments
        None, // now — monotonic clock
    ))?
    .map_err(fail)
}

/// The three project seams `LogsEnv` borrows, owned so they can live on the
/// handler's stack for the duration of one call.
struct LogSeams {
    /// `bindings.list_bindings` for zone resolution.
    bindings: Box<spiderhead_sources::cloudflare_traffic::BindingsLookup<'static>>,
    /// `workspace.open_project` for the project root.
    open_project: Box<spiderhead_sources::cloudflare_traffic::ProjectOpener<'static>>,
    /// `logstore.import_logs` for the retained artifact.
    import_logs: Box<spiderhead_sources::cloudflare_traffic::LogImport<'static>>,
}

/// Live implementations of the three seams.
fn log_seams() -> LogSeams {
    LogSeams {
        bindings: Box::new(|project: &str| {
            spiderhead_projects::bindings::list_bindings(Path::new(project), None)
                .map_err(|error| CfError::Python("ValueError", error.to_string()))
        }),
        open_project: Box::new(|project: &str| {
            spiderhead_projects::workspace::open_project(Path::new(project), None).map_err(
                |error| {
                    let (class, message) = spiderhead_projects::error::py_exception_name(&error);
                    CfError::Python(class, message)
                },
            )
        }),
        import_logs: Box::new(|path: &str, project: &str, max_lines: i64| {
            let context = ImportContext {
                loader: &Workspaces,
                free_space: &free_space,
                now: &now,
            };
            Ok(logstore_import(
                &context,
                path,
                project,
                ImportOptions {
                    max_lines,
                    ..ImportOptions::default()
                },
            ))
        }),
    }
}

/// A `LogsEnv` borrowing the live seams.
fn logs_env<'a>(host: &'a SystemHost, seams: &'a LogSeams) -> LogsEnv<'a> {
    LogsEnv {
        host,
        home: &home,
        bindings: Some(&*seams.bindings),
        open_project: Some(&*seams.open_project),
        import_logs: Some(&*seams.import_logs),
    }
}

/// `cloudflare_history`: retained aggregate Cloudflare snapshots of a project.
pub fn cloudflare_history(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(project) = present(&args, "project") else {
        return Err(fail("project required"));
    };
    let mut call = serde_json::Map::new();
    call.insert("project".to_owned(), json!(project));
    for key in [
        "zone_id",
        "snapshot_id",
        "limit",
        "offset",
        "include_facts",
        "facts_offset",
        "facts_limit",
    ] {
        if let Some(value) = opt(&args, key) {
            call.insert(key.to_owned(), value.clone());
        }
    }
    cloudflare_history::list_snapshots(&Value::Object(call)).map_err(fail)
}

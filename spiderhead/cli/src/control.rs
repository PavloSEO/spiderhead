//! The three commands whose terminal shape is their own: `doctor` (a component
//! list or `--json`), `update-check` and `update-download` (the bare result
//! document, no `seohead: <cmd>` banner). They call the shared handlers.

use serde_json::{json, Map, Value};

use spiderhead_mcp::error::ToolError;

use crate::exit::handler_failed;
use crate::registry::{CommandHandler, Registry};
use crate::{CliError, ExitCode, Output, ParsedCommand};

/// Register the three commands over the shared handlers.
pub fn register(registry: &mut Registry) {
    registry.register("doctor", Box::new(Doctor));
    registry.register("update-check", Box::new(Update("update_check")));
    registry.register("update-download", Box::new(Update("update_download")));
}

/// The set options of a command as handler kwargs, renaming dests to parameter names.
fn collect(cmd: &ParsedCommand, renames: &[(&str, &str)]) -> Value {
    let mut kwargs = Map::new();
    for (dest, name) in renames {
        match cmd.values.get(*dest) {
            Some(value) if !value.is_null() && value.as_str() != Some("==SUPPRESS==") => {
                kwargs.insert((*name).to_owned(), value.clone());
            }
            _ => {}
        }
    }
    Value::Object(kwargs)
}

fn handler_error(error: ToolError, out: &mut Output) -> ExitCode {
    let message = match error {
        ToolError::Failed(message) => message,
        ToolError::NotYetPorted(name) => format!("{name} is not yet available in this build"),
    };
    out.eprintln(&format!("error: {message}"));
    ExitCode::FAILURE
}

struct Doctor;

impl CommandHandler for Doctor {
    fn run(&self, cmd: &ParsedCommand, out: &mut Output) -> Result<ExitCode, CliError> {
        let kwargs = collect(
            cmd,
            &[
                ("projects_directory", "projects_directory"),
                ("config", "config_path"),
                ("sf_cli", "sf_cli"),
                ("mcp_http_port", "mcp_http_port"),
            ],
        );
        let result = match spiderhead_mcp::handlers::dispatch("doctor", &kwargs) {
            Ok(result) => result,
            Err(error) => return Ok(handler_error(error, out)),
        };
        if cmd.values.get("json") == Some(&json!(true)) {
            out.emit(&result);
            return Ok(ExitCode::SUCCESS);
        }
        for (name, component) in result.as_object().into_iter().flatten() {
            if name == "format" {
                continue;
            }
            let Some(fields) = component.as_object() else {
                continue;
            };
            if let Some(state) = fields.get("state") {
                out.println(&format!("{name}: {}", text(state)));
                reason_line(fields, "  ", out);
            } else {
                out.println(&format!("{name}:"));
                for (sub, subcomponent) in fields {
                    if let Some(sub_fields) = subcomponent.as_object() {
                        if let Some(state) = sub_fields.get("state") {
                            out.println(&format!("  {sub}: {}", text(state)));
                            reason_line(sub_fields, "    ", out);
                        }
                    }
                }
            }
        }
        Ok(ExitCode::SUCCESS)
    }
}

fn text(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}

fn reason_line(fields: &Map<String, Value>, indent: &str, out: &mut Output) {
    if let Some(reason) = fields.get("reason").and_then(Value::as_str) {
        if !reason.is_empty() {
            out.println(&format!("{indent}{reason}"));
        }
    }
}

/// `update-check` / `update-download`: the result document, exit 1 unless `ok`.
struct Update(&'static str);

impl CommandHandler for Update {
    fn run(&self, cmd: &ParsedCommand, out: &mut Output) -> Result<ExitCode, CliError> {
        let kwargs = collect(
            cmd,
            &[
                ("consent", "consent"),
                ("automatic", "automatic"),
                ("max_releases", "max_releases"),
                ("tag", "tag"),
                ("asset_name", "asset_name"),
                ("out", "out"),
                ("max_bytes", "max_bytes"),
            ],
        );
        match spiderhead_mcp::handlers::dispatch(self.0, &kwargs) {
            Ok(result) => {
                out.emit(&result);
                Ok(if handler_failed(&result) || result.get("ok").is_none() {
                    ExitCode::FAILURE
                } else {
                    ExitCode::SUCCESS
                })
            }
            Err(error) => Ok(handler_error(error, out)),
        }
    }
}

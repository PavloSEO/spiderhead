//! Parsing argv with the clap tree and mapping the matches back to
//! [`ParsedCommand`]: every declared dest keeps its declared default, so
//! the handler layer sees the same values map as before.
//!
//! Help and version requests, and every usage error, end as an [`Exit`] with
//! clap's rendering and exit code (0 for help, 2 for usage errors).

use clap::builder::ArgAction;
use clap::error::{ContextKind, ContextValue, Error as ClapError, ErrorKind};
use clap::parser::ValueSource;
use clap::ArgMatches;
use serde_json::{Map, Value};

use crate::spec::{parser_dest, PyVal};
use crate::ParsedCommand;

/// A finished parse that did not lead to a command: help, version, or a usage error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exit {
    /// Process exit code (0 for help/version, 2 for usage errors).
    pub code: i32,
    /// Text for stdout.
    pub stdout: String,
    /// Text for stderr.
    pub stderr: String,
}

/// Result of parsing an argv.
#[derive(Debug)]
pub enum Outcome {
    /// A command was selected.
    Run(ParsedCommand),
    /// Help, version, or an error; nothing to run.
    Exit(Exit),
}

/// Parse `argv` (without the program name) against the declared command tree.
pub fn parse(argv: &[String]) -> Outcome {
    let mut root = crate::spec::command();
    if argv.is_empty() {
        // A bare invocation is help on stdout, not an error.
        return Outcome::Exit(Exit {
            code: 0,
            stdout: root.render_help().to_string(),
            stderr: String::new(),
        });
    }
    match try_matches(&root, argv) {
        Ok(matches) => {
            let mut path = Vec::new();
            let mut values = Map::new();
            collect(&root, &matches, &mut path, &mut values);
            Outcome::Run(ParsedCommand { path, values })
        }
        Err(error) => match error.kind() {
            ErrorKind::MissingRequiredArgument => {
                // A command refused to run only because `--project`/`--directory`
                // was missing while the working directory names a project: retry
                // with the discovered project filled in.
                if let Some(extra) = project_fill(&error) {
                    let mut retry = argv.to_vec();
                    retry.extend(extra);
                    if let Ok(retry) = try_matches(&root, &retry) {
                        let mut path = Vec::new();
                        let mut values = Map::new();
                        collect(&root, &retry, &mut path, &mut values);
                        return Outcome::Run(ParsedCommand { path, values });
                    }
                }
                exit_of(&error)
            }
            _ => exit_of(&error),
        },
    }
}

fn try_matches(root: &clap::builder::Command, argv: &[String]) -> Result<ArgMatches, ClapError> {
    let mut owned = Vec::with_capacity(argv.len() + 1);
    owned.push(std::ffi::OsString::from("spiderhead"));
    owned.extend(
        argv.iter()
            .map(String::as_str)
            .map(std::ffi::OsString::from),
    );
    root.clone().try_get_matches_from(owned)
}

fn exit_of(error: &ClapError) -> Outcome {
    // Help and version go to stdout, usage errors to stderr; both exit 0 / 2.
    let stdout = if error.use_stderr() {
        String::new()
    } else {
        error.render().to_string()
    };
    let stderr = if error.use_stderr() {
        error.render().to_string()
    } else {
        String::new()
    };
    Outcome::Exit(Exit {
        code: error.exit_code(),
        stdout,
        stderr,
    })
}

/// The `--project`/`--directory` flag/value pairs to retry with, when the only
/// missing required arguments are those and the working directory names a project.
fn project_fill(error: &ClapError) -> Option<Vec<String>> {
    let missing = error.get(ContextKind::InvalidArg)?;
    let names = match missing {
        ContextValue::Strings(names) => names,
        _ => return None,
    };
    let mut flags = Vec::new();
    for name in names {
        let flag = name.split(' ').next().unwrap_or_default().to_owned();
        if flag != "--project" && flag != "--directory" {
            return None;
        }
        flags.push(flag);
    }
    let found = spiderhead_core::project_root::discover_here()?;
    let value = found.to_string_lossy().into_owned();
    Some(
        flags
            .into_iter()
            .flat_map(|flag| [flag, value.clone()])
            .collect(),
    )
}

/// Walk the matched subcommand chain; at each level, every declared dest keeps
/// its default (or null), then an explicit value overwrites it in place, so
/// key order follows the declarations exactly as the previous parser's did.
fn collect(
    cmd: &clap::builder::Command,
    m: &ArgMatches,
    path: &mut Vec<String>,
    values: &mut Map<String, Value>,
) {
    for arg in cmd.get_arguments() {
        let action = arg.get_action();
        if matches!(action, ArgAction::Help | ArgAction::Version) {
            continue;
        }
        let id = arg.get_id().as_str();
        match action {
            ArgAction::Set => {
                let value = m
                    .get_one::<PyVal>(id)
                    .map_or(Value::Null, |v| v.value.clone());
                values.insert(id.to_owned(), value);
            }
            ArgAction::SetTrue | ArgAction::SetFalse => {
                let value = if m.value_source(id) == Some(ValueSource::CommandLine) {
                    Value::Bool(m.get_flag(id))
                } else if arg.get_default_values().is_empty() {
                    // A switch whose declared default was null (`--live-recheck`).
                    Value::Null
                } else {
                    Value::Bool(false)
                };
                values.insert(id.to_owned(), value);
            }
            ArgAction::Append => {
                let value = if m.value_source(id) == Some(ValueSource::CommandLine) {
                    Value::Array(
                        m.get_many::<PyVal>(id)
                            .into_iter()
                            .flatten()
                            .map(|v| v.value.clone())
                            .collect(),
                    )
                } else if let Some(default) = arg.get_default_values().first() {
                    // `--map` ships an empty-list default; keep it a list.
                    serde_json::from_str(default.to_string_lossy().as_ref())
                        .unwrap_or(Value::Array(Vec::new()))
                } else {
                    Value::Null
                };
                values.insert(id.to_owned(), value);
            }
            _ => {}
        }
    }
    if path.as_slice() == ["sf", "run"] {
        // `--live-recheck` / `--no-live-recheck` share one dest: the later
        // flag on the command line wins, and neither leaves a key of its own.
        let given = |id: &str| m.value_source(id) == Some(ValueSource::CommandLine);
        let value = match (given("no_live_recheck"), given("live_recheck")) {
            (true, false) => Some(Value::Bool(false)),
            (false, true) => Some(Value::Bool(true)),
            (true, true) => {
                let yes_first = m.index_of("live_recheck") < m.index_of("no_live_recheck");
                Some(Value::Bool(yes_first))
            }
            (false, false) => None,
        };
        values.insert("live_recheck".to_owned(), value.unwrap_or(Value::Null));
        values.remove("no_live_recheck");
    }
    if let Some((sub, sub_matches)) = m.subcommand() {
        values.insert(parser_dest(path).to_owned(), Value::String(sub.to_owned()));
        path.push(sub.to_owned());
        if let Some(sub_cmd) = cmd.find_subcommand(sub) {
            collect(sub_cmd, sub_matches, path, values);
        }
    }
}

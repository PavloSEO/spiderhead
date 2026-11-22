//! The `spiderhead` command-line surface: the clap command tree, `--help`,
//! parse errors, exit codes, output formats and the handler registry.
//!
//! Every command, flag, choice and default is declared once in [`spec`] with
//! the clap builder API; clap renders help and usage errors (exit code 2) and
//! [`parse`] maps the matches back to the dest-keyed values map the handlers
//! consume. Command handlers are plugged in through [`Registry`]; until a
//! domain crate registers one, a command is [`NotYetAvailable`].

mod control;
mod convert;
mod discovery;
mod domain;
mod errors;
mod exit;
mod json_errors;
mod legacy;
mod output;
mod parse;
mod registry;
mod semantics;
mod sources_kwargs;
pub mod spec;

use serde_json::{Map, Value};

pub use domain::registry;
pub use errors::CliError;
pub use exit::{emit_result, handler_failed, result_exit_code};
pub use output::{render_csv, render_json, render_table, Format, Output};
pub use registry::{CommandHandler, NotYetAvailable, Registry};

/// Toolkit release the surface reports on `--version`.
pub const TOOLKIT_VERSION: &str = spiderhead_core::user_agents::PRODUCT_VERSION;

/// Process exit code: 0 success, 1 command failure, 2 usage error (and gate failures).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitCode(pub u8);

impl ExitCode {
    /// The command succeeded.
    pub const SUCCESS: Self = Self(0);
    /// The command failed.
    pub const FAILURE: Self = Self(1);
    /// Usage error, or a result that is neither clean nor failed.
    pub const USAGE: Self = Self(2);
}

impl From<ExitCode> for std::process::ExitCode {
    fn from(code: ExitCode) -> Self {
        Self::from(code.0)
    }
}

/// A successfully parsed command line.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedCommand {
    /// Command words as typed, e.g. `["scan", "list"]` or `["crawl-site"]`.
    pub path: Vec<String>,
    /// Parsed values by declared dest; unset options carry their declared default.
    pub values: Map<String, Value>,
}

/// Parse `argv` (without the program name), run the selected handler and return the exit code.
///
/// `sf` / `semantics` skip the deprecated-flag rewrite (their parsers own their
/// flags), a bare invocation prints help and exits 0, and a handler error
/// becomes `error: <message>` on stderr with exit code 1.
pub fn run(argv: &[String], registry: &Registry, out: &mut Output) -> Result<ExitCode, CliError> {
    spiderhead_projects::action_log::set_interface("cli");
    if let Some(code) = discovery::builtin(argv, out) {
        return Ok(code);
    }
    let delegated = argv
        .first()
        .is_some_and(|a| matches!(a.as_str(), "sf" | "semantics"));
    let (args, warnings) = if delegated {
        (argv.to_vec(), Vec::new())
    } else {
        legacy::rewrite_deprecated_scan_flags(argv)
    };
    match parse::parse(&args) {
        parse::Outcome::Exit(exit) => {
            out.stdout.push_str(&exit.stdout);
            out.stderr.push_str(&exit.stderr);
            Ok(ExitCode(exit.code.clamp(0, 255) as u8))
        }
        parse::Outcome::Run(mut cmd) => {
            discovery::fill_project(&mut cmd);
            for w in warnings {
                out.eprintln(&format!("warning: {w}"));
            }
            if cmd.path.is_empty() {
                out.stdout
                    .push_str(&spec::command().render_help().to_string());
                return Ok(ExitCode::SUCCESS);
            }
            match registry.handler_for(&cmd).run(&cmd, out) {
                Ok(code) => Ok(code),
                Err(CliError::Io(e)) => Err(CliError::Io(e)),
                Err(e) => {
                    out.eprintln(&format!("error: {e}"));
                    Ok(ExitCode::FAILURE)
                }
            }
        }
    }
}

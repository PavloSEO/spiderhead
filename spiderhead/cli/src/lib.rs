//! The `spiderhead` command-line surface: command tree, `--help`, parse errors, exit codes,
//! output formats and the handler registry.
//!
//! The command tree is data (`data/cli_tree.json`, captured from the reference argparse tree); help
//! screens and parse errors are produced by ports of argparse's formatter and parser so the text
//! is identical to the reference CLI. Command handlers are plugged in through [`Registry`]; until a
//! domain crate registers one, a command is [`NotYetPorted`].

mod argparse;
mod control;
mod convert;
mod discovery;
mod domain;
mod errors;
mod exit;
mod help;
mod legacy;
mod output;
mod pycompat;
mod pyjson;
mod registry;
mod semantics;
mod sources_kwargs;
mod textwrap;
mod tree;

use serde_json::{Map, Value};

pub use argparse::{parse_args, Exit, Outcome};
pub use domain::registry;
pub use errors::CliError;
pub use exit::{emit_result, handler_failed, result_exit_code};
pub use output::{render_csv, render_json, render_table, Format, Output};
pub use registry::{CommandHandler, NotYetPorted, Registry};
pub use tree::{tree, Node, Tree};

/// Toolkit release the surface mirrors; substituted for `{VERSION}` in `--version` output.
pub const TOOLKIT_VERSION: &str = "3.2.0";

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
    /// Parsed values by argparse `dest`; unset options carry their declared default.
    pub values: Map<String, Value>,
}

/// Parse `argv` (without the program name), run the selected handler and return the exit code.
///
/// Mirrors `spiderhead.cli.main`: `sf` / `semantics` own their parsers, deprecated saved-scan
/// flags are rewritten first, a bare invocation prints help and exits 0, and a handler error
/// becomes `error: <message>` on stderr with exit code 1.
pub fn run(argv: &[String], registry: &Registry, out: &mut Output) -> Result<ExitCode, CliError> {
    spiderhead_projects::action_log::set_interface("cli");
    if let Some(code) = discovery::builtin(argv, out) {
        return Ok(code);
    }
    let root = &tree()?.root;
    let parse = |argv: &[String]| {
        let delegated = argv
            .first()
            .and_then(|a| matches!(a.as_str(), "sf" | "semantics").then(|| root.child(a)))
            .flatten();
        match delegated {
            Some(node) => (parse_args(node, &argv[1..]), Vec::new()),
            None => {
                let (args, warnings) = legacy::rewrite_deprecated_scan_flags(argv);
                (parse_args(root, &args), warnings)
            }
        }
    };
    let (mut outcome, warnings) = parse(argv);
    if let Outcome::Exit(exit) = &outcome {
        if let Some(extra) = discovery::missing_project_flags(exit.code, &exit.stderr) {
            let mut retry: Vec<String> = argv.to_vec();
            retry.extend(extra);
            let (second, _) = parse(&retry);
            if matches!(second, Outcome::Run(_)) {
                outcome = second;
            }
        }
    }
    let cmd = match outcome {
        Outcome::Exit(exit) => {
            out.stdout.push_str(&exit.stdout);
            out.stderr.push_str(&exit.stderr);
            return Ok(ExitCode(exit.code as u8));
        }
        Outcome::Run(mut cmd) => {
            discovery::fill_project(&mut cmd);
            cmd
        }
    };
    for w in warnings {
        out.eprintln(&format!("warning: {w}"));
    }
    if cmd.path.is_empty() {
        out.stdout.push_str(&help::help(root));
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

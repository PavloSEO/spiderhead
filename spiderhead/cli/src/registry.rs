//! Command handlers: the seam between this surface and the domain crates.

use std::collections::BTreeMap;

use crate::{CliError, ExitCode, Output, ParsedCommand};

/// Implemented later by domain crates; this crate ships only [`NotYetPorted`].
pub trait CommandHandler: Send + Sync {
    /// Run the command and return its exit code; output goes through `out`.
    fn run(&self, cmd: &ParsedCommand, out: &mut Output) -> Result<ExitCode, CliError>;
}

/// Placeholder for every command whose domain logic is not ported yet.
pub struct NotYetPorted;

impl CommandHandler for NotYetPorted {
    fn run(&self, cmd: &ParsedCommand, out: &mut Output) -> Result<ExitCode, CliError> {
        out.eprintln(&format!(
            "spiderhead {}: not yet available in this build",
            cmd.path.join(" ")
        ));
        Ok(ExitCode::FAILURE)
    }
}

/// Handlers keyed by command path (`"scan list"`, `"crawl-site"`); unregistered commands fall
/// back to [`NotYetPorted`].
#[derive(Default)]
pub struct Registry {
    handlers: BTreeMap<String, Box<dyn CommandHandler>>,
}

impl Registry {
    /// An empty registry: every command is [`NotYetPorted`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `handler` for the command at `path` (words joined by spaces); replaces any previous one.
    pub fn register(&mut self, path: &str, handler: Box<dyn CommandHandler>) {
        self.handlers.insert(path.to_owned(), handler);
    }

    /// The handler for a parsed command.
    pub fn handler_for(&self, cmd: &ParsedCommand) -> &dyn CommandHandler {
        self.handlers
            .get(&cmd.path.join(" "))
            .map_or(&NotYetPorted, |h| h.as_ref())
    }
}

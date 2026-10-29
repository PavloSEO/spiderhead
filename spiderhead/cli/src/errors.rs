//! Error type of the CLI surface.

use thiserror::Error;

/// Failures a command handler or the surface itself can report.
///
/// reference prints `error: <message>` to stderr and exits 1 for any handler exception; the
/// dispatcher does the same with this type's `Display`.
#[derive(Debug, Error)]
pub enum CliError {
    /// The embedded command tree is not valid JSON (a build-time bug).
    #[error("embedded command tree is invalid: {0}")]
    Tree(String),
    /// A handler rejected its input.
    #[error("{0}")]
    Input(String),
    /// A handler failed while doing its work.
    #[error("{0}")]
    Failed(String),
    /// Writing output failed.
    #[error("output error: {0}")]
    Io(#[from] std::io::Error),
}

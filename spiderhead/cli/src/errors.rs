//! Error type of the CLI surface.

use thiserror::Error;

/// Failures a command handler or the surface itself can report.
///
/// the reference prints `error: <message>` to stderr and exits 1 for any handler exception; the
/// dispatcher does the same with this type's `Display`.
#[derive(Debug, Error)]
pub enum CliError {
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

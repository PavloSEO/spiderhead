//! Errors a tool handler can report; each becomes an `isError` result on the wire.

/// Why a tool call did not produce its value.
///
/// The `Display` text is what the client sees in the error result's text block,
/// so the wording is part of the wire contract (see `DESIGN.md`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToolError {
    /// The tool exists in the catalog but its domain crate is not linked into
    /// this build. All handlers answer this until their slice lands.
    #[error("{0} is not yet available in this build")]
    NotYetPorted(String),

    /// A handler-reported failure; the message reaches the client verbatim,
    /// mirroring the reference `_checked`/`ToolError` passthrough.
    #[error("{0}")]
    Failed(String),
}

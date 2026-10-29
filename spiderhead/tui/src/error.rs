//! Error type for the terminal shell crate.

/// Errors raised by the TUI layer (argument validation and terminal I/O).
#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    /// Invalid `language` argument.
    #[error("{0}")]
    Language(String),
    /// Invalid status kind for a badge.
    #[error("unknown status kind: {0:?} (expected one of ('ok', 'warn', 'info', 'off'))")]
    BadgeKind(String),
    /// Scan selector found nothing.
    #[error("selected scan is absent from the retained observation")]
    MissingScan,
    /// Terminal I/O failure while reading keys or toggling raw mode.
    #[error("terminal I/O: {0}")]
    Io(String),
}

/// Result alias for TUI calls.
pub type TuiResult<T> = Result<T, TuiError>;

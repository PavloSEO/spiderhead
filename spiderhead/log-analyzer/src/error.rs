//! Error type shared by every log-analysis entry point.

use thiserror::Error;

/// Failure of a log-analysis operation.
///
/// Public entry points that report results as data convert these into
/// `{"ok": false, "error": ...}` documents; validation failures of the caller's
/// arguments stay errors.
#[derive(Debug, Error)]
pub enum LogsError {
    /// An argument or stored value is outside its documented bounds or shape.
    #[error("{0}")]
    Value(String),
    /// The operating system refused a file operation.
    #[error("{0}")]
    Io(String),
    /// The embedded SQLite database failed.
    #[error("{0}")]
    Sqlite(String),
    /// A stored value has the wrong type for the operation.
    #[error("{0}")]
    Type(String),
}

impl LogsError {
    /// Shorthand for a bounds or shape error.
    pub fn value(message: impl Into<String>) -> Self {
        Self::Value(message.into())
    }
}

impl From<rusqlite::Error> for LogsError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error.to_string())
    }
}

impl From<std::io::Error> for LogsError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(os_message(&error, None))
    }
}

/// Render an I/O error the way the operator-facing reports always have:
/// `[Errno 2] No such file or directory: 'path'`.
pub fn os_message(error: &std::io::Error, path: Option<&str>) -> String {
    match error.raw_os_error() {
        Some(code) => {
            let text = error.to_string();
            let text = text
                .rsplit_once(" (os error")
                .map_or(text.as_str(), |(head, _)| head);
            match path {
                Some(path) => format!("[Errno {code}] {text}: '{path}'"),
                None => format!("[Errno {code}] {text}"),
            }
        }
        None => error.to_string(),
    }
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, LogsError>;

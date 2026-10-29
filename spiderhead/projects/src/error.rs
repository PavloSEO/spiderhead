//! Error type shared by the project modules.
//!
//! Contract violations are reported as value errors; OS and SQLite failures keep
//! their own variants, so a caller can tell a contract failure from an IO failure.

use thiserror::Error;

/// Failure of a project operation.
#[derive(Debug, Error)]
pub enum ProjectsError {
    /// Contract violation; the message is the exact user-facing text.
    #[error("{0}")]
    Value(String),
    /// Filesystem failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// SQLite failure from the storage layer.
    #[error(transparent)]
    Store(#[from] spiderhead_store::error::StoreError),
    /// JSON (de)serialisation failure.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// An error with a recorded class name (kept in project data); callers treat
    /// it as a value error.
    #[error("{message}")]
    Py {
        /// Recorded class name, such as `ProjectExportError` or `KeyError`.
        name: &'static str,
        /// The `str(exc)` payload.
        message: String,
    },
}

/// Result alias used across the crate.
pub type ProjectsResult<T> = Result<T, ProjectsError>;

/// Build the `ValueError` variant from a message.
pub fn value(message: impl Into<String>) -> ProjectsError {
    ProjectsError::Value(message.into())
}

/// Build an error carrying a recorded class name.
pub fn py_named(name: &'static str, message: impl Into<String>) -> ProjectsError {
    ProjectsError::Py {
        name,
        message: message.into(),
    }
}

/// `(class name, message)` pair recorded for one error.
pub fn py_exception_name(error: &ProjectsError) -> (&'static str, String) {
    match error {
        ProjectsError::Py { name, message } => (name, message.clone()),
        ProjectsError::Value(message) => ("ValueError", message.clone()),
        ProjectsError::Json(err) => ("JSONDecodeError", err.to_string()),
        ProjectsError::Io(err) => os_exception(err),
        ProjectsError::Store(err) => store_exception(err),
    }
}

/// Map an `io::Error` onto the recorded `OSError` class name and message text.
fn os_exception(err: &std::io::Error) -> (&'static str, String) {
    let name = os_error_name(err);
    let text = match err.raw_os_error() {
        Some(n) => format!("[Errno {n}] {}", errno_text(n)),
        None => err.to_string(),
    };
    (name, text)
}

/// reference exception class name for an `io::Error`.
#[cfg(unix)]
fn os_error_name(err: &std::io::Error) -> &'static str {
    match err.raw_os_error() {
        Some(libc::ENOENT) => "FileNotFoundError",
        Some(libc::EEXIST) => "FileExistsError",
        Some(libc::EACCES) | Some(libc::EPERM) => "PermissionError",
        Some(libc::ENOTDIR) => "NotADirectoryError",
        Some(libc::EISDIR) => "IsADirectoryError",
        Some(libc::ENOSPC) => "OSError",
        _ => "OSError",
    }
}

/// reference exception class name where error numbers differ: match on the
/// platform-neutral kind instead.
#[cfg(not(unix))]
fn os_error_name(err: &std::io::Error) -> &'static str {
    use std::io::ErrorKind;
    match err.kind() {
        ErrorKind::NotFound => "FileNotFoundError",
        ErrorKind::AlreadyExists => "FileExistsError",
        ErrorKind::PermissionDenied => "PermissionError",
        ErrorKind::NotADirectory => "NotADirectoryError",
        ErrorKind::IsADirectory => "IsADirectoryError",
        _ => "OSError",
    }
}

/// `os.strerror` text for one errno value.
#[cfg(unix)]
fn errno_text(errno: i32) -> String {
    // SAFETY: `strerror` returns a valid static C string for any errno on POSIX.
    unsafe { std::ffi::CStr::from_ptr(libc::strerror(errno)) }
        .to_string_lossy()
        .into_owned()
}

/// `os.strerror` text where `strerror` is not linked: the OS message for the code.
#[cfg(not(unix))]
fn errno_text(errno: i32) -> String {
    std::io::Error::from_raw_os_error(errno).to_string()
}

/// Best-effort recorded class name for a storage-layer failure.
fn store_exception(err: &spiderhead_store::error::StoreError) -> (&'static str, String) {
    use spiderhead_store::error::StoreError;
    match err {
        StoreError::Sqlite(_) => ("OperationalError", strip_prefix(&err.to_string())),
        StoreError::Io(inner) => os_exception(inner),
        StoreError::Json(inner) => ("JSONDecodeError", inner.to_string()),
        _ => ("Exception", strip_prefix(&err.to_string())),
    }
}

/// Drop the `sqlite: `/`io: `/`json: ` display prefix `StoreError` adds.
fn strip_prefix(text: &str) -> String {
    text.split_once(": ")
        .map(|(_, rest)| rest)
        .unwrap_or(text)
        .to_owned()
}

/// `io::Result` -> `ProjectsResult` for one statement.
pub fn io<T>(result: std::io::Result<T>) -> ProjectsResult<T> {
    result.map_err(ProjectsError::Io)
}

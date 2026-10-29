//! Error type for the Yandex Webmaster adapter.

use super::*;

/// Failure inside the Webmaster adapter: a transport error or a data-handling error.
#[derive(Debug)]
pub enum WmError {
    /// Missing credential. The message names the source, never the secret.
    MissingCredential(String),
    /// HTTP error response: status, reason phrase and readable body.
    Http {
        /// HTTP status code.
        code: i64,
        /// Reason phrase from the response.
        reason: String,
        /// Response body, decoded with replacement characters.
        body: String,
    },
    /// Connection failure; the message is `<urlopen error {reason}>`.
    Url(String),
    /// Timeout; the message is used as-is.
    Timeout(String),
    /// Value error, including JSON and UTF-8 decode errors.
    Value(String),
    /// Missing key; the message is the quoted key.
    Key(String),
    /// Any other failure, identified by its class name and message.
    Python {
        /// Error class name.
        class: String,
        /// Error message.
        message: String,
    },
}

impl WmError {
    /// Error class name.
    pub fn python_class(&self) -> &str {
        match self {
            WmError::MissingCredential(_) => "MissingCredential",
            WmError::Http { .. } => "HTTPError",
            WmError::Url(_) => "URLError",
            WmError::Timeout(_) => "TimeoutError",
            WmError::Value(_) => "ValueError",
            WmError::Key(_) => "KeyError",
            WmError::Python { class, .. } => class.as_str(),
        }
    }

    /// Errors that `collect` turns into an error response instead of propagating.
    pub(super) fn collect_catches(&self) -> bool {
        matches!(
            self,
            WmError::Http { .. }
                | WmError::Url(_)
                | WmError::Timeout(_)
                | WmError::Value(_)
                | WmError::Key(_)
        )
    }

    /// Errors that `url_queries` turns into a retry or an error response.
    pub(super) fn queries_catches(&self) -> bool {
        matches!(
            self,
            WmError::Url(_) | WmError::Timeout(_) | WmError::Value(_) | WmError::Key(_)
        )
    }

    pub(super) fn type_error(message: impl Into<String>) -> Self {
        WmError::Python {
            class: "TypeError".to_owned(),
            message: message.into(),
        }
    }

    pub(super) fn value_error(message: impl Into<String>) -> Self {
        WmError::Value(message.into())
    }

    /// Indexing a non-mapping: lists and strings get index errors, other types are not subscriptable.
    pub(super) fn subscript(value: &Value) -> Self {
        match value {
            Value::Array(_) => {
                WmError::type_error("list indices must be integers or slices, not str")
            }
            Value::String(_) => WmError::type_error("string indices must be integers, not 'str'"),
            other => WmError::type_error(format!(
                "'{}' object is not subscriptable",
                pystr::type_name(other)
            )),
        }
    }

    /// Calling `get` on a non-mapping is an attribute error.
    pub(super) fn attribute_get(value: &Value) -> Self {
        WmError::Python {
            class: "AttributeError".to_owned(),
            message: format!(
                "'{}' object has no attribute 'get'",
                pystr::type_name(value)
            ),
        }
    }
}

impl std::fmt::Display for WmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WmError::Http { code, reason, .. } => write!(f, "HTTP Error {code}: {reason}"),
            WmError::Url(reason) => write!(f, "<urlopen error {reason}>"),
            WmError::MissingCredential(message)
            | WmError::Timeout(message)
            | WmError::Value(message)
            | WmError::Key(message)
            | WmError::Python { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for WmError {}

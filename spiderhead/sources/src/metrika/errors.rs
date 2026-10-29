//! Error types for the Metrika client.

use super::*;

/// `MetrikaError` — an API error carrying the HTTP status and the service's message.
#[derive(Debug, Clone)]
pub struct MetrikaError {
    /// HTTP status; `0` for transport-level failures.
    pub status: i64,
    /// Message returned by the service.
    pub message: String,
}

impl std::fmt::Display for MetrikaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Metrica {}: {}", self.status, self.message)
    }
}

impl std::error::Error for MetrikaError {}

/// Errors the client raises instead of returning a body: an API error, a missing credential,
/// or an uncaught failure such as a JSON decode error or a spend-log I/O error.
#[derive(Debug)]
pub enum MetrikaFail {
    /// API error with status and message.
    Metrika(MetrikaError),
    /// `MissingCredential` — no usable token source.
    MissingCredential(String),
    /// Any other failure that was not caught on the way out.
    Python {
        /// Error class name.
        class: String,
        /// Error message text.
        message: String,
    },
}

impl MetrikaFail {
    /// Error class name.
    pub fn python_class(&self) -> &str {
        match self {
            MetrikaFail::Metrika(_) => "MetrikaError",
            MetrikaFail::MissingCredential(_) => "MissingCredential",
            MetrikaFail::Python { class, .. } => class.as_str(),
        }
    }

    pub(super) fn type_error(message: impl Into<String>) -> Self {
        MetrikaFail::python("TypeError", message)
    }

    pub(super) fn attribute_get(value: &Value) -> Self {
        MetrikaFail::python(
            "AttributeError",
            format!(
                "'{}' object has no attribute 'get'",
                pystr::type_name(value)
            ),
        )
    }

    /// An uncaught failure, such as an invalid value or a type error.
    pub(crate) fn python(class: &str, message: impl Into<String>) -> Self {
        MetrikaFail::Python {
            class: class.to_owned(),
            message: message.into(),
        }
    }
}

impl From<MetrikaError> for MetrikaFail {
    fn from(err: MetrikaError) -> Self {
        MetrikaFail::Metrika(err)
    }
}

impl std::fmt::Display for MetrikaFail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MetrikaFail::Metrika(err) => err.fmt(f),
            MetrikaFail::MissingCredential(message) | MetrikaFail::Python { message, .. } => {
                f.write_str(message)
            }
        }
    }
}

impl std::error::Error for MetrikaFail {}

/// A resolved report timezone: either an IANA zone name or a fixed UTC offset.
#[derive(Debug, Clone)]
pub(crate) enum Tz {
    /// Named IANA zone; the offset is resolved at the current time.
    Named(String, i64),
    /// Fixed offset in seconds.
    Fixed(i64),
}

impl Tz {
    /// The UTC offset in seconds.
    pub(crate) fn offset(&self) -> i64 {
        match self {
            Tz::Named(_, offset) | Tz::Fixed(offset) => *offset,
        }
    }

    /// Display text: the zone name, or `UTC` / `UTC±HH:MM` for a fixed offset.
    pub(crate) fn display(&self) -> String {
        match self {
            Tz::Named(name, _) => name.clone(),
            Tz::Fixed(0) => "UTC".to_owned(),
            Tz::Fixed(seconds) => {
                let sign = if *seconds < 0 { '-' } else { '+' };
                let abs = seconds.unsigned_abs();
                format!("UTC{sign}{:02}:{:02}", abs / 3600, abs % 3600 / 60)
            }
        }
    }
}

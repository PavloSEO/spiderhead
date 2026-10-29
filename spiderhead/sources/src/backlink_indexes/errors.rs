//! Error type for backlink index collection, plus the transport reply shapes it consumes.
use super::*;

/// Errors this module raises.
#[derive(Debug)]
pub enum BlError {
    /// A typed failure, such as an invalid value, a wrong type or an I/O error.
    Python {
        /// Name of the error kind.
        class: String,
        /// Error message.
        message: String,
    },
}

impl BlError {
    /// Name of the error kind.
    pub fn python_class(&self) -> &str {
        match self {
            BlError::Python { class, .. } => class.as_str(),
        }
    }
}

impl std::fmt::Display for BlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BlError::Python { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for BlError {}

/// A failure with an explicit error kind name.
pub(super) fn pyerr(class: &str, message: impl Into<String>) -> BlError {
    BlError::Python {
        class: class.to_owned(),
        message: message.into(),
    }
}

/// Invalid input value.
pub(super) fn value_err<T>(message: impl Into<String>) -> Result<T, BlError> {
    Err(pyerr("ValueError", message))
}

/// Value has the wrong type.
pub(super) fn type_err<T>(message: impl Into<String>) -> Result<T, BlError> {
    Err(pyerr("TypeError", message))
}

/// Transport result: the body text and response headers.
pub struct TransportReply {
    /// Body text, decoded as strict UTF-8.
    pub body: String,
    /// Response header pairs in wire order.
    pub headers: Vec<(String, String)>,
}

/// A transport failure. `Caught` covers network, timeout, I/O and value errors, which
/// `collect` records; `Raised` propagates unchanged.
pub enum TransportFail {
    /// A recoverable failure; `collect` records an uncertain charge and fails.
    Caught,
    /// An error that propagates unchanged, as (class, message).
    Raised(String, String),
}

/// Shape of the request function passed as `transport`.
pub type TransportFn<'a> = dyn Fn(&Request) -> Result<TransportReply, TransportFail> + 'a;

/// Credential reader: (config path, env var) to the stored secret.
pub type CredProbe<'a> = dyn Fn(&str, &str) -> Result<String, CredentialError> + 'a;

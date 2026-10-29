//! Arsenkin error type and the helpers that build each error kind.
//! Provider failures carry the upstream code; other variants are local validation or credential errors.
use super::*;

/// Provider or client error; displays as `{code}: {message}`.
#[derive(Debug)]
pub enum ArsError {
    /// A provider-side failure with a code (`"429"`, `"NETWORK"`, `"INVALID_TASK_ID"`,
    /// `"STATUS"`, `"TIMEOUT"`, `"RETRY"` or an HTTP status string). Boxed to keep the
    /// error type small.
    Provider(Box<ProviderError>),
    /// No token source is configured.
    MissingCredential(String),
    /// Any other failure, such as an I/O or decoding error.
    Python {
        /// Name of the original error kind.
        class: String,
        /// Message of the original error.
        message: String,
    },
}

/// Provider-side failure with its upstream code, message and optional response body.
#[derive(Debug)]
pub struct ProviderError {
    /// Error code.
    pub code: String,
    /// The message payload.
    pub message: Value,
    /// Parsed response body for status errors and `set_task` validation failures.
    pub payload: Option<Value>,
}

impl ArsError {
    /// Name of the original error kind.
    pub fn python_class(&self) -> &str {
        match self {
            ArsError::Provider(_) => "ArsenkinError",
            ArsError::MissingCredential(_) => "MissingCredential",
            ArsError::Python { class, .. } => class.as_str(),
        }
    }

    /// Error code; present only for provider failures.
    pub fn code(&self) -> Option<&str> {
        match self {
            ArsError::Provider(err) => Some(err.code.as_str()),
            _ => None,
        }
    }
}

impl std::fmt::Display for ArsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArsError::Provider(err) => {
                // Displays as `{code}: {message}`; the payload is rendered as text.
                write!(f, "{}: {}", err.code, pystr::repr_value(&err.message))
            }
            ArsError::MissingCredential(m) | ArsError::Python { message: m, .. } => f.write_str(m),
        }
    }
}

impl std::error::Error for ArsError {}

/// Invalid input value.
pub(super) fn value_err(message: impl Into<String>) -> ArsError {
    ArsError::Python {
        class: "ValueError".to_owned(),
        message: message.into(),
    }
}

/// Value has the wrong type.
pub(super) fn type_err(message: impl Into<String>) -> ArsError {
    ArsError::Python {
        class: "TypeError".to_owned(),
        message: message.into(),
    }
}

/// Accessed an attribute that a value of this type does not have.
pub(super) fn attr_err(value: &Value, attr: &str) -> ArsError {
    ArsError::Python {
        class: "AttributeError".to_owned(),
        message: format!(
            "'{}' object has no attribute '{}'",
            pystr::type_name(value),
            attr
        ),
    }
}

/// A failure with an explicit error kind name.
pub(super) fn pyerr(class: &str, message: impl Into<String>) -> ArsError {
    ArsError::Python {
        class: class.to_owned(),
        message: message.into(),
    }
}

/// Build a provider error with an optional payload.
pub(super) fn provider(code: impl Into<String>, message: Value) -> ArsError {
    ArsError::Provider(Box::new(ProviderError {
        code: code.into(),
        message,
        payload: None,
    }))
}

/// Credential failure as surfaced by this module.
pub(super) fn cred_err(err: CredentialError) -> ArsError {
    match err {
        CredentialError::Missing(message) => ArsError::MissingCredential(message),
        other => pyerr(other.python_class(), other.to_string()),
    }
}

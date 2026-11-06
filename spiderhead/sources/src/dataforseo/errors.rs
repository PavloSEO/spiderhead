//! Error type returned by the DataForSEO adapter, one variant per failure kind.
use super::*;

/// Every failure the adapter returns, tagged with its error kind.
#[derive(Debug, thiserror::Error)]
pub enum DfsError {
    /// Provider error with a status; the text carries the `DataForSEO {status}: ` prefix.
    /// Wrapper `error` fields use [`DfsError::message`], which is the bare message.
    #[error("DataForSEO {status}: {message}")]
    Provider {
        /// HTTP status, or `0` for envelope and transport failures.
        status: i64,
        /// Bare error message.
        message: String,
    },
    /// No login or password source is configured.
    #[error("{0}")]
    MissingCredential(String),
    /// Any other failure, such as invalid input, a wrong type or an I/O error.
    #[error("{message}")]
    Classed {
        /// Name of the error kind.
        class: String,
        /// Error message.
        message: String,
    },
}

impl DfsError {
    /// Name of the error kind.
    pub fn error_class(&self) -> &str {
        match self {
            DfsError::Provider { .. } => "DataForSEOError",
            DfsError::MissingCredential(_) => "MissingCredential",
            DfsError::Classed { class, .. } => class.as_str(),
        }
    }

    /// Message used in wrapped `error` fields, without the status prefix.
    pub fn message(&self) -> &str {
        match self {
            DfsError::Provider { message, .. } => message,
            DfsError::MissingCredential(message) | DfsError::Classed { message, .. } => message,
        }
    }
}

/// Invalid input value.
pub(crate) fn value_err(message: &str) -> DfsError {
    DfsError::Classed {
        class: "ValueError".to_owned(),
        message: message.to_owned(),
    }
}

/// Value has the wrong type.
pub(crate) fn type_err(message: &str) -> DfsError {
    DfsError::Classed {
        class: "TypeError".to_owned(),
        message: message.to_owned(),
    }
}

/// A value was missing an expected attribute.
pub(crate) fn attr_err(value: &Value, attr: &str) -> DfsError {
    DfsError::Classed {
        class: "AttributeError".to_owned(),
        message: format!(
            "'{}' object has no attribute '{}'",
            pystr::type_name(value),
            attr
        ),
    }
}

/// A failure with an explicit error kind name.
pub(crate) fn pyerr(class: &str, message: impl Into<String>) -> DfsError {
    DfsError::Classed {
        class: class.to_owned(),
        message: message.into(),
    }
}

/// Credential failure as surfaced by this module.
pub(super) fn cred_err(err: CredentialError) -> DfsError {
    match err {
        CredentialError::Missing(message) => DfsError::MissingCredential(message),
        other => pyerr(other.error_class(), other.to_string()),
    }
}

//! Error type for the Yandex Cloud client.

/// Error returned by the Yandex Cloud client.
/// Each variant is a failure class that the retry logic handles differently.
#[derive(Debug)]
pub enum CloudError {
    /// No API key or folder ID is configured.
    MissingCredential(String),
    /// A billed request got no usable response, so it may have been charged.
    NetworkAmbiguous(String),
    /// Transport failure; retried when the request was not billed.
    Url(String),
    /// TLS failure; retried the same way as `Url`.
    Ssl(String),
    /// Timed out; not retried, so it reaches the caller.
    Timeout(String),
    /// Response body is not valid UTF-8; billed requests are still recorded.
    Decode(String),
    /// Any other failure, identified by its class name and message.
    Python {
        /// Error class name.
        class: String,
        /// Error message.
        message: String,
    },
}

impl CloudError {
    /// Error class name.
    pub fn python_class(&self) -> &str {
        match self {
            CloudError::MissingCredential(_) => "MissingCredential",
            CloudError::NetworkAmbiguous(_) => "NetworkAmbiguousError",
            CloudError::Url(_) => "URLError",
            CloudError::Ssl(_) => "SSLError",
            CloudError::Timeout(_) => "TimeoutError",
            CloudError::Decode(_) => "UnicodeDecodeError",
            CloudError::Python { class, .. } => class.as_str(),
        }
    }

    /// Only transport and TLS failures are retried automatically.
    pub(super) fn network_retryable(&self) -> bool {
        matches!(self, CloudError::Url(_) | CloudError::Ssl(_))
    }

    /// Build the generic variant from a class name and message.
    pub(crate) fn python(class: &str, message: impl Into<String>) -> Self {
        CloudError::Python {
            class: class.to_owned(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CloudError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CloudError::Url(reason) => write!(f, "<urlopen error {reason}>"),
            CloudError::MissingCredential(message)
            | CloudError::NetworkAmbiguous(message)
            | CloudError::Ssl(message)
            | CloudError::Timeout(message)
            | CloudError::Decode(message)
            | CloudError::Python { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for CloudError {}

//! Error types for GA4 calls: transport faults, the public Ga4Error and the injection seams.
use super::*;

/// Transport failures for `(url, payload, bearer) -> body`, grouped by kind.
#[derive(Debug)]
pub enum Ga4Fault {
    /// HTTP error response, carrying its status code.
    Http { code: i64, reason: String },
    /// Network failure before a response.
    Url(String),
    /// Request timed out.
    Timeout(String),
    /// Malformed body or a collection boundary error.
    Value(String),
    /// Any other error; it propagates rather than becoming a `failed` envelope.
    Other {
        /// Name of the error kind.
        class: String,
        /// Error message.
        message: String,
    },
}

impl Ga4Fault {
    /// HTTP status used for retry and failure-kind decisions in `ga4_content`.
    pub fn status(&self) -> Option<i64> {
        match self {
            Ga4Fault::Http { code, .. } => Some(*code),
            _ => None,
        }
    }

    /// Name of the error kind.
    pub fn python_class(&self) -> &str {
        match self {
            Ga4Fault::Http { .. } => "HTTPError",
            Ga4Fault::Url(_) => "URLError",
            Ga4Fault::Timeout(_) => "TimeoutError",
            Ga4Fault::Value(_) => "ValueError",
            Ga4Fault::Other { class, .. } => class.as_str(),
        }
    }
}

impl std::fmt::Display for Ga4Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Ga4Fault::Http { code, reason } => write!(f, "HTTP Error {code}: {reason}"),
            Ga4Fault::Url(reason) => write!(f, "<urlopen error {reason}>"),
            Ga4Fault::Timeout(message) | Ga4Fault::Value(message) => f.write_str(message),
            Ga4Fault::Other { message, .. } => f.write_str(message),
        }
    }
}

impl From<&TransportError> for Ga4Fault {
    fn from(err: &TransportError) -> Self {
        match err {
            TransportError::Http(response) => Ga4Fault::Http {
                code: i64::from(response.status),
                reason: pyerr::reason_phrase(response.status).to_owned(),
            },
            TransportError::Connect(message) => Ga4Fault::Url(message.clone()),
            TransportError::InvalidRequest(message) => Ga4Fault::Value(message.clone()),
        }
    }
}

/// Errors that `run_report`, `landing_pages` and `page_views` raise instead of returning an
/// envelope: invalid values and propagated credential failures.
#[derive(Debug, thiserror::Error)]
pub enum Ga4Error {
    /// Invalid argument, or a conversion failure in the middle of a page loop.
    #[error("{0}")]
    Invalid(String),
    /// A comparison hit a non-numeric value.
    #[error("{0}")]
    TypeError(String),
    /// An error that escaped the transport unchanged; its kind and message are kept.
    #[error("{message}")]
    Python {
        /// Name of the error kind.
        class: String,
        /// Error message.
        message: String,
    },
    /// Credential errors that are not `MissingCredential` propagate unchanged.
    #[error("{0}")]
    Credential(#[from] CredentialError),
}

impl Ga4Error {
    /// Name of the error kind.
    pub fn python_class(&self) -> &str {
        match self {
            Ga4Error::Invalid(_) => "ValueError",
            Ga4Error::TypeError(_) => "TypeError",
            Ga4Error::Python { class, .. } => class.as_str(),
            Ga4Error::Credential(err) => err.python_class(),
        }
    }
}

/// Service-account token request; the requested scope is recorded.
pub type SaToken<'a> = dyn Fn(&str) -> Result<String, CredentialError> + 'a;

/// Time-zone lookup: `Ok` resolves, `Err(None)` means the zone is not found, and
/// `Err(Some(msg))` is the invalid-key error.
pub type ZoneinfoProbe<'a> = dyn Fn(&str) -> Result<(), Option<String>> + 'a;

/// The environment that credential, transport and clock seams resolve against.
pub struct Ga4Env<'a, T: Transport> {
    /// Environment seam (`GA4_ACCESS_TOKEN`, …).
    pub host: &'a dyn Host,
    /// Credential store root.
    pub root: &'a Path,
    /// HTTP transport for `_default_transport` and the service-account exchange.
    pub http: &'a T,
    /// Access-token source; None reads the real credential store.
    pub access_token: Option<&'a dyn Fn() -> Result<String, CredentialError>>,
    /// Service-account availability check.
    pub sa_available: Option<&'a dyn Fn() -> bool>,
    /// Service-account token source.
    pub sa_token: Option<&'a SaToken<'a>>,
    /// Per-request row cap; tests can override it.
    pub page_rows: i64,
    /// Retry pause; tests use a recording sleeper.
    pub sleeper: Option<&'a crate::http::Sleeper>,
    /// Time-zone resolution; None uses the system zone database.
    /// `Ok` resolves, `Err(None)` means the zone is not found, and `Err(Some(msg))` is
    pub zoneinfo: Option<&'a ZoneinfoProbe<'a>>,
    /// Page request cap; tests can override it.
    pub max_requests: i64,
}

impl<'a, T: Transport> Ga4Env<'a, T> {
    /// Production-shaped environment: real credentials, real transport, stock bounds.
    pub fn live(host: &'a dyn Host, root: &'a Path, http: &'a T) -> Self {
        Ga4Env {
            host,
            root,
            http,
            access_token: None,
            sa_available: None,
            sa_token: None,
            page_rows: MAX_ROWS,
            sleeper: None,
            zoneinfo: None,
            max_requests: crate::ga4_content::MAX_PAGE_REQUESTS,
        }
    }
}

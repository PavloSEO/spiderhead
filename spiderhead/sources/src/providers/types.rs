//! Shared types for provider dispatch: errors, registry rows and the injected seams.

use super::*;

/// Error returned by a provider dispatch seam; carries the fields the lifecycle code inspects
/// (`code`, `status`, `message`).
#[derive(Debug, Clone)]
pub struct DispatchError {
    /// Provider error class name (e.g. `ArsenkinError`, `MetrikaError`).
    pub class: String,
    /// Human-readable message.
    pub message: String,
    /// Optional error code.
    pub code: Option<String>,
    /// Optional HTTP status.
    pub status: Option<i64>,
    /// `MissingCredential` marker — becomes `not_configured` evidence.
    pub missing_credential: bool,
}

impl From<DispatchError> for CfError {
    fn from(error: DispatchError) -> Self {
        CfError::Python(Box::leak(error.class.into_boxed_str()), error.message)
    }
}

impl DispatchError {
    /// A plain error with a class and message and no extra fields.
    pub fn plain(class: &str, message: &str) -> Self {
        Self {
            class: class.to_owned(),
            message: message.to_owned(),
            code: None,
            status: None,
            missing_credential: false,
        }
    }
}

/// Registry row: config components, access, operations, quota mode and privacy class.
pub type RegistryEntry = (
    &'static [&'static str],
    &'static str,
    &'static [&'static str],
    &'static str,
    &'static str,
);

/// The provider-function dispatch seam.
pub type Dispatch<'a> = &'a dyn Fn(&str, Option<&Value>) -> Result<Value, DispatchError>;

/// Scan-opening seam: returns `(page_count, pages, scan_uuid)`.
pub type OpenScan<'a> = &'a dyn Fn(&str) -> Result<(i64, Vec<Value>, String), CfError>;

/// Credential status: per-component configured flag and detail text.
pub type CredentialDetails = (Map<String, Value>, Map<String, Value>);

/// The provider-lifecycle event sink.
pub type EventSink<'a> = &'a dyn Fn(&str, Value);

/// Everything the registry needs from the outside world. Provider calls are named routes
/// (`"gsc.discover_properties"`, `"metrika.report"`, …), so one seam drives live wiring and replayed tests.
pub struct ProviderEnv<'a> {
    /// Host env/filesystem seams used by `credentials`.
    pub host: &'a dyn Host,
    /// Config root holding provider credentials.
    pub root: &'a Path,
    /// Whether a Search Console service account is available.
    pub service_account_available: &'a dyn Fn() -> bool,
    /// Search Console service-account status string.
    pub service_account_status: &'a dyn Fn() -> String,
    /// Whether a durable OAuth grant exists for the provider.
    pub grant_available: &'a dyn Fn(&str) -> bool,
    /// Current UTC timestamp, RFC 3339 style.
    pub now: &'a dyn Fn() -> String,
    /// Monotonic clock for response timing.
    pub monotonic: &'a dyn Fn() -> f64,
    /// Provider function dispatch: `(route, request) -> result`.
    pub dispatch: Dispatch<'a>,
    /// Opens a scan: `(page_count, pages, scan_uuid)`.
    pub open_scan: OpenScan<'a>,
}

pub(super) const fn entry(
    components: &'static [&'static str],
    access: &'static str,
    operations: &'static [&'static str],
    quota: &'static str,
    privacy: &'static str,
) -> (
    &'static [&'static str],
    &'static str,
    &'static [&'static str],
    &'static str,
    &'static str,
) {
    (components, access, operations, quota, privacy)
}

pub(super) const WEBMASTER_OPS: &[&str] = &[
    "hosts",
    "indexing",
    "crawl",
    "sitemaps",
    "search_performance",
    "summary",
    "diagnostics",
    "sqi_history",
    "search_history",
    "query_history",
    "in_search_history",
    "events_history",
    "indexing_history",
    "important_urls",
    "broken_links_samples",
    "broken_links_history",
    "external_links_history",
    "external_links_samples",
];

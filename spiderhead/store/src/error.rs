//! Error type shared by every store module.

use thiserror::Error;

/// Failure of a store operation, one variant per failure kind.
#[derive(Debug, Error)]
pub enum StoreError {
    /// SQLite failure.
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// File-system failure.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// JSON (de)serialisation failure.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// The artifact does not satisfy its format contract (wrong application id, version, objects).
    #[error("invalid artifact: {0}")]
    Invalid(String),
    #[error(transparent)]
    EntityLimitError(#[from] crate::crawl_capture::EntityLimitError),
    #[error(transparent)]
    EntityDecodeError(#[from] crate::crawl_capture::EntityDecodeError),
    #[error(transparent)]
    UrlListError(#[from] crate::crawl_list_input::UrlListError),
    #[error(transparent)]
    ScanError(#[from] crate::scan_artifact::ScanError),
    #[error(transparent)]
    ScanBackpressure(#[from] crate::scan_artifact::ScanBackpressure),
    #[error(transparent)]
    AuditV2Error(#[from] crate::audit_v2::AuditV2Error),
    #[error(transparent)]
    AuditReadTimeout(#[from] crate::audit_v2::AuditReadTimeout),
    #[error(transparent)]
    UnmeasuredChecks(#[from] crate::issue_selection::UnmeasuredChecks),
    #[error(transparent)]
    AuditSizeError(#[from] crate::native_audit::AuditSizeError),
    #[error(transparent)]
    LinksError(#[from] crate::url_links::LinksError),
    #[error(transparent)]
    Budget(#[from] crate::url_links::Budget),
    #[error(transparent)]
    QueryError(#[from] crate::url_query_schema::QueryError),
    #[error(transparent)]
    RequestBudgetExhausted(#[from] crate::crawl_records::throttle::RequestBudgetExhausted),
    /// Feature not implemented yet.
    #[error("not ported yet: {0}")]
    NotPorted(&'static str),
    #[error(transparent)]
    LedgerError(#[from] crate::ledger::schema::LedgerError),
}

/// Result alias used across the crate.
pub type StoreResult<T> = Result<T, StoreError>;

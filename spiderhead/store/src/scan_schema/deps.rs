//! Seam to functions owned by other modules, which may still be placeholders.
//!
//! Every call goes through [`deps`], so reconciling a changed owner signature touches only [`Owners`].
//! Unit tests can swap the implementation with [`with_deps`].

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use rusqlite::Connection;
use serde_json::{Map, Value};

use crate::error::StoreError;
use crate::native_scan::NativeScan;

type R<T> = Result<T, StoreError>;

/// Functions owned by other modules that the schema layer calls.
pub trait SchemaDeps: Send + Sync {
    /// Redact secret values from the effective settings.
    fn redact_config(&self, settings: &Value) -> R<Value>;
    /// Check the recorded credential snapshot.
    fn validate_recorded_credentials(&self, snapshot: &Value) -> R<Value>;
    /// Credential check against the live scan; `Some` carries the reason when it fails.
    fn credential_verifier(&self, live: &Value, scan_uuid: &str) -> R<Option<String>>;
    /// Validate one context item against the scan's sitemap roots.
    fn validate_context(
        &self,
        con: &Connection,
        item: &Map<String, Value>,
        sitemap_roots: Option<&BTreeSet<i64>>,
    ) -> R<()>;
    /// Validate the rendered-route ledger.
    fn validate_ledger(&self, con: &Connection) -> R<()>;
    /// Validate the scan.v2 structures; an audit is not required.
    fn validate_v2(&self, con: &Connection) -> R<()>;
    /// Upgrade to scan.v2, creating the resource-graph, event, transport and discovery-ledger tables
    /// when a scan.v2 is created.
    fn upgrade_to_v2(&self, con: &Connection) -> R<()>;
    /// Ids of the selected sitemap roots.
    fn sitemap_root_ids(&self, con: &Connection) -> R<BTreeSet<i64>>;
    /// Record a sitemap root with its source and ordinal.
    fn declare_sitemap(&self, con: &Connection, url: &str, source: &str, ordinal: i64) -> R<()>;
    /// Validate the retained corpus against the scan and its retention policy.
    fn validate_corpus(
        &self,
        con: &Connection,
        scan: &Map<String, Value>,
        retention: &Map<String, Value>,
    ) -> R<()>;
    /// Summary of the retained corpus under the retention policy.
    fn corpus_summary(&self, con: &Connection, retention: &Map<String, Value>) -> R<Value>;
    /// Reanalysis capability state of the scan.
    fn reanalysis_state(&self, con: &Connection) -> R<String>;
    /// Open the scan as a writer, taking over the connection and the lock file.
    fn open_native(
        &self,
        path: &Path,
        con: Connection,
        lock: Option<std::fs::File>,
    ) -> R<NativeScan>;
    /// Record an anonymous-session resume, flagged as inferred or not.
    fn record_anonymous_session_resume(&self, scan: &NativeScan, inferred: bool) -> R<()>;
}

fn to_map(value: &Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

/// The real implementations, forwarding to the owning modules.
pub struct Owners;

impl SchemaDeps for Owners {
    fn redact_config(&self, settings: &Value) -> R<Value> {
        crate::credential_context::redact_config(&to_map(settings)).map(Value::Object)
    }

    fn validate_recorded_credentials(&self, snapshot: &Value) -> R<Value> {
        crate::credential_context::validate_recorded_credentials(&to_map(snapshot))
            .map(Value::Object)
    }

    fn credential_verifier(&self, live: &Value, scan_uuid: &str) -> R<Option<String>> {
        crate::credential_context::credential_verifier(&to_map(live), scan_uuid)
    }

    fn validate_context(
        &self,
        con: &Connection,
        item: &Map<String, Value>,
        sitemap_roots: Option<&BTreeSet<i64>>,
    ) -> R<()> {
        crate::native_context::validate_context(con, item, sitemap_roots)
    }

    fn validate_ledger(&self, con: &Connection) -> R<()> {
        crate::rendered_routes::validate_ledger(con)
    }

    fn validate_v2(&self, con: &Connection) -> R<()> {
        crate::retry::validate_v2(con, false)
    }

    fn upgrade_to_v2(&self, con: &Connection) -> R<()> {
        crate::retry::upgrade_to_v2(con)?;
        crate::resource_graph::ensure_schema(con)?;
        crate::events::ensure_schema(con)?;
        crate::transport::ensure_schema(con)?;
        crate::discovery_ledger::ensure_schema(con)
    }

    fn sitemap_root_ids(&self, con: &Connection) -> R<BTreeSet<i64>> {
        crate::sitemaps::root_ids(con)
    }

    fn declare_sitemap(&self, con: &Connection, url: &str, source: &str, ordinal: i64) -> R<()> {
        crate::sitemaps::declare(
            con,
            &crate::native_scan::NativeContextSink,
            url,
            source,
            ordinal,
        )
        .map(|_| ())
    }

    fn validate_corpus(
        &self,
        con: &Connection,
        scan: &Map<String, Value>,
        retention: &Map<String, Value>,
    ) -> R<()> {
        crate::corpus_validation::validate_corpus(con, scan, retention, true)
    }

    fn corpus_summary(&self, con: &Connection, retention: &Map<String, Value>) -> R<Value> {
        crate::corpus::corpus_summary(con, &Value::Object(retention.clone())).map(Value::Object)
    }

    fn reanalysis_state(&self, con: &Connection) -> R<String> {
        Ok(crate::native_scan::reanalysis_capability(con)?
            .get("state")
            .cloned()
            .unwrap_or_default())
    }

    fn open_native(
        &self,
        path: &Path,
        con: Connection,
        lock: Option<std::fs::File>,
    ) -> R<NativeScan> {
        NativeScan::new(path, con, lock)
    }

    fn record_anonymous_session_resume(&self, scan: &NativeScan, inferred: bool) -> R<()> {
        crate::scan_bodies::NativeScanBodies::record_anonymous_session_resume(scan, inferred)
    }
}

#[cfg(test)]
thread_local! {
    static OVERRIDE: std::cell::RefCell<Option<Arc<dyn SchemaDeps>>> = const { std::cell::RefCell::new(None) };
}

/// The implementation in effect for this thread: [`Owners`] unless a test replaced it.
pub fn deps() -> Arc<dyn SchemaDeps> {
    #[cfg(test)]
    if let Some(found) = OVERRIDE.with(|cell| cell.borrow().clone()) {
        return found;
    }
    Arc::new(Owners)
}

/// Run `body` with `replacement` standing in for the owners (tests only).
#[cfg(test)]
pub fn with_deps<T>(replacement: Arc<dyn SchemaDeps>, body: impl FnOnce() -> T) -> T {
    let previous = OVERRIDE.with(|cell| cell.replace(Some(replacement)));
    let result = body();
    OVERRIDE.with(|cell| cell.replace(previous));
    result
}

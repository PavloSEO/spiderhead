//! Read-only inspection (`inspect`) and bounded progress metadata (`observe`).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Map, Value};
use spiderhead_core::sqlite::open_readonly;

use super::lifecycle::message_of;
use super::{bad, NativeScanSchemaMixin};
use crate::error::StoreError;
use crate::history::regular;
use crate::scan_artifact::{query_maps, runtime, APPLICATION_ID, MAX_RECORD_BYTES, USER_VERSION};

/// Inspection budget: at least 30 s, otherwise the artifact size over 4 MiB/s.
///
/// Default timeout for an inspect pass; the rate is documented at `INSPECT_BYTES_PER_SECOND`.
fn default_timeout(path: &Path) -> f64 {
    let size = std::fs::metadata(path).map(|m| m.len() as i64).unwrap_or(0);
    super::default_inspect_timeout(size)
}

impl NativeScanSchemaMixin {
    /// Validate an unfinished native scan read-only; it is not a report reader.
    ///
    /// `timeout_seconds` defaults to unset, derived from the artifact size.
    pub fn inspect(
        path: &Path,
        timeout_seconds: Option<f64>,
    ) -> Result<Map<String, Value>, StoreError> {
        runtime()?;
        let source = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let timeout = timeout_seconds.unwrap_or_else(|| default_timeout(&source));
        if !timeout.is_finite() || timeout <= 0.0 {
            return Err(bad("native inspection timeout must be positive"));
        }
        let start = Instant::now();
        let timed_out = Arc::new(AtomicBool::new(false));
        let outcome = Self::inspect_inner(&source, start, timeout, &timed_out);
        outcome.map_err(|error| {
            if timed_out.load(Ordering::Relaxed) {
                let size = std::fs::metadata(&source)
                    .map(|m| format!("{} bytes", m.len()))
                    .unwrap_or_else(|_| "unknown size".to_owned());
                return bad(format!(
                    "native scan inspection exceeded its {timeout:.1}s budget after {:.1}s \
                     (artifact: {size}); pass a larger timeout_seconds for a large or \
                     slow-to-read artifact",
                    start.elapsed().as_secs_f64()
                ));
            }
            bad(format!(
                "cannot inspect native scan: {}",
                message_of(&error)
            ))
        })
    }

    fn inspect_inner(
        source: &Path,
        start: Instant,
        timeout: f64,
        timed_out: &Arc<AtomicBool>,
    ) -> Result<Map<String, Value>, StoreError> {
        let con = open_readonly(
            source,
            Duration::from_secs(5),
            &[
                "PRAGMA trusted_schema=OFF",
                "PRAGMA query_only=ON",
                "PRAGMA foreign_keys=ON",
            ],
        )?;
        let deadline = start + Duration::from_secs_f64(timeout);
        let flag = Arc::clone(timed_out);
        // Runs inside SQLite's progress handler; a plain flag tells "we cancelled this on
        // purpose" apart from any other SQLite failure in the caller.
        con.progress_handler(
            10_000,
            Some(move || {
                if Instant::now() > deadline {
                    flag.store(true, Ordering::Relaxed);
                    return true;
                }
                false
            }),
        );
        con.execute_batch("BEGIN")?;
        Self::validate_native(&con)?;
        let scan = query_maps(&con, "SELECT * FROM scan WHERE singleton=1")?
            .into_iter()
            .next()
            .ok_or_else(|| bad("native scan requires exactly one header"))?;
        let mut counts = Map::new();
        for table in [
            "pages",
            "links",
            "forms",
            "decisions",
            "frontier",
            "query_variants",
        ] {
            let n: i64 = con.query_row(&format!("SELECT COUNT(*) FROM \"{table}\""), [], |r| {
                r.get(0)
            })?;
            counts.insert(table.to_owned(), Value::from(n));
        }
        let mut out = Map::new();
        out.insert("scan".into(), Value::Object(scan));
        out.insert("counts".into(), Value::Object(counts));
        Ok(out)
    }

    /// Read bounded progress metadata, explicitly without validating evidence.
    ///
    /// Callers use durable progress receipts for frequent ticks. This read-only fallback is
    /// for missing receipts; it must never imply audit acceptance.
    /// `timeout_seconds` defaults to 0.2.
    pub fn observe(path: &Path, timeout_seconds: f64) -> Result<Map<String, Value>, StoreError> {
        if !timeout_seconds.is_finite() || !(timeout_seconds > 0.0 && timeout_seconds <= 5.0) {
            return Err(bad(
                "observation timeout must be positive and at most 5 seconds",
            ));
        }
        let source = std::path::absolute(path)?;
        regular(&source)?;
        let before = link_identity(&source)?;
        let con = open_readonly(
            &source,
            Duration::from_secs_f64(timeout_seconds),
            &[
                "PRAGMA trusted_schema=OFF",
                "PRAGMA query_only=ON",
                "PRAGMA cache_size=-2048",
            ],
        )?;
        let deadline = Instant::now() + Duration::from_secs_f64(timeout_seconds);
        con.progress_handler(1000, Some(move || Instant::now() > deadline));
        let unavailable = |e: rusqlite::Error| {
            bad(format!(
                "progress metadata unavailable within bounded read: {e}"
            ))
        };
        let read = || -> Result<Map<String, Value>, StoreError> {
            con.execute_batch("BEGIN")?;
            let version: i64 = con.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            let application: i64 = con.query_row("PRAGMA application_id", [], |r| r.get(0))?;
            if application != APPLICATION_ID || (version != USER_VERSION && version != 2) {
                return Err(bad("unsupported progress artifact identity"));
            }
            let columns = [
                "scan_uuid",
                "format_version",
                "lifecycle",
                "finish_reason",
                "evidence_revision",
                "crawl_partial",
                "corpus_partial",
                "start_url",
            ];
            let size_sql = columns
                .iter()
                .map(|name| format!("COALESCE(length(CAST({name} AS BLOB)),0)"))
                .collect::<Vec<_>>()
                .join("+");
            let size: Option<i64> = con
                .query_row(
                    &format!("SELECT {size_sql} FROM scan WHERE singleton=1"),
                    [],
                    |r| r.get(0),
                )
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })?;
            if size.is_none_or(|n| n > MAX_RECORD_BYTES) {
                return Err(bad("progress header is missing or oversized"));
            }
            let header = query_maps(
                &con,
                &format!("SELECT {} FROM scan WHERE singleton=1", columns.join(",")),
            )?
            .into_iter()
            .next()
            .unwrap_or_default();
            if header.get("format_version").and_then(Value::as_str)
                != Some(&format!("scan.v{version}"))
            {
                return Err(bad("progress format header disagrees"));
            }
            let pages: i64 = con.query_row("SELECT COUNT(*) FROM pages", [], |r| r.get(0))?;
            let mut counts = Map::new();
            counts.insert("pages".into(), Value::from(pages));
            for row in query_maps(
                &con,
                "SELECT state,COUNT(*) AS n FROM frontier GROUP BY state",
            )? {
                if let (Some(state), Some(n)) =
                    (row.get("state").and_then(Value::as_str), row.get("n"))
                {
                    counts.insert(state.to_owned(), n.clone());
                }
            }
            for state in ["queued", "inflight", "done", "excluded"] {
                counts.entry(state).or_insert(Value::from(0));
            }
            let after = link_identity(&source)?;
            if (before.0, before.1) != (after.0, after.1) {
                return Err(bad("progress artifact identity changed"));
            }
            let mut out = Map::new();
            out.insert("scan".into(), Value::Object(header));
            out.insert("counts".into(), Value::Object(counts));
            out.insert("validation".into(), Value::from("metadata_only"));
            Ok(out)
        };
        read().map_err(|error| match error {
            StoreError::Sqlite(e) => unavailable(e),
            other => other,
        })
    }
}

use spiderhead_core::filesystem::link_identity;

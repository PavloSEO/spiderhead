//! Transactional single-writer storage for unfinished native scan artifacts.
//!
//! The struct lives here with `pub(crate)` state; the other layers add their methods as further
//! `impl NativeScan` blocks in their own files. The writer primitives they share (`con`, `begin`,
//! `rollback`, `note`, `assert_mutable`, `hit`, `event`) are defined below.

use crate::audit_v2::{write_audit_v2, AuditV2Reader};
use crate::crawl_events::{timestamp, validate as validate_event, EventSink, FORMAT, MAX_EVENTS};
use crate::error::StoreError;
use crate::events;
use crate::external_checks::{summary_item, validate_check, validate_summary, MAX_EXTERNAL_CHECKS};
use crate::native_audit::validate_audit;
use crate::scan_artifact::{
    audit as parse_audit, dump, expected, insert, sha, url as intern_url, validate_audit_document,
    ColumnInfo, ScanBackpressure, ScanError, MAX_JSON_BYTES, MAX_RECORD_BYTES,
};
use crate::scan_lease::{bounded_items, json_chunks, utc, MAX_PAGE_COMMIT_ITEMS};
use crate::{corpus, sitemaps};
use indexmap::IndexMap;
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const BODY_TABLES: &[&str] = &["bodies", "responses", "documents", "resource_refs"];

/// Sorted for a stable output order.
pub const NATIVE_CONTEXT: &[&str] = &["native_commit", "robots_blocked_url"];

// `None` in `NativeScan::event`'s `occurred_at: Option<&Value>` stamps the current time;
// `Some(&Value::Null)` records an unknown time.

pub const WAL_BACKPRESSURE_BYTES: i64 = 67108864;

pub const WAL_CHECKPOINT_WAIT_SECONDS: f64 = 30.0;

pub const WAL_WAIT_NOTE: &str = "a write waited for a reader to release the bounded WAL checkpoint";

fn scan_err(message: impl Into<String>) -> StoreError {
    StoreError::ScanError(ScanError {
        message: message.into(),
    })
}

/// Seam to `native_context::put_context`; forwards to the context writer.
fn put_context(
    con: &Connection,
    item: &Map<String, Value>,
    sitemap_roots: Option<&BTreeSet<i64>>,
) -> Result<(), StoreError> {
    crate::native_context::put_context(con, item, sitemap_roots)
}

/// [`sitemaps::ContextSink`] backed by the scan's context writer.
pub(crate) struct NativeContextSink;

impl sitemaps::ContextSink for NativeContextSink {
    fn put_context(&self, con: &Connection, item: &Map<String, Value>) -> Result<(), StoreError> {
        put_context(con, item, None)
    }
}

/// Event-timeline bookkeeping of a scan.v2 writer. Events go straight to the saved timeline;
/// only the count is kept in memory.
#[derive(Debug, Clone, Copy)]
struct EventState {
    cap: i64,
    captured: i64,
    dropped: i64,
}

/// `SELECT * FROM scan WHERE singleton=1` as an ordered JSON object.
fn scan_row(con: &Connection) -> Result<Map<String, Value>, StoreError> {
    let mut statement = con.prepare("SELECT * FROM scan WHERE singleton=1")?;
    let names: Vec<String> = statement
        .column_names()
        .iter()
        .map(|n| n.to_string())
        .collect();
    let row = statement.query_row([], |row| {
        let mut out = Map::new();
        for (index, name) in names.iter().enumerate() {
            let value = match row.get::<_, SqlValue>(index)? {
                SqlValue::Null => Value::Null,
                SqlValue::Integer(n) => json!(n),
                SqlValue::Real(f) => json!(f),
                SqlValue::Text(s) => Value::String(s),
                SqlValue::Blob(b) => json!(b),
            };
            out.insert(name.clone(), value);
        }
        Ok(out)
    })?;
    Ok(row)
}

fn json_column(con: &Connection, sql: &str) -> Result<Value, StoreError> {
    let text: String = con.query_row(sql, [], |row| row.get(0))?;
    Ok(serde_json::from_str(&text)?)
}

/// Test hook fired by [`NativeScan::hit`].
pub type Failpoint = Box<dyn Fn(&str)>;

/// Cached `(corpus summary, reanalysis capability)` pair.
type CachedSummary = (BTreeMap<String, Value>, Option<BTreeMap<String, String>>);

/// Audit header plus its collections keyed by JSON pointer, in document order.
pub type AuditParts = (Map<String, Value>, IndexMap<String, Vec<Value>>);

/// One locked native scan writer; fetch workers must not own this object.
pub struct NativeScan {
    pub(crate) path: PathBuf,
    /// `None` once closed.
    pub(crate) con: Option<Connection>,
    /// Exclusive advisory lock file; dropping it releases the lock.
    pub(crate) lock: Option<File>,
    pub failpoint: Option<Failpoint>,
    events: Cell<Option<EventState>>,
    sparse_summary: Option<CachedSummary>,
    stable_corpus_summary: Option<CachedSummary>,
    #[allow(dead_code)] // read by the page-commit layer (scan_lease)
    pub(crate) page_ordinal_next: Option<i64>,
    #[allow(dead_code)] // read by the page-commit layer (scan_lease)
    pub(crate) page_columns: BTreeMap<String, ColumnInfo>,
    #[allow(dead_code)] // read by the page-commit layer (scan_lease)
    pub(crate) v2_pages: bool,
    /// Test seams that override module constants.
    pub wal_backpressure_bytes: i64,
    pub wal_checkpoint_wait_seconds: f64,
}

impl NativeScan {
    /// Adopt an opened writer connection and the exclusive lock file; on a `scan.v2` artifact,
    /// resume the saved event timeline and refuse inconsistent coverage.
    pub fn new(
        path: &Path,
        connection: Connection,
        lock: Option<File>,
    ) -> Result<Self, StoreError> {
        let page_columns = expected()?
            .columns
            .get("pages")
            .map(|columns| {
                columns
                    .iter()
                    .map(|c| (c.name.clone(), c.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let user_version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let v2_pages = user_version == 2;
        let mut event_state = None;
        if user_version == 2 {
            events::ensure_schema(&connection)?;
            let prior: i64 = connection.query_row(
                "SELECT COALESCE(MAX(sequence),0) FROM scan_events",
                [],
                |r| r.get(0),
            )?;
            let meta: Option<(SqlValue, SqlValue, SqlValue)> = connection
                .query_row(
                    "SELECT cap,captured,dropped FROM scan_event_meta WHERE singleton=1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            let invalid = || scan_err("saved event timeline coverage is invalid");
            let (cap, dropped) = match &meta {
                None => (MAX_EVENTS, 0),
                Some((SqlValue::Integer(cap), captured, dropped)) => {
                    let (SqlValue::Integer(captured), SqlValue::Integer(dropped)) =
                        (captured, dropped)
                    else {
                        return Err(invalid());
                    };
                    if *captured != prior || *dropped < 0 {
                        return Err(invalid());
                    }
                    (*cap, *dropped)
                }
                Some(_) => return Err(invalid()),
            };
            if !(1..=MAX_EVENTS).contains(&cap) || prior > cap {
                return Err(invalid());
            }
            event_state = Some(EventState {
                cap,
                captured: prior,
                dropped,
            });
        }
        Ok(Self {
            path: path.to_path_buf(),
            con: Some(connection),
            lock,
            failpoint: None,
            events: Cell::new(event_state),
            sparse_summary: None,
            stable_corpus_summary: None,
            page_ordinal_next: None,
            page_columns,
            v2_pages,
            wal_backpressure_bytes: WAL_BACKPRESSURE_BYTES,
            wal_checkpoint_wait_seconds: WAL_CHECKPOINT_WAIT_SECONDS,
        })
    }

    /// The writer connection (panics after [`close`](Self::close)).
    ///
    /// Callers that can propagate errors should prefer [`try_con`](Self::try_con).
    pub fn con(&self) -> &Connection {
        self.con.as_ref().expect("native scan is closed")
    }

    /// The writer connection, or `StoreError::Invalid` after [`close`](Self::close).
    pub fn try_con(&self) -> Result<&Connection, StoreError> {
        self.con
            .as_ref()
            .ok_or_else(|| StoreError::Invalid("native scan is closed".into()))
    }

    /// Path of the scan artifact.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one typed event to the saved timeline (no-op on `scan.v1`). `occurred_at = None`
    /// stamps the current time; `Some(&Value::Null)` records an unknown time.
    pub fn event(
        &self,
        event_type: &str,
        payload: &Map<String, Value>,
        occurred_at: Option<&Value>,
    ) -> Result<(), StoreError> {
        let Some(mut state) = self.events.get() else {
            return Ok(());
        };
        if state.captured >= state.cap {
            state.dropped += 1;
            self.events.set(Some(state));
            return Ok(());
        }
        let (stamp, stamp_state) = timestamp(occurred_at)?;
        let record = json!({
            "format": FORMAT,
            "sequence": state.captured + 1,
            "event_type": event_type,
            "occurred_at": stamp,
            "timestamp_state": stamp_state,
            "payload": payload,
        });
        let event: Map<String, Value> = validate_event(&record)?.into_iter().collect();
        let con = self.try_con()?;
        events::append(con, &event)?;
        state.captured += 1;
        self.events.set(Some(state));
        Ok(())
    }

    /// Persist the timeline's coverage row.
    pub fn event_coverage(&self) -> Result<(), StoreError> {
        let Some(state) = self.events.get() else {
            return Ok(());
        };
        let coverage = json!({
            "state": if state.dropped > 0 { "partial" } else { "complete" },
            "captured": state.captured,
            "dropped": state.dropped,
            "cap": state.cap,
        });
        let Value::Object(coverage) = coverage else {
            unreachable!("object literal")
        };
        events::set_coverage(self.con(), &coverage)
    }

    /// Persist bounded adapter events in one writer transaction after worker fold-back.
    pub fn record_events(&mut self, sink: &EventSink) -> Result<(), StoreError> {
        if self.events.get().is_none() {
            return Ok(());
        }
        let snapshot = sink.snapshot();
        let dropped = sink
            .coverage()
            .get("dropped")
            .and_then(Value::as_i64)
            .filter(|n| *n >= 0)
            .ok_or_else(|| scan_err("event sink is invalid"))?;
        let Some(Value::Array(raw_events)) = snapshot.get("events") else {
            return Err(scan_err("event sink is invalid"));
        };
        self.begin(true)?;
        let result = (|| {
            for raw in raw_events {
                let event = validate_event(raw)?;
                let payload = match event.get("payload") {
                    Some(Value::Object(payload)) => payload.clone(),
                    _ => return Err(scan_err("event sink is invalid")),
                };
                let event_type = event
                    .get("event_type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let occurred_at = event.get("occurred_at").cloned().unwrap_or(Value::Null);
                self.event(&event_type, &payload, Some(&occurred_at))?;
            }
            if let Some(mut state) = self.events.get() {
                state.dropped += dropped;
                self.events.set(Some(state));
            }
            self.event_coverage()?;
            self.con().execute_batch("COMMIT")?;
            Ok(())
        })();
        if result.is_err() {
            self.rollback();
        }
        result
    }

    pub fn assert_mutable(&self) -> Result<(), StoreError> {
        let lifecycle: String =
            self.con()
                .query_row("SELECT lifecycle FROM scan WHERE singleton=1", [], |r| {
                    r.get(0)
                })?;
        if matches!(lifecycle.as_str(), "finished" | "failed") {
            return Err(scan_err(format!(
                "native scan lifecycle '{lifecycle}' is immutable"
            )));
        }
        Ok(())
    }

    /// Close the connection and release the writer lock; idempotent.
    pub fn close(&mut self) {
        if let Some(con) = self.con.take() {
            let _ = con.close();
        }
        self.lock = None;
    }

    /// Fire the test failpoint `name`, if one is installed.
    pub fn hit(&self, name: &str) {
        if let Some(failpoint) = &self.failpoint {
            failpoint(name);
        }
    }

    /// Open the writer transaction (`BEGIN IMMEDIATE`), retiring any legacy audit slot first.
    /// Defaults: the WAL bound is enforced.
    pub fn begin(&self, enforce_wal_bound: bool) -> Result<(), StoreError> {
        let waited = if enforce_wal_bound {
            self.enforce_wal_bound()?
        } else {
            false
        };
        let con = self.con();
        con.execute_batch("BEGIN IMMEDIATE")?;
        con.execute("DELETE FROM audit", [])?;
        if waited {
            // Say so in the artifact itself: a run that quietly stalls for half a minute per
            // write is the silence this registry exists to break. The note is diagnostic, so a
            // full registry drops it rather than turning a survived pause into a failed write.
            match self.note(WAL_WAIT_NOTE) {
                Ok(()) | Err(StoreError::ScanError(_)) => {}
                Err(other) => return Err(other),
            }
        }
        Ok(())
    }

    /// Checkpoint an oversized WAL, waiting out readers; True if it waited.
    ///
    /// ``TRUNCATE`` answers busy while any reader holds a read transaction, and
    /// reading a scan while it is written is what this lane is for -- scan-list
    /// and scan-inspect do it deliberately, and a WAL reader cannot block the
    /// writer. Retrying inside a bounded deadline lets such a reader finish
    /// instead of ending a multi-hour crawl on one unlucky instant. A checkpoint
    /// still blocked at the deadline is backpressure the caller may wait on; a
    /// WAL that stays oversized with nothing blocking it remains fatal.
    pub fn enforce_wal_bound(&self) -> Result<bool, StoreError> {
        let mut wal = self.path.clone().into_os_string();
        wal.push("-wal");
        let wal = PathBuf::from(wal);
        let limit = self.wal_backpressure_bytes as u64;
        let oversized = |wal: &Path| std::fs::metadata(wal).is_ok_and(|m| m.len() > limit);
        if !oversized(&wal) {
            return Ok(false);
        }
        let con = self.con();
        // Poll the checkpoint ourselves instead of letting the connection's busy timeout absorb
        // each attempt, so the deadline below is the only bound.
        let prior_timeout: i64 = con.query_row("PRAGMA busy_timeout", [], |r| r.get(0))?;
        con.execute_batch("PRAGMA busy_timeout=0")?;
        let deadline = Instant::now() + Duration::from_secs_f64(self.wal_checkpoint_wait_seconds);
        let mut delay = 0.01_f64;
        let mut waited = false;
        let mut busy = 0_i64;
        let outcome = (|| -> Result<bool, StoreError> {
            loop {
                busy = con.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))?;
                if !oversized(&wal) {
                    return Ok(true);
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Ok(false);
                }
                std::thread::sleep(remaining.min(Duration::from_secs_f64(delay)));
                delay = (delay * 2.0).min(0.5);
                waited = true;
            }
        })();
        con.execute_batch(&format!("PRAGMA busy_timeout={prior_timeout}"))?;
        if outcome? {
            return Ok(waited);
        }
        if busy != 0 {
            return Err(StoreError::ScanBackpressure(ScanBackpressure {
                message: format!(
                    "WAL backpressure: a reader kept the bounded log from being checkpointed for {}s",
                    self.wal_checkpoint_wait_seconds
                ),
            }));
        }
        Err(scan_err(
            "WAL backpressure: checkpoint could not reduce the bounded log",
        ))
    }

    /// Roll back the open writer transaction, if any.
    pub fn rollback(&self) {
        if let Some(con) = &self.con {
            if !con.is_autocommit() {
                let _ = con.execute_batch("ROLLBACK");
            }
        }
    }

    /// Run the body in one transaction: commit on success, roll back and re-raise on failure.
    pub(crate) fn transact<T>(
        &self,
        body: impl FnOnce(&Connection) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.begin(true)?;
        let con = self.con();
        let result = body(con).and_then(|value| {
            con.execute_batch("COMMIT")?;
            Ok(value)
        });
        if result.is_err() {
            self.rollback();
        }
        result
    }

    /// The context payload of `(kind, key)`, or `None`.
    pub fn read_context(&self, kind: &str, key: &str) -> Result<Option<Value>, StoreError> {
        let text: Option<String> = self
            .con()
            .query_row(
                "SELECT payload_json FROM context_items WHERE kind=? AND item_key=?",
                [kind, key],
                |r| r.get(0),
            )
            .optional()?;
        text.map(|text| serde_json::from_str(&text).map_err(StoreError::from))
            .transpose()
    }

    fn require_v2(&self) -> Result<(), StoreError> {
        let version: String = self.con().query_row(
            "SELECT format_version FROM scan WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if version == "scan.v2" {
            Ok(())
        } else {
            Err(scan_err("external checks require scan.v2 retained storage"))
        }
    }

    /// Persist one bounded external outcome in stable decision order.
    pub fn record_external_check(
        &self,
        ordinal: i64,
        payload: &Map<String, Value>,
    ) -> Result<(), StoreError> {
        self.assert_mutable()?;
        self.require_v2()?;
        let payload_json = dump(&Value::Object(payload.clone()));
        let mut item = Map::new();
        for (key, value) in [
            ("kind", "external_check".to_string()),
            ("item_key", ordinal.to_string()),
            ("payload_version", "scan_context.v1".to_string()),
            ("payload_json", payload_json.clone()),
            ("completeness", "complete".to_string()),
            ("reason", String::new()),
        ] {
            item.insert(key.to_string(), Value::String(value));
        }
        validate_check(&item, &Value::Object(payload.clone()))?;
        if !(0..MAX_EXTERNAL_CHECKS).contains(&ordinal) {
            return Err(scan_err(
                "external check ordinal is outside the retained bound",
            ));
        }
        let outcome = payload
            .get("outcome")
            .and_then(Value::as_str)
            .ok_or_else(|| scan_err("external check outcome is missing"))?
            .to_string();
        self.transact(|con| {
            let existing: Option<String> = con
                .query_row(
                    "SELECT payload_json FROM external_checks WHERE ordinal=?",
                    [ordinal],
                    |r| r.get(0),
                )
                .optional()?;
            if existing.is_some_and(|existing| existing != payload_json) {
                return Err(scan_err(
                    "external check retry disagrees with retained outcome",
                ));
            }
            con.execute(
                "INSERT OR IGNORE INTO external_checks(ordinal,outcome,payload_json) VALUES(?,?,?)",
                rusqlite::params![ordinal, outcome, payload_json],
            )?;
            Ok(())
        })
    }

    /// Retained external outcomes in decision order (empty on `scan.v1`).
    pub fn external_checks(&self) -> Result<Vec<Value>, StoreError> {
        if self.require_v2().is_err() {
            return Ok(Vec::new());
        }
        let mut statement = self
            .con()
            .prepare("SELECT payload_json FROM external_checks ORDER BY ordinal")?;
        let texts = statement
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        texts
            .iter()
            .map(|text| serde_json::from_str(text).map_err(StoreError::from))
            .collect()
    }

    pub fn record_external_checks_summary(
        &self,
        summary: &Map<String, Value>,
    ) -> Result<(), StoreError> {
        self.assert_mutable()?;
        self.require_v2()?;
        let item = summary_item(summary)?;
        validate_summary(&item, &Value::Object(summary.clone()))?;
        let payload_json = item
            .get("payload_json")
            .and_then(Value::as_str)
            .ok_or_else(|| scan_err("external check summary item has no payload"))?
            .to_string();
        self.transact(|con| {
            con.execute(
                "INSERT INTO external_check_summary(singleton,payload_json) VALUES(1,?) \
                 ON CONFLICT(singleton) DO UPDATE SET payload_json=excluded.payload_json",
                [&payload_json],
            )?;
            Ok(())
        })
    }

    /// Update declared corpus availability in the evidence transaction.
    /// `committed_document_id` and `resource_inventory_state` default to none.
    pub fn sync_corpus(
        &mut self,
        committed_document_id: Option<i64>,
        resource_inventory_state: Option<&str>,
    ) -> Result<(), StoreError> {
        // Field-level borrow: sync_corpus assigns self.*_summary while `con` is still live.
        let con = self
            .con
            .as_ref()
            .ok_or_else(|| StoreError::Invalid("native scan is closed".into()))?;
        let (capabilities_json, retention_json, source_kind, config_json): (
            String,
            String,
            String,
            String,
        ) = con.query_row(
            "SELECT capabilities_json,retention_json,source_kind,config_json FROM scan",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        let mut capabilities: Map<String, Value> = serde_json::from_str(&capabilities_json)?;
        let policy_value: Value = serde_json::from_str(&retention_json)?;
        let policy: Map<String, Value> = policy_value
            .as_object()
            .ok_or_else(|| scan_err("scan retention policy is not an object"))?
            .clone();
        let config: Value = serde_json::from_str(&config_json)?;
        let native = source_kind == "native";
        let fetch_resources = config
            .pointer("/resources/fetch")
            .is_some_and(|v| v.as_bool().unwrap_or(!v.is_null()));
        let evidence_present: i64 = con.query_row(
            "SELECT EXISTS(SELECT 1 FROM responses) OR EXISTS(SELECT 1 FROM documents) \
             OR EXISTS(SELECT 1 FROM bodies) OR EXISTS(SELECT 1 FROM resource_refs)",
            [],
            |r| r.get(0),
        )?;
        let sparse = native
            && policy.get("body_mode").and_then(Value::as_str) == Some("off")
            && !fetch_resources
            && evidence_present == 0;
        let summary: BTreeMap<String, Value>;
        let reanalysis: Option<BTreeMap<String, String>>;
        let stable = self.stable_corpus_summary.clone();
        if let (Some((stable_summary, stable_reanalysis)), Some(document_id)) =
            (stable, committed_document_id)
        {
            let states: Option<(Option<String>, Option<String>)> = con
                .query_row(
                    "SELECT d.body_state,r.body_state FROM documents d \
                     JOIN responses r ON r.response_id=d.source_response_id WHERE d.document_id=?",
                    [document_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let both_complete = states.is_some_and(|(document, response)| {
                document.as_deref() == Some("complete") && response.as_deref() == Some("complete")
            });
            if both_complete && resource_inventory_state == Some("complete") {
                summary = stable_summary;
                reanalysis = stable_reanalysis;
            } else {
                self.stable_corpus_summary = None;
                summary = corpus::corpus_summary(con, &policy_value)?
                    .into_iter()
                    .collect();
                reanalysis = if native {
                    Some(reanalysis_capability(con)?)
                } else {
                    None
                };
            }
        } else if let (true, Some((cached, cached_reanalysis))) =
            (sparse, self.sparse_summary.clone())
        {
            summary = cached;
            reanalysis = cached_reanalysis;
        } else {
            summary = corpus::corpus_summary(con, &policy_value)?
                .into_iter()
                .collect();
            reanalysis = if native {
                Some(reanalysis_capability(con)?)
            } else {
                None
            };
            if sparse {
                // Coverage is invariant while these evidence tables stay empty. The first full
                // calculation uses the existing contract; later commits reuse it only after
                // checking the invariant in SQL.
                self.sparse_summary = Some((summary.clone(), reanalysis.clone()));
            }
            // A normal static HTML page adds one complete response, document and resource
            // inventory record atomically. Once the complete state is reached it cannot regress
            // on another such append, so retain its exact public summary and prove the next
            // append's invariants with the small document lookup above instead of re-counting
            // the entire growing corpus on every page.
            let cap_state = |name: &str| -> Option<String> {
                summary
                    .get("capabilities")?
                    .get(name)?
                    .get("state")?
                    .as_str()
                    .map(str::to_string)
            };
            let corpus_partial = summary
                .get("corpus_partial")
                .is_some_and(|v| v.as_bool().unwrap_or(false));
            let resources = cap_state("resource_refs");
            let resource_bodies = cap_state("resource_bodies");
            if !corpus_partial
                && cap_state("responses").as_deref() == Some("complete")
                && cap_state("html_bodies").as_deref() == Some("complete")
                && cap_state("rendered_bodies").as_deref() == Some("unavailable")
                && resources.as_deref().is_none_or(|s| s == "complete")
                && resource_bodies
                    .as_deref()
                    .is_none_or(|s| !fetch_resources && s == "unavailable")
                && reanalysis
                    .as_ref()
                    .is_none_or(|r| r.get("state").map(String::as_str) == Some("complete"))
            {
                self.stable_corpus_summary = Some((summary.clone(), reanalysis.clone()));
            }
        }
        if let Some(Value::Object(update)) = summary.get("capabilities") {
            for (key, value) in update {
                capabilities.insert(key.clone(), value.clone());
            }
        }
        if let Some(reanalysis) = &reanalysis {
            capabilities.insert("offline_reanalysis".to_string(), json!(reanalysis));
        }
        let partial = summary
            .get("corpus_partial")
            .is_some_and(|v| v.as_bool().unwrap_or(false));
        con.execute(
            "UPDATE scan SET capabilities_json=?,corpus_partial=? WHERE singleton=1",
            rusqlite::params![dump(&Value::Object(capabilities)), i64::from(partial)],
        )?;
        Ok(())
    }

    pub fn declare_sitemap(
        &self,
        url: &str,
        source: &str,
        ordinal: i64,
    ) -> Result<i64, StoreError> {
        self.assert_mutable()?;
        json_chunks(
            &json!({"url": url, "source": source, "ordinal": ordinal}),
            MAX_RECORD_BYTES,
        )?;
        self.transact(|con| sitemaps::declare(con, &NativeContextSink, url, source, ordinal))
    }

    /// Selected sitemap roots (at most 5000), ordered by declaration ordinal.
    pub fn sitemap_roots(&self) -> Result<Vec<Map<String, Value>>, StoreError> {
        let con = self.con();
        let mut statement =
            con.prepare("SELECT payload_json FROM context_items WHERE kind='sitemap_declaration'")?;
        let texts = statement
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut roots = Vec::new();
        for text in texts {
            if roots.len() >= sitemaps::MAX_SELECTED_ROOTS as usize {
                return Err(scan_err("too many selected sitemap roots"));
            }
            let Value::Object(mut value) = serde_json::from_str::<Value>(&text)? else {
                return Err(scan_err("sitemap declaration is not an object"));
            };
            let url_id = value
                .get("sitemap_url_id")
                .and_then(Value::as_i64)
                .ok_or_else(|| scan_err("sitemap declaration has no URL id"))?;
            let url: String =
                con.query_row("SELECT url FROM urls WHERE url_id=?", [url_id], |r| {
                    r.get(0)
                })?;
            value.insert("url".to_string(), Value::String(url));
            roots.push(value);
        }
        roots.sort_by_key(|v| v.get("ordinal").and_then(Value::as_i64).unwrap_or(0));
        Ok(roots)
    }

    /// `(source ordinal, URL)` of one sitemap's members in source order.
    pub fn iter_sitemap_members(&self, sid: i64) -> Result<Vec<(i64, String)>, StoreError> {
        let con = self.con();
        let mut statement = con.prepare(
            "SELECT payload_json FROM context_items WHERE kind='sitemap_declared_url' \
             AND item_key LIKE ? ORDER BY CAST(substr(item_key,instr(item_key,':ordinal:')+9) AS INTEGER)",
        )?;
        let texts = statement
            .query_map([format!("sitemap:{sid}:ordinal:%")], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut members = Vec::with_capacity(texts.len());
        for text in texts {
            let payload: Value = serde_json::from_str(&text)?;
            let url_id = payload
                .get("url_id")
                .and_then(Value::as_i64)
                .ok_or_else(|| scan_err("sitemap member has no URL id"))?;
            let ordinal = payload
                .get("ordinal")
                .and_then(Value::as_i64)
                .ok_or_else(|| scan_err("sitemap member has no ordinal"))?;
            let url: String =
                con.query_row("SELECT url FROM urls WHERE url_id=?", [url_id], |r| {
                    r.get(0)
                })?;
            members.push((ordinal, url));
        }
        Ok(members)
    }

    /// Append one bounded chunk of `[ordinal, url]` pairs to a sitemap's membership.
    pub fn write_sitemap_members(&self, sid: i64, entries: &[Value]) -> Result<(), StoreError> {
        self.assert_mutable()?;
        let entries = bounded_items(entries, "sitemap membership chunk", 256)?;
        let roots = sitemaps::root_ids(self.con())?;
        self.transact(|con| {
            let mut next_ordinal: i64 = con.query_row(
                "SELECT COUNT(*) FROM context_items WHERE kind='sitemap_declared_url'",
                [],
                |r| r.get(0),
            )?;
            for entry in &entries {
                let pair = entry.as_array().filter(|p| p.len() == 2);
                let (Some(ordinal), Some(url)) = (
                    pair.and_then(|p| p[0].as_i64()).filter(|n| *n >= 0),
                    pair.and_then(|p| p[1].as_str()).filter(|u| !u.is_empty()),
                ) else {
                    return Err(scan_err("sitemap member requires a source ordinal and URL"));
                };
                let key = format!("sitemap:{sid}:ordinal:{ordinal}");
                let exists = con
                    .query_row(
                        "SELECT 1 FROM context_items WHERE kind='sitemap_declared_url' AND item_key=?",
                        [&key],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some();
                if !exists {
                    if ordinal != next_ordinal {
                        return Err(scan_err(
                            "sitemap member order changed during capture/resume",
                        ));
                    }
                    next_ordinal += 1;
                }
                let payload = json!({
                    "sitemap_url_id": sid,
                    "url_id": intern_url(con, url)?,
                    "ordinal": ordinal,
                });
                let item = sitemaps::context(
                    "sitemap_declared_url",
                    &key,
                    &payload,
                    true,
                    "selected sitemap expansion membership",
                );
                put_context(con, &item, Some(&roots))?;
            }
            Ok(())
        })
    }

    pub fn finish_sitemap(&self, sid: i64, complete: bool, reason: &str) -> Result<(), StoreError> {
        self.assert_mutable()?;
        json_chunks(
            &json!({"sitemap_url_id": sid, "complete": complete, "reason": reason}),
            MAX_RECORD_BYTES,
        )?;
        self.transact(|con| sitemaps::finish(con, sid, complete, reason))
    }

    /// Declare the delivered collector and reset only recoverable interruption state.
    /// `links_available` defaults to true.
    pub fn begin_collection(&self, links_available: bool) -> Result<(), StoreError> {
        self.assert_mutable()?;
        self.transact(|con| {
            let (limitations_json, capabilities_json): (String, String) = con.query_row(
                "SELECT limitations_json,capabilities_json FROM scan WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let limitations: Vec<String> = serde_json::from_str(&limitations_json)?;
            let mut partial = limitations.iter().any(|reason| {
                matches!(
                    reason.split(':').next().unwrap_or(""),
                    "link_observations_omitted"
                        | "form_observations_omitted"
                        | "response_body_unavailable"
                        | "resource_declarations_omitted"
                )
            });
            let unfinished: i64 = con.query_row(
                "SELECT (SELECT COUNT(*) FROM context_items WHERE kind='sitemap_declaration') \
                 != (SELECT COUNT(*) FROM context_items WHERE kind='sitemap_fetch_summary' \
                 AND completeness='complete')",
                [],
                |r| r.get(0),
            )?;
            partial = partial || unfinished != 0;
            let mut capabilities: Map<String, Value> = serde_json::from_str(&capabilities_json)?;
            for kind in ["pages", "links"] {
                capabilities.insert(
                    kind.to_string(),
                    json!({
                        "state": if partial { "partial" } else { "complete" },
                        "reason": if partial {
                            "native observation prefix omitted"
                        } else {
                            "committed native crawl observations"
                        },
                    }),
                );
            }
            if !links_available {
                capabilities.insert(
                    "links".to_string(),
                    json!({
                        "state": "unavailable",
                        "reason": "explicit URL-list collection does not retain link observations",
                    }),
                );
            }
            capabilities.insert(
                "resume".to_string(),
                json!({
                    "state": "complete",
                    "reason": "native frontier and runtime state retained",
                }),
            );
            con.execute(
                "UPDATE scan SET lifecycle='running',finish_reason='running',finished_at=NULL,\
                 crawl_partial=?,capabilities_json=? WHERE singleton=1",
                rusqlite::params![i64::from(partial), dump(&Value::Object(capabilities))],
            )?;
            Ok(())
        })
    }

    /// Add one deduplicated note to the bounded limitation registry.
    pub fn note(&self, note: &str) -> Result<(), StoreError> {
        let con = self.con();
        let mut notes: Vec<Value> = serde_json::from_str(&con.query_row(
            "SELECT limitations_json FROM scan WHERE singleton=1",
            [],
            |r| r.get::<_, String>(0),
        )?)?;
        if !notes.iter().any(|n| n.as_str() == Some(note)) {
            if notes.len() >= 64 {
                return Err(scan_err("native scan limitation registry is full"));
            }
            notes.push(Value::String(note.to_string()));
        }
        con.execute(
            "UPDATE scan SET limitations_json=? WHERE singleton=1",
            [dump(&Value::Array(notes))],
        )?;
        Ok(())
    }

    /// Keep a missing analyzer result distinct from incomplete collection.
    pub fn note_audit_unavailable(&self, reason: &str) -> Result<(), StoreError> {
        self.assert_mutable()?;
        if reason.is_empty() || reason.chars().count() > 500 {
            return Err(scan_err(
                "audit availability reason must be a short nonempty string",
            ));
        }
        self.transact(|_| self.note(&format!("audit unavailable: {reason}")))
    }

    /// Validate and store context items (JSON objects) in one transaction.
    pub fn write_context(&self, items: &[Value]) -> Result<(), StoreError> {
        self.assert_mutable()?;
        let items = bounded_items(items, "context", MAX_PAGE_COMMIT_ITEMS)?;
        self.transact(|con| {
            for item in &items {
                let Value::Object(item) = item else {
                    return Err(scan_err("context item must be an object"));
                };
                put_context(con, item, None)?;
            }
            Ok(())
        })
    }

    /// Split top-level audit collections without copying their rows.
    ///
    /// The analyzer still owns its result objects.  This writer only avoids a
    /// second complete JSON document when the legacy audit slot is too small:
    /// every named collection is consumed directly by ``audit.v2`` in its
    /// original order. Lists nested in a collection item stay with that item,
    /// which preserves its atomic JSON row boundary.
    ///
    /// Collection rows are cloned here, so the header keeps an empty list in place of each
    /// collection, keyed in `collections` by JSON pointer.
    pub fn audit_v2_parts(document: &Map<String, Value>) -> Result<AuditParts, StoreError> {
        fn pointer(path: &str, key: &str) -> String {
            format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"))
        }
        fn split(
            value: &Map<String, Value>,
            path: &str,
            collections: &mut IndexMap<String, Vec<Value>>,
        ) -> Map<String, Value> {
            let mut header = Map::new();
            for (key, child) in value {
                let child_path = pointer(path, key);
                match child {
                    Value::Array(rows) => {
                        header.insert(key.clone(), Value::Array(Vec::new()));
                        collections.insert(child_path, rows.clone());
                    }
                    Value::Object(inner) => {
                        header.insert(
                            key.clone(),
                            Value::Object(split(inner, &child_path, collections)),
                        );
                    }
                    other => {
                        header.insert(key.clone(), other.clone());
                    }
                }
            }
            header
        }
        let mut collections = IndexMap::new();
        let header = split(document, "", &mut collections);
        if collections.is_empty() {
            return Err(scan_err(
                "audit.v2 needs at least one ordered audit collection",
            ));
        }
        Ok((header, collections))
    }

    /// The binding a companion must carry: scan identity, evidence revision and writer.
    pub fn audit_binding(&self) -> Result<Map<String, Value>, StoreError> {
        let scan = scan_row(self.con())?;
        let pick = |column: &str| scan.get(column).cloned().unwrap_or(Value::Null);
        Ok(Map::from_iter([
            ("scan_uuid".to_string(), pick("scan_uuid")),
            ("evidence_revision".to_string(), pick("evidence_revision")),
            ("analyzer_version".to_string(), pick("writer_version")),
            ("analyzer_revision".to_string(), pick("writer_revision")),
        ]))
    }

    /// Publish a validated audit.v2 companion and retire the legacy slot.
    pub fn save_audit_v2_document(&self, document: &Map<String, Value>) -> Result<(), StoreError> {
        let (header, collections) = Self::audit_v2_parts(document)?;
        self.save_audit_v2(&header, &collections)
    }

    /// Publish ordered audit collections without building a legacy JSON document.
    pub fn save_audit_v2(
        &self,
        header: &Map<String, Value>,
        collections: &IndexMap<String, Vec<Value>>,
    ) -> Result<(), StoreError> {
        self.assert_mutable()?;
        write_audit_v2(&self.path, header, collections, &self.audit_binding()?)?;
        // Validate the newly published companion before making it the only current audit
        // reference in the scan database.
        drop(AuditV2Reader::new(&self.path, true, false, None)?);
        self.transact(|con| {
            con.execute("DELETE FROM audit", [])?;
            Ok(())
        })
    }

    pub fn save_audit(&self, document: &Map<String, Value>) -> Result<(), StoreError> {
        self.assert_mutable()?;
        // Check the exact serialized shape before allocating another complete audit string or
        // opening the replacing transaction. Never truncate it.
        let mut counter = LimitedCounter {
            bytes: 0,
            limit: MAX_JSON_BYTES as usize,
        };
        if serde_json::to_writer_pretty(&mut counter, document).is_err() {
            // Validate before publishing the companion. ``audit.v2`` is selected before
            // constructing a second whole-document JSON string, so this branch does not turn a
            // completed collection into an unavailable audit solely because the legacy slot is
            // finite.
            validate_audit_document(&Value::Object(document.clone()))?;
            return self.save_audit_v2_document(document);
        }
        let raw = serde_json::to_string_pretty(document)?;
        parse_audit(&raw)?;
        let schema_version = document
            .get("schema_version")
            .cloned()
            .ok_or_else(|| scan_err("audit document has no schema_version"))?;
        self.transact(|con| {
            let scan = scan_row(con)?;
            let pick = |column: &str| scan.get(column).cloned().unwrap_or(Value::Null);
            let mut row = Map::new();
            row.insert("singleton".to_string(), json!(1));
            row.insert("schema_version".to_string(), schema_version);
            row.insert("analyzer_version".to_string(), pick("writer_version"));
            row.insert("analyzer_revision".to_string(), pick("writer_revision"));
            row.insert("evidence_revision".to_string(), pick("evidence_revision"));
            row.insert("document_json".to_string(), Value::String(raw.clone()));
            row.insert("sha256".to_string(), Value::String(sha(&raw)));
            row.insert("created_at".to_string(), Value::String(utc()?));
            insert(con, "audit", &row)?;
            validate_audit(con, &scan, true)?;
            Ok(())
        })
    }
}

/// Counts serialized bytes and fails once `limit` is exceeded.
struct LimitedCounter {
    bytes: usize,
    limit: usize,
}

impl std::io::Write for LimitedCounter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes += buf.len();
        if self.bytes > self.limit {
            return Err(std::io::Error::other(
                "audit document exceeds the legacy slot",
            ));
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Count admitted HTML inputs in SQL without reading body BLOBs or page lists.
pub fn reanalysis_capability(con: &Connection) -> Result<BTreeMap<String, String>, StoreError> {
    let config = json_column(con, "SELECT config_json FROM scan WHERE singleton=1")?;
    let parse_limit = config
        .pointer("/limits/max_response_bytes")
        .and_then(Value::as_i64)
        .ok_or_else(|| scan_err("scan config has no limits.max_response_bytes"))?;
    let (required, usable): (i64, i64) = con.query_row(
        "WITH active_static AS (
            SELECT d.url_id,MAX(d.document_id) AS document_id FROM documents d
            JOIN context_items c ON c.kind='resource_inventory' AND c.item_key='document:'||d.document_id
            WHERE d.representation='static' GROUP BY d.url_id
        )
        SELECT COUNT(*),COALESCE(SUM(CASE WHEN
            selected.body_state='complete' AND selected.body_sha256 IS NOT NULL
            AND raw.body_state='complete' AND raw.body_sha256 IS NOT NULL
            AND raw_body.decoded_bytes<=? AND selected_body.decoded_bytes<=?
            THEN 1 ELSE 0 END),0)
        FROM pages p
        LEFT JOIN documents selected ON selected.document_id=p.document_id
        LEFT JOIN active_static a ON a.url_id=p.url_id
        LEFT JOIN documents raw ON raw.document_id=CASE WHEN p.representation='static'
            THEN p.document_id ELSE a.document_id END
        LEFT JOIN bodies raw_body ON raw_body.sha256=raw.body_sha256
        LEFT JOIN bodies selected_body ON selected_body.sha256=selected.body_sha256
        WHERE lower(p.content_type) LIKE '%html%' AND p.status_code IS NOT NULL",
        [parse_limit, parse_limit],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let state = |state: &str, reason: &str| {
        BTreeMap::from([
            ("state".to_string(), state.to_string()),
            ("reason".to_string(), reason.to_string()),
        ])
    };
    Ok(if usable != 0 && usable == required {
        state("complete", "")
    } else if usable != 0 {
        state(
            "partial",
            "some HTML documents lack retained raw or selected inputs",
        )
    } else {
        state("unavailable", "no retained raw HTML inputs")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SITES: &[&str] = &[
        "basic",
        "canonical-hreflang",
        "errors",
        "redirects",
        "robots-sitemap",
        "schema",
    ];

    fn oracle_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../oracle")
    }

    /// Writable copy of a recorded raw scan in a unique scratch directory.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "spiderhead-native-scan-{}-{name}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch dir");
            Scratch(dir)
        }
        fn scan_of(&self, site: &str) -> PathBuf {
            let target = self.0.join(format!("{site}.sqlite"));
            std::fs::copy(
                oracle_root()
                    .join("raw-scans")
                    .join(site)
                    .join("scan.sqlite"),
                &target,
            )
            .expect("copy raw scan");
            target
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn open(path: &Path) -> NativeScan {
        let con = Connection::open(path).expect("open scan");
        con.busy_timeout(Duration::from_secs(5)).unwrap();
        NativeScan::new(path, con, None).expect("native scan")
    }

    fn baseline_source(site: &str) -> Map<String, Value> {
        let text = std::fs::read_to_string(
            oracle_root()
                .join("baselines")
                .join(site)
                .join("scan-status.json"),
        )
        .expect("baseline");
        let document: Value = serde_json::from_str(&text).unwrap();
        document["stdout"]["source"]
            .as_object()
            .expect("source")
            .clone()
    }

    // Check: open each recorded raw scan and match its `scan-status` source summary.
    #[test]
    fn oracle_open_matches_status_baseline_for_every_site() {
        let scratch = Scratch::new("oracle");
        for site in SITES {
            let scan = open(&scratch.scan_of(site));
            let header = scan_row(scan.con()).unwrap();
            let mut compared = 0;
            for (key, expected) in baseline_source(site) {
                if matches!(&expected, Value::String(s) if s.starts_with('<')) {
                    continue; // normalised timestamp / uuid placeholder
                }
                let actual = match header.get(&key) {
                    Some(Value::Number(n))
                        if matches!(key.as_str(), "corpus_partial" | "crawl_partial") =>
                    {
                        json!(n.as_i64() != Some(0))
                    }
                    Some(other) => other.clone(),
                    None => panic!("{site}: scan header has no {key}"),
                };
                assert_eq!(actual, expected, "{site}: {key}");
                compared += 1;
            }
            assert!(compared >= 8, "{site}: compared only {compared} fields");
            assert!(!scan.v2_pages && !scan.page_columns.is_empty());
        }
    }

    // Check: the SQL-only reanalysis capability equals what the writer stored.
    #[test]
    fn reanalysis_capability_matches_stored_capability() {
        let scratch = Scratch::new("reanalysis");
        for site in SITES {
            let scan = open(&scratch.scan_of(site));
            let header = scan_row(scan.con()).unwrap();
            let capabilities: Value =
                serde_json::from_str(header["capabilities_json"].as_str().unwrap()).unwrap();
            let computed = reanalysis_capability(scan.con()).unwrap();
            assert_eq!(
                json!(computed),
                capabilities["offline_reanalysis"],
                "{site}"
            );
        }
    }

    #[test]
    fn audit_binding_names_the_header_identity() {
        let scratch = Scratch::new("binding");
        let scan = open(&scratch.scan_of("basic"));
        let binding = scan.audit_binding().unwrap();
        let source = baseline_source("basic");
        assert_eq!(binding["evidence_revision"], source["evidence_revision"]);
        assert_eq!(binding["analyzer_version"], source["writer_version"]);
        assert_eq!(binding["analyzer_revision"], source["writer_revision"]);
        assert!(binding["scan_uuid"].is_string());
    }

    #[test]
    fn finished_scan_is_immutable() {
        let scratch = Scratch::new("immutable");
        let scan = open(&scratch.scan_of("basic"));
        let error = scan.assert_mutable().unwrap_err();
        assert!(
            error.to_string().contains("'finished' is immutable"),
            "{error}"
        );
        assert!(scan.begin_collection(true).is_err());
        assert!(scan.note_audit_unavailable("x").is_err());
    }

    #[test]
    fn begin_collection_reopens_a_scan_and_declares_capabilities() {
        let scratch = Scratch::new("collection");
        let path = scratch.scan_of("basic");
        let scan = open(&path);
        scan.con()
            .execute("UPDATE scan SET lifecycle='running'", [])
            .unwrap();
        scan.begin_collection(false).unwrap();
        let (lifecycle, reason, partial, caps): (String, String, i64, String) = scan
            .con()
            .query_row(
                "SELECT lifecycle,finish_reason,crawl_partial,capabilities_json FROM scan",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            (lifecycle.as_str(), reason.as_str(), partial),
            ("running", "running", 0)
        );
        let caps: Value = serde_json::from_str(&caps).unwrap();
        assert_eq!(caps["links"]["state"], "unavailable");
        assert_eq!(caps["pages"]["state"], "complete");
        assert_eq!(caps["resume"]["state"], "complete");
        assert!(scan.con().is_autocommit());
    }

    // The limitation registry is deduplicated and bounded.
    #[test]
    fn notes_are_deduplicated_and_capped() {
        let scratch = Scratch::new("notes");
        let scan = open(&scratch.scan_of("basic"));
        scan.con()
            .execute("UPDATE scan SET lifecycle='running'", [])
            .unwrap();
        scan.con()
            .execute("UPDATE scan SET limitations_json='[]'", [])
            .unwrap();
        scan.note_audit_unavailable("analyzer failed").unwrap();
        scan.note_audit_unavailable("analyzer failed").unwrap();
        let notes: String = scan
            .con()
            .query_row("SELECT limitations_json FROM scan", [], |r| r.get(0))
            .unwrap();
        assert_eq!(notes, r#"["audit unavailable: analyzer failed"]"#);
        for i in 0..63 {
            scan.note_audit_unavailable(&format!("n{i}")).unwrap();
        }
        let error = scan.note_audit_unavailable("one too many").unwrap_err();
        assert!(error.to_string().contains("registry is full"), "{error}");
        assert!(scan.con().is_autocommit(), "failed write must roll back");
        assert!(scan.note_audit_unavailable("").is_err());
        assert!(scan.note_audit_unavailable(&"x".repeat(501)).is_err());
    }

    #[test]
    fn failed_transaction_rolls_back() {
        let scratch = Scratch::new("rollback");
        let scan = open(&scratch.scan_of("basic"));
        let before: String = scan
            .con()
            .query_row("SELECT limitations_json FROM scan", [], |r| r.get(0))
            .unwrap();
        let result: Result<(), StoreError> = scan.transact(|con| {
            con.execute("UPDATE scan SET limitations_json='[]'", [])?;
            Err(scan_err("boom"))
        });
        assert!(result.is_err());
        assert!(scan.con().is_autocommit());
        let after: String = scan
            .con()
            .query_row("SELECT limitations_json FROM scan", [], |r| r.get(0))
            .unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn read_context_returns_payload_or_none() {
        let scratch = Scratch::new("context");
        let scan = open(&scratch.scan_of("robots-sitemap"));
        assert_eq!(scan.read_context("no_such_kind", "run").unwrap(), None);
        let (kind, key, payload): (String, String, String) = scan
            .con()
            .query_row(
                "SELECT kind,item_key,payload_json FROM context_items LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            scan.read_context(&kind, &key).unwrap(),
            Some(serde_json::from_str::<Value>(&payload).unwrap())
        );
    }

    // v1 external checks remain unavailable.
    #[test]
    fn v1_external_checks_remain_unavailable() {
        let scratch = Scratch::new("external");
        let scan = open(&scratch.scan_of("basic"));
        scan.con()
            .execute("UPDATE scan SET lifecycle='running'", [])
            .unwrap();
        assert!(scan.external_checks().unwrap().is_empty());
        let error = scan.record_external_check(0, &Map::new()).unwrap_err();
        assert!(error.to_string().contains("require scan.v2"), "{error}");
        assert!(scan.record_external_checks_summary(&Map::new()).is_err());
    }

    // Audit v2 parts keep nested sitemap rows re-iterable.
    #[test]
    fn audit_v2_parts_keep_nested_rows_under_pointer_keys() {
        let Value::Object(document) = json!({
            "issues": [{"id": "ISSUE-000001"}],
            "pages": [{"url": "https://example.test/"}],
            "groups": [],
            "summary": {"sitemap": {"linked_not_in_sitemap": ["https://example.test/orphan"]}},
            "odd/key~": [1],
        }) else {
            unreachable!()
        };
        let (header, collections) = NativeScan::audit_v2_parts(&document).unwrap();
        assert_eq!(header["issues"], json!([]));
        assert_eq!(header["pages"], json!([]));
        assert_eq!(header["groups"], json!([]));
        assert_eq!(
            header["summary"]["sitemap"]["linked_not_in_sitemap"],
            json!([])
        );
        assert_eq!(
            collections["/summary/sitemap/linked_not_in_sitemap"],
            vec![json!("https://example.test/orphan")]
        );
        assert_eq!(collections["/odd~1key~0"], vec![json!(1)]);
        assert_eq!(
            collections.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "/issues",
                "/pages",
                "/groups",
                "/summary/sitemap/linked_not_in_sitemap",
                "/odd~1key~0"
            ],
            "original order is kept"
        );
    }

    #[test]
    fn audit_v2_parts_need_a_collection() {
        let Value::Object(document) = json!({"summary": {"n": 1}}) else {
            unreachable!()
        };
        assert!(NativeScan::audit_v2_parts(&document).is_err());
    }

    #[test]
    fn small_wal_is_not_waited_for() {
        let scratch = Scratch::new("wal-small");
        let scan = open(&scratch.scan_of("basic"));
        assert!(!scan.enforce_wal_bound().unwrap());
    }

    // WAL backpressure: a reader holding a snapshot keeps TRUNCATE busy past the deadline
    #[test]
    fn blocked_wal_checkpoint_is_backpressure() {
        let scratch = Scratch::new("wal-busy");
        let path = scratch.scan_of("basic");
        let mut scan = open(&path);
        scan.con().execute_batch("PRAGMA journal_mode=WAL").unwrap();
        scan.con()
            .execute_batch("UPDATE scan SET lifecycle='running'")
            .unwrap();
        scan.wal_backpressure_bytes = 1;
        scan.wal_checkpoint_wait_seconds = 0.05;
        let reader = Connection::open(&path).unwrap();
        reader
            .execute_batch("BEGIN; SELECT COUNT(*) FROM scan;")
            .unwrap();
        scan.con()
            .execute_batch("UPDATE scan SET limitations_json=limitations_json")
            .unwrap();
        let error = scan.enforce_wal_bound().unwrap_err();
        assert!(
            matches!(error, StoreError::ScanBackpressure(_)),
            "{error:?}"
        );
        assert!(scan.begin(true).is_err());
        reader.execute_batch("COMMIT").unwrap();
        // reader gone: the checkpoint truncates the log and the writer proceeds
        assert!(!scan.enforce_wal_bound().unwrap());
        scan.begin(true).unwrap();
        scan.rollback();
        assert!(scan.con().is_autocommit());
    }

    #[test]
    fn close_is_idempotent_and_releases_the_connection() {
        let scratch = Scratch::new("close");
        let mut scan = open(&scratch.scan_of("basic"));
        scan.close();
        scan.close();
        assert!(scan.con.is_none());
        scan.rollback();
    }

    #[test]
    fn failpoint_fires_by_name() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let scratch = Scratch::new("failpoint");
        let mut scan = open(&scratch.scan_of("basic"));
        let seen = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&seen);
        scan.failpoint = Some(Box::new(move |name| {
            sink.borrow_mut().push(name.to_string())
        }));
        scan.hit("before_commit");
        assert_eq!(*seen.borrow(), vec!["before_commit".to_string()]);
    }

    fn v2_scan(scratch: &Scratch) -> NativeScan {
        let path = scratch.scan_of("basic");
        let con = Connection::open(&path).unwrap();
        con.execute_batch("PRAGMA user_version=2").unwrap();
        con.execute_batch("UPDATE scan SET lifecycle='running'")
            .unwrap();
        drop(con);
        open(&path)
    }

    #[test]
    fn v1_scan_has_no_event_timeline() {
        let scratch = Scratch::new("events-v1");
        let scan = open(&scratch.scan_of("basic"));
        scan.event("budget", &Map::new(), None).unwrap();
        scan.event_coverage().unwrap();
        let tables: i64 = scan
            .con()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name='scan_events'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0);
    }

    #[test]
    fn v2_events_are_sequenced_capped_and_resumed() {
        let scratch = Scratch::new("events-v2");
        let mut scan = v2_scan(&scratch);
        let Value::Object(payload) = json!({"kind": "requests", "limit": 10, "used": 1}) else {
            unreachable!()
        };
        scan.event("budget", &payload, Some(&json!("2026-10-27T10:00:00Z")))
            .unwrap();
        scan.event("budget", &payload, None).unwrap();
        scan.event_coverage().unwrap();
        let (count, max): (i64, i64) = scan
            .con()
            .query_row("SELECT COUNT(*),MAX(sequence) FROM scan_events", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((count, max), (2, 2));
        let meta: (i64, i64, i64) = scan
            .con()
            .query_row(
                "SELECT cap,captured,dropped FROM scan_event_meta",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(meta, (MAX_EVENTS, 2, 0));
        // reopening resumes the sequence from the saved timeline
        let path = scan.path().to_path_buf();
        scan.close();
        let again = open(&path);
        again.event("budget", &payload, None).unwrap();
        let max: i64 = again
            .con()
            .query_row("SELECT MAX(sequence) FROM scan_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(max, 3);
        // a full timeline drops and says so
        again
            .events
            .set(again.events.get().map(|s| EventState { cap: 3, ..s }));
        again.event("budget", &payload, None).unwrap();
        again.event_coverage().unwrap();
        let meta: (i64, i64, i64) = again
            .con()
            .query_row(
                "SELECT cap,captured,dropped FROM scan_event_meta",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(meta, (3, 3, 1));
    }

    #[test]
    fn inconsistent_saved_timeline_coverage_is_refused() {
        let scratch = Scratch::new("events-bad");
        let scan = v2_scan(&scratch);
        let path = scan.path().to_path_buf();
        scan.con()
            .execute("INSERT INTO scan_event_meta VALUES(1,100,5,0)", [])
            .unwrap();
        drop(scan);
        let con = Connection::open(&path).unwrap();
        let error = NativeScan::new(&path, con, None).err().expect("refused");
        assert!(
            error
                .to_string()
                .contains("event timeline coverage is invalid"),
            "{error}"
        );
    }

    #[test]
    fn record_events_persists_adapter_events_atomically() {
        let scratch = Scratch::new("record-events");
        let mut scan = v2_scan(&scratch);
        let mut sink = EventSink::new(10, None).unwrap();
        let payload: IndexMap<String, Value> = [
            ("kind".to_string(), json!("requests")),
            ("limit".to_string(), json!(10)),
            ("used".to_string(), json!(2)),
        ]
        .into_iter()
        .collect();
        sink.emit("budget", &payload, Some(&json!("2026-10-27T10:00:00Z")))
            .unwrap();
        sink.emit("budget", &payload, None).unwrap();
        scan.record_events(&sink).unwrap();
        let count: i64 = scan
            .con()
            .query_row("SELECT COUNT(*) FROM scan_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);
        assert!(scan.con().is_autocommit());
    }

    #[test]
    fn execute_does_not_leave_write_transaction_open_after_begin_error() {
        let scratch = Scratch::new("begin-error");
        let scan = open(&scratch.scan_of("basic"));
        scan.con().execute_batch("DROP TABLE audit").unwrap();
        assert!(scan.begin(false).is_err());
        scan.rollback();
        assert!(scan.con().is_autocommit());
    }
}

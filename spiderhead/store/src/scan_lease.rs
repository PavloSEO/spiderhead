//! Lease, frontier, commit and finalization lanes of the native scan writer.
//!
//! Also home to the small serialization primitives every writer layer shares (bounded JSON
//! chunking, digests, fsync helpers, the Lease structs).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::discovery_ledger::query_maps;
use crate::error::StoreError;
use crate::scan_artifact::{config as validate_config, dump, insert, ScanError, MAX_RECORD_BYTES};

/// Page columns stored as JSON text, and the record field each comes from.
pub const PAGE_JSON_SOURCES: &[(&str, &str)] = &[
    ("redirect_chain_json", "redirect_chain"),
    ("hreflang_json", "hreflang"),
    ("heading_outline_json", "heading_outline"),
    ("invalid_link_targets_json", "invalid_link_targets"),
    ("link_placement_json", "link_placement"),
    ("trust_signals_json", "trust_signals"),
    ("duplicate_ids_json", "duplicate_ids"),
    ("canonical_chain_json", "canonical_chain"),
];

/// Page fields a record may legitimately lack (NULL is their recorded state).
pub const OPTIONAL_PAGE_SOURCES: &[&str] = &[
    "avg_words_per_sentence",
    "canonical_outside_head",
    "directives_outside_head",
    "duplicate_ids",
    "flesch",
    "hreflang_outside_head",
    "invalid_link_targets",
    "meta_description_outside_head",
    "readability",
    "response_time",
    "sentence_count",
    "status_code",
    "text_ratio",
    "title_outside_head",
    "trust_signals",
];

/// Largest number of edges one page commit may carry.
pub const MAX_EDGES_PER_PAGE: i64 = 20_000;
/// Largest number of items in one bounded per-page list.
pub const MAX_PAGE_COMMIT_ITEMS: i64 = 20_000;
/// Free space a snapshot must leave untouched.
pub const SNAPSHOT_RESERVE_BYTES: i64 = 1_073_741_824;
/// Wall-clock budget of a snapshot backup.
pub const BACKUP_TIMEOUT_SECONDS: f64 = 60.0;
/// Wall-clock budget of finalization.
pub const FINALIZATION_TIMEOUT_SECONDS: f64 = 10.0;

/// One frontier URL claimed for fetching.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Lease {
    /// Row id of the URL.
    pub url_id: i64,
    /// The URL.
    pub url: String,
    /// Crawl depth.
    pub depth: i64,
    /// Position in the frontier queue.
    pub queue_ordinal: i64,
}

/// Result of committing one page.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CommitReceipt {
    /// Evidence revision after the commit.
    pub evidence_revision: i64,
    /// True when the page had been committed before.
    pub already_committed: bool,
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// The current UTC time as ISO-8601 with a `Z` suffix and no fraction when it is zero.
pub fn utc() -> Result<String, StoreError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| StoreError::Invalid(format!("clock before the epoch: {e}")))?;
    Ok(utc_from(elapsed.as_secs() as i64, elapsed.subsec_micros()))
}

/// Format `seconds` since the epoch (plus microseconds) like `datetime.isoformat()` with `Z`.
pub fn utc_from(seconds: i64, micros: u32) -> String {
    let (year, month, day) = civil_from_days(seconds.div_euclid(86_400));
    let rem = seconds.rem_euclid(86_400);
    let base = format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    );
    if micros == 0 {
        format!("{base}Z")
    } else {
        format!("{base}.{micros:06}Z")
    }
}

fn scan_error(message: &str) -> StoreError {
    ScanError {
        message: message.to_string(),
    }
    .into()
}

/// Bound scalars and nesting, then encode `value` as compact sorted-key JSON.
///
/// The walk runs before any escaped copy is allocated; the encoded size is bounded by `limit`.
/// The encoder's parts are concatenated into one chunk.
/// `limit` defaults to `MAX_RECORD_BYTES`.
pub fn json_chunks(value: &Value, limit: i64) -> Result<Vec<Vec<u8>>, StoreError> {
    let mut stack: Vec<std::vec::IntoIter<&Value>> = vec![vec![value].into_iter()];
    let mut raw_size: i64 = 0;
    while let Some(top) = stack.last_mut() {
        let Some(item) = top.next() else {
            stack.pop();
            continue;
        };
        raw_size += 1;
        match item {
            Value::String(text) => {
                if text.chars().count() as i64 > limit - raw_size {
                    return Err(scan_error("native JSON payload exceeds its byte limit"));
                }
                raw_size += text.len() as i64;
            }
            Value::Object(map) => {
                let mut children: Vec<&Value> = Vec::with_capacity(map.len() * 2);
                // Keys are strings, so they are walked as string items.
                for key in map.keys() {
                    raw_size += 1;
                    if key.chars().count() as i64 > limit - raw_size {
                        return Err(scan_error("native JSON payload exceeds its byte limit"));
                    }
                    raw_size += key.len() as i64;
                }
                children.extend(map.values());
                stack.push(children.into_iter());
            }
            Value::Array(items) => stack.push(items.iter().collect::<Vec<_>>().into_iter()),
            _ => {}
        }
        if raw_size > limit || stack.len() > 64 {
            return Err(scan_error(
                "native JSON payload exceeds its byte or nesting limit",
            ));
        }
    }
    let encoded = spiderhead_core::common::canonical_json(value).into_bytes();
    if encoded.len() as i64 > limit {
        return Err(scan_error("native JSON payload exceeds its byte limit"));
    }
    Ok(vec![encoded])
}

/// SHA-256 of the bounded canonical JSON of `value`.
pub fn digest(value: &Value) -> Result<String, StoreError> {
    let mut hasher = Sha256::new();
    for chunk in json_chunks(value, 8 * MAX_RECORD_BYTES)? {
        hasher.update(chunk);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Collect at most `limit` values whose encoded size stays within one record.
/// `limit` defaults to `MAX_PAGE_COMMIT_ITEMS`.
pub fn bounded_items(values: &[Value], label: &str, limit: i64) -> Result<Vec<Value>, StoreError> {
    let mut items = Vec::new();
    let mut size: i64 = 0;
    for value in values.iter().take(limit as usize + 1) {
        size += json_chunks(value, MAX_RECORD_BYTES)?
            .iter()
            .map(|c| c.len() as i64)
            .sum::<i64>();
        if items.len() as i64 == limit || size > MAX_RECORD_BYTES {
            return Err(scan_error(&format!(
                "{label} exceeds the bounded per-page limit ({limit} items / 8 MiB)"
            )));
        }
        items.push(value.clone());
    }
    Ok(items)
}

/// Flush a file's data to disk.
pub fn fsync_file(path: &Path) -> Result<(), StoreError> {
    Ok(std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?
        .sync_all()?)
}

/// Flush directory entries (POSIX).
pub fn fsync_directory(directory: &Path) -> Result<(), StoreError> {
    Ok(std::fs::File::open(directory)?.sync_all()?)
}

/// Everything one page commit folds back in a single transaction.
#[derive(Debug, Clone, Default)]
pub struct PageCommit {
    /// The claimed lease being committed.
    pub lease: Option<Lease>,
    /// The page record.
    pub record: Map<String, Value>,
    /// Outgoing links.
    pub links: Vec<Map<String, Value>>,
    /// Forms found on the page.
    pub forms: Vec<Map<String, Value>>,
    /// Scope decisions.
    pub decisions: Vec<Map<String, Value>>,
    /// Newly discovered `(url, depth)` pairs.
    pub discovered: Vec<(String, i64)>,
    /// Reserved query variants `(path, query_key, url)`.
    pub query_reservations: Vec<(String, String, String)>,
    /// Frontier candidates.
    pub candidates: Vec<Map<String, Value>>,
    /// Partial-capture reasons.
    pub partial_reasons: Vec<String>,
    /// Runtime state to store.
    pub runtime: Option<Map<String, Value>>,
    /// Typed context items.
    pub context: Vec<Map<String, Value>>,
    /// Content capture block.
    pub content_capture: Option<Map<String, Value>>,
    /// Rendered route observations.
    pub route_observations: Vec<Map<String, Value>>,
    /// Rendered route coverage.
    pub route_coverage: Option<Map<String, Value>>,
    /// Captured responses.
    pub captures: Vec<crate::crawl_capture::CaptureEvent>,
    /// Resource graph rows.
    pub resources: Vec<Map<String, Value>>,
    /// Resource inventory state.
    pub resource_inventory_state: Option<String>,
    /// Resources omitted by the per-page bound.
    pub resources_omitted: i64,
}

/// What the lease lanes need from the writer that owns them (`NativeScan`): its connection and
/// the transaction and event primitives that stay on the
/// writer object.
pub trait ScanWriterHost {
    /// The writer's SQLite connection.
    fn connection(&self) -> &Connection;
    /// Refuse a finished or failed scan.
    fn assert_mutable(&self) -> Result<(), StoreError>;
    /// Start a writer transaction (`BEGIN IMMEDIATE`, WAL bound, audit reset).
    fn begin(&self, enforce_wal_bound: bool) -> Result<(), StoreError>;
    /// Roll back the open transaction, if any.
    fn rollback(&self);
    /// Fire a named failpoint (tests only).
    fn hit(&self, name: &str);
    /// Record one adapter event, when an event sink is attached.
    fn event(&self, event_type: &str, payload: Value) -> Result<(), StoreError>;
    /// Persist the event sink's coverage, when a sink is attached.
    fn event_coverage(&self) -> Result<(), StoreError>;
}

fn json_item(row: &Map<String, Value>, key: &str) -> Value {
    row.get(key).cloned().unwrap_or(Value::Null)
}

fn is_count(value: &Value) -> bool {
    value.is_i64() && value.as_i64().is_some_and(|n| n >= 0)
}

fn finite_nonnegative(value: &Value) -> bool {
    value.is_number() && value.as_f64().is_some_and(|f| f.is_finite() && f >= 0.0)
}

fn put_context(con: &Connection, item: &Map<String, Value>) -> Result<(), StoreError> {
    crate::native_context::put_context(con, item, None)
}

/// Leases, frontier flow, runtime state and the first finalization steps of a native scan.
///
/// A trait with default methods over [`ScanWriterHost`], which the
/// writer implements by delegating to its own transaction primitives. Page commit
/// (`page_row`, `commit_page`) needs the writer's page-column state and corpus lanes, so it
/// lives on `NativeScan` itself (`scan_commit`).
pub trait NativeScanLease: ScanWriterHost {
    /// Run `body` in a writer transaction: commit on success, roll back on any failure.
    fn in_transaction<T>(
        &self,
        enforce_wal_bound: bool,
        body: impl FnOnce() -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.begin(enforce_wal_bound)?;
        match body() {
            Ok(value) => {
                self.connection().execute_batch("COMMIT")?;
                Ok(value)
            }
            Err(e) => {
                self.rollback();
                Err(e)
            }
        }
    }

    /// Keep single-writer counts in SQLite's transaction/rollback domain.
    ///
    /// TEMP triggers do not change the durable scan schema. Reopening rebuilds counts from
    /// retained rows once; every later snapshot is constant-size.
    fn ensure_resume_counts(&self) -> Result<(), StoreError> {
        let con = self.connection();
        if con
            .prepare("SELECT 1 FROM sqlite_temp_master WHERE name='native_resume_counts'")?
            .exists([])?
        {
            return Ok(());
        }
        con.execute_batch(
            "CREATE TEMP TABLE native_resume_counts(name TEXT PRIMARY KEY,value INTEGER NOT NULL)",
        )?;
        con.execute_batch("INSERT INTO native_resume_counts SELECT 'pages',COUNT(*) FROM pages")?;
        for state in ["queued", "inflight", "done", "excluded"] {
            con.execute(
                "INSERT INTO native_resume_counts SELECT ?,COUNT(*) FROM frontier WHERE state=?",
                [state, state],
            )?;
        }
        for (table, name) in [("pages", "'pages'"), ("frontier", "NEW.state")] {
            con.execute_batch(&format!(
                "CREATE TEMP TRIGGER native_count_{table}_insert AFTER INSERT ON main.{table} BEGIN UPDATE native_resume_counts SET value=value+1 WHERE name={name}; END"
            ))?;
            let old = name.replace("NEW.", "OLD.");
            con.execute_batch(&format!(
                "CREATE TEMP TRIGGER native_count_{table}_delete AFTER DELETE ON main.{table} BEGIN UPDATE native_resume_counts SET value=value-1 WHERE name={old}; END"
            ))?;
        }
        con.execute_batch(
            "CREATE TEMP TRIGGER native_count_frontier_update AFTER UPDATE OF state ON main.frontier BEGIN UPDATE native_resume_counts SET value=value-1 WHERE name=OLD.state; UPDATE native_resume_counts SET value=value+1 WHERE name=NEW.state; END",
        )?;
        Ok(())
    }

    /// Read only scalar resume state; complete edge counts are opt-in.
    /// `include_edges` defaults to false.
    fn resume_snapshot(&self, include_edges: bool) -> Result<Map<String, Value>, StoreError> {
        let con = self.connection();
        let scan = query_maps(con, "SELECT * FROM scan WHERE singleton=1", [])?
            .into_iter()
            .next()
            .ok_or_else(|| scan_error("native scan has no header"))?;
        let mut runtime = query_maps(con, "SELECT * FROM resume_state WHERE singleton=1", [])?
            .into_iter()
            .next()
            .ok_or_else(|| scan_error("native scan has no resume state"))?;
        let throttle_text = runtime.remove("throttle_state_json").unwrap_or(Value::Null);
        let mut throttle: Map<String, Value> =
            serde_json::from_str(throttle_text.as_str().unwrap_or("{}"))?;
        throttle.remove("schema_version");
        throttle.entry("requests_used").or_insert(json!(0));
        runtime.insert("throttle".into(), Value::Object(throttle));
        runtime.remove("singleton");
        runtime.remove("state_version");
        self.ensure_resume_counts()?;
        let mut counts = Map::new();
        for row in query_maps(con, "SELECT name,value FROM temp.native_resume_counts", [])? {
            counts.insert(
                row["name"].as_str().unwrap_or("").to_string(),
                json_item(&row, "value"),
            );
        }
        if include_edges {
            for table in ["links", "forms", "decisions"] {
                let n: i64 =
                    con.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?;
                counts.insert(table.into(), json!(n));
            }
        }
        let mut out = Map::new();
        out.insert("scan".into(), Value::Object(scan));
        out.insert("runtime".into(), Value::Object(runtime));
        out.insert("counts".into(), Value::Object(counts));
        Ok(out)
    }

    /// Seed the frontier with the initial entries, in one transaction.
    fn seed_frontier(
        &self,
        entries: &[Map<String, Value>],
    ) -> Result<Map<String, Value>, StoreError> {
        self.assert_mutable()?;
        let values: Vec<Value> = entries.iter().cloned().map(Value::Object).collect();
        let bounded = bounded_items(&values, "initial seeds", MAX_PAGE_COMMIT_ITEMS)?;
        let entries: Vec<Map<String, Value>> = bounded
            .into_iter()
            .filter_map(|v| v.as_object().cloned())
            .collect();
        let limit = self.stored_query_limit_in_tx_free()?;
        self.in_transaction(true, || {
            let con = self.connection();
            let start: String =
                con.query_row("SELECT start_url FROM scan WHERE singleton=1", [], |r| {
                    r.get(0)
                })?;
            let counts = crate::frontier::apply_seeds(con, &entries, Some(limit), &start)?;
            let version: String = con.query_row(
                "SELECT format_version FROM scan WHERE singleton=1",
                [],
                |r| r.get(0),
            )?;
            if version == "scan.v2" {
                crate::discovery_ledger::store_seeds(con, &entries)?;
            }
            Ok(counts)
        })
    }

    /// Record short partial-capture reasons in the header (limitations and capabilities).
    fn partial_reasons(&self, reasons: &[String]) -> Result<(), StoreError> {
        if reasons.is_empty() {
            return Ok(());
        }
        if reasons.len() > 16
            || reasons
                .iter()
                .any(|r| r.is_empty() || r.chars().count() > 200)
        {
            return Err(scan_error(
                "partial reasons must be a finite short string list",
            ));
        }
        let con = self.connection();
        let (limitations_text, capabilities_text): (String, String) = con.query_row(
            "SELECT limitations_json,capabilities_json FROM scan WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let mut limitations: Vec<Value> = serde_json::from_str(&limitations_text)?;
        for reason in reasons {
            if !limitations.iter().any(|l| l.as_str() == Some(reason)) {
                if limitations.len() >= 64 {
                    return Err(scan_error("native scan limitation registry is full"));
                }
                limitations.push(json!(reason));
            }
        }
        let mut capabilities: Map<String, Value> = serde_json::from_str(&capabilities_text)?;
        for kind in ["pages", "links"] {
            let unavailable = capabilities
                .get(kind)
                .and_then(|c| c.get("state"))
                .and_then(Value::as_str)
                == Some("unavailable");
            if unavailable {
                continue;
            }
            capabilities.insert(
                kind.into(),
                json!({"state": "partial", "reason": reasons.join("; ")}),
            );
        }
        con.execute(
            "UPDATE scan SET crawl_partial=1, limitations_json=?, capabilities_json=? WHERE singleton=1",
            params![dump(&Value::Array(limitations)), dump(&Value::Object(capabilities))],
        )?;
        Ok(())
    }

    /// Exclude the exact inflight lease at the head of the inflight prefix.
    /// `context` defaults to empty.
    fn exclude_lease(
        &self,
        lease: &Lease,
        reason: &str,
        runtime: &Map<String, Value>,
        context: &[Map<String, Value>],
    ) -> Result<(), StoreError> {
        self.assert_mutable()?;
        if reason.is_empty() || reason.chars().count() > 200 {
            return Err(scan_error(
                "exclusion reason must be a short nonempty string",
            ));
        }
        let values: Vec<Value> = context.iter().cloned().map(Value::Object).collect();
        let context: Vec<Map<String, Value>> =
            bounded_items(&values, "exclusion context", MAX_PAGE_COMMIT_ITEMS)?
                .into_iter()
                .filter_map(|v| v.as_object().cloned())
                .collect();
        self.in_transaction(true, || {
            let con = self.connection();
            let row: Option<(String, i64, i64, String)> = con
                .query_row(
                    "SELECT u.url,f.depth,f.queue_ordinal,f.state FROM frontier f JOIN urls u USING(url_id) WHERE f.url_id=?",
                    [lease.url_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;
            let exact = row.as_ref().is_some_and(|(url, depth, ordinal, state)| {
                (url.as_str(), *depth, *ordinal, state.as_str())
                    == (lease.url.as_str(), lease.depth, lease.queue_ordinal, "inflight")
            });
            if !exact {
                return Err(scan_error("exclusion requires the exact inflight lease"));
            }
            let first: Option<i64> =
                con.query_row("SELECT MIN(queue_ordinal) FROM frontier WHERE state='inflight'", [], |r| r.get(0))?;
            if first != Some(lease.queue_ordinal) {
                return Err(scan_error("exclusion must preserve the contiguous inflight prefix"));
            }
            let mut decision = Map::new();
            decision.insert("url".into(), json!(lease.url));
            decision.insert("reason".into(), json!(reason));
            decision.insert("source".into(), json!("frontier"));
            decision.insert("depth".into(), json!(lease.depth));
            decision.insert("occurrence_key".into(), json!(format!("lease:{}:excluded", lease.queue_ordinal)));
            insert(con, "decisions", &decision)?;
            con.execute("UPDATE frontier SET state='excluded' WHERE url_id=?", [lease.url_id])?;
            self.write_runtime(runtime, 0)?;
            for item in &context {
                put_context(con, item)?;
            }
            Ok(())
        })
    }

    /// Add only new frontier identities in one transaction.
    /// `reason` defaults to `enqueue`.
    fn enqueue(&self, entries: &[(String, i64)], reason: &str) -> Result<Vec<Lease>, StoreError> {
        self.assert_mutable()?;
        if reason.is_empty() || reason.chars().count() > 200 {
            return Err(scan_error("enqueue reason must be a short nonempty string"));
        }
        let values: Vec<Value> = entries.iter().map(|(u, d)| json!([u, d])).collect();
        bounded_items(&values, "initial frontier entries", MAX_PAGE_COMMIT_ITEMS)?;
        for (url, depth) in entries {
            if url.is_empty() || *depth < 0 {
                return Err(scan_error(
                    "frontier enqueue requires nonempty URL strings and nonnegative integer depths",
                ));
            }
        }
        self.in_transaction(true, || {
            let con = self.connection();
            let mut next_ordinal: i64 =
                con.query_row("SELECT COALESCE(MAX(queue_ordinal)+1, 0) FROM frontier", [], |r| r.get(0))?;
            let mut added = Vec::new();
            for (url, depth) in entries {
                let url_id = crate::scan_artifact::url(con, url)?;
                let known = con.prepare("SELECT 1 FROM frontier WHERE url_id=?")?.exists([url_id])?;
                if known {
                    continue;
                }
                con.execute(
                    "INSERT INTO frontier(url_id, queue_ordinal, depth, state) VALUES(?,?,?, 'queued')",
                    params![url_id, next_ordinal, depth],
                )?;
                added.push(Lease { url_id, url: url.clone(), depth: *depth, queue_ordinal: next_ordinal });
                self.event(
                    "queue",
                    json!({"queue_ordinal": next_ordinal, "url_id": url_id, "depth": depth, "reason": reason, "count": 1}),
                )?;
                next_ordinal += 1;
            }
            self.event_coverage()?;
            Ok(added)
        })
    }

    /// Drop queued frontier URLs the operator's narrowed scope rejects (#1485).
    ///
    /// `classify` maps a queued URL to its rejection reason under the NEW scope ("" when still
    /// accepted): the crawl layer owns the scope rules, the artifact owns its queue. Only rows
    /// still queued are dropped: a URL already claimed into a batch is fetched, and that fact
    /// stays visible. Every drop gets a decision row naming the rule that rejected it, the stored
    /// scope header moves with its fingerprints in the same transaction (so a resumed crawl
    /// discovers under the narrowed rules), and one `scope_narrowed` event per applied pattern
    /// records the operator action with its drop count.
    /// Both patterns default to unset.
    fn narrow_scope(
        &self,
        classify: &dyn Fn(&str) -> String,
        exclude_pattern: Option<&str>,
        include_pattern: Option<&str>,
    ) -> Result<BTreeMap<String, i64>, StoreError> {
        self.assert_mutable()?;
        if exclude_pattern.is_none() && include_pattern.is_none() {
            return Err(scan_error(
                "narrow_scope requires an exclude or include pattern",
            ));
        }
        let (reasons, queued) = self.in_transaction(true, || {
            let con = self.connection();
            let start_url: String = con.query_row("SELECT start_url FROM scan WHERE singleton=1", [], |r| r.get(0))?;
            let rows: Vec<(i64, String, i64, i64)> = {
                let mut stmt = con.prepare(
                    "SELECT f.url_id, u.url, f.depth, f.queue_ordinal FROM frontier f JOIN urls u USING(url_id) WHERE f.state='queued' ORDER BY f.queue_ordinal",
                )?;
                let mapped = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
                mapped.collect::<Result<_, _>>()?
            };
            let mut reasons: BTreeMap<String, i64> = BTreeMap::new();
            for (url_id, url, depth, ordinal) in &rows {
                if *url == start_url {
                    // The seed is always fetched, exactly like the discovery scope's own rule: a
                    // crawl whose start URL the operator's include pattern rejects would resume
                    // into an empty site (an excluded start is never re-queued), so it is exempt.
                    continue;
                }
                let reason = classify(url);
                if reason.is_empty() {
                    continue;
                }
                let mut decision = Map::new();
                decision.insert("url".into(), json!(url));
                decision.insert("reason".into(), json!(reason));
                decision.insert("source".into(), json!("operator"));
                decision.insert("depth".into(), json!(depth));
                decision.insert("occurrence_key".into(), json!(format!("operator:{ordinal}:narrowed")));
                insert(con, "decisions", &decision)?;
                con.execute("UPDATE frontier SET state='excluded' WHERE url_id=?", [url_id])?;
                *reasons.entry(reason).or_insert(0) += 1;
            }
            self.narrow_config_scope(exclude_pattern, include_pattern)?;
            if let Some(pattern) = exclude_pattern {
                self.event(
                    "scope_narrowed",
                    json!({"kind": "exclude", "pattern": pattern, "dropped": reasons.get("excluded_by_pattern").copied().unwrap_or(0)}),
                )?;
            }
            if let Some(pattern) = include_pattern {
                self.event(
                    "scope_narrowed",
                    json!({"kind": "include", "pattern": pattern, "dropped": reasons.get("not_included_by_pattern").copied().unwrap_or(0)}),
                )?;
            }
            self.event_coverage()?;
            Ok((reasons, rows.len() as i64))
        })?;
        let dropped: i64 = reasons.values().sum();
        Ok(BTreeMap::from([
            ("dropped".to_string(), dropped),
            ("queued".to_string(), queued - dropped),
        ]))
    }

    /// Fold one operator pattern into the stored scope header, fingerprints included.
    ///
    /// Called inside the caller's transaction. The header fingerprint covers the scope patterns,
    /// and the config-extension context carries the effective-config fingerprint, so both are
    /// recomputed from the exact shapes their readers reconstruct: a later open or resume
    /// validates against what this wrote, and the scan event beside it says the change was the
    /// operator's.
    fn narrow_config_scope(
        &self,
        exclude_pattern: Option<&str>,
        include_pattern: Option<&str>,
    ) -> Result<(), StoreError> {
        let con = self.connection();
        let apply = |patterns: &mut Map<String, Value>| {
            let scope = patterns.entry("scope").or_insert_with(|| json!({}));
            if let Some(scope) = scope.as_object_mut() {
                if let Some(pattern) = exclude_pattern {
                    let mut list: Vec<Value> = scope
                        .get("exclude_patterns")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    list.push(json!(pattern));
                    scope.insert("exclude_patterns".into(), Value::Array(list));
                }
                if let Some(pattern) = include_pattern {
                    scope.insert("include_patterns".into(), json!([pattern]));
                }
            }
        };
        let mut effective = crate::scan_schema::read_effective_config(con)?;
        apply(&mut effective);
        let header_text: String =
            con.query_row("SELECT config_json FROM scan WHERE singleton=1", [], |r| {
                r.get(0)
            })?;
        let mut header: Map<String, Value> = serde_json::from_str(&header_text)?;
        apply(&mut header);
        let fingerprint = spiderhead_core::crawl_settings::fingerprint(
            &Value::Object(header.clone()),
            &spiderhead_core::host::SystemHost,
        )
        .map_err(|e| StoreError::Invalid(e.to_string()))?;
        con.execute(
            "UPDATE scan SET config_json=?, config_fingerprint=? WHERE singleton=1",
            params![dump(&Value::Object(header)), fingerprint],
        )?;
        let item: Option<String> = con
            .query_row(
                "SELECT payload_json FROM context_items WHERE kind='native_config_extension' AND item_key='run'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(text) = item {
            let mut payload: Map<String, Value> = serde_json::from_str(&text)?;
            payload.insert(
                "effective_fingerprint".into(),
                json!(crate::scan_schema::effective_config_fingerprint(&effective)),
            );
            con.execute(
                "UPDATE context_items SET payload_json=? WHERE kind='native_config_extension' AND item_key='run'",
                [dump(&Value::Object(payload))],
            )?;
        }
        Ok(())
    }

    /// Queue operator-supplied targets at depth 0, journalled as operator URLs (#1485).
    ///
    /// Injection is deliberate and explicit: these URLs bypass the scope rules, because the
    /// operator asked for exactly these targets, not for discovery. Each accepted URL gets its
    /// `queue` event with reason `operator`, and one `urls_added` event records how many new
    /// frontier identities the request added. Already-known URLs add nothing and say so.
    fn enqueue_operator(&self, urls: &[String]) -> Result<BTreeMap<String, i64>, StoreError> {
        let entries: Vec<(String, i64)> = urls.iter().map(|u| (u.clone(), 0)).collect();
        let added = self.enqueue(&entries, "operator")?;
        self.in_transaction(true, || {
            self.event("urls_added", json!({"count": added.len()}))?;
            self.event_coverage()
        })?;
        Ok(BTreeMap::from([
            ("added".to_string(), added.len() as i64),
            (
                "already_known".to_string(),
                (entries.len() - added.len()) as i64,
            ),
        ]))
    }

    /// Claim up to `limit` queued URLs, bounded by the stored concurrency.
    fn claim(&self, limit: i64) -> Result<Vec<Lease>, StoreError> {
        if limit < 1 {
            return Err(StoreError::Invalid(
                "claim limit must be a positive integer".into(),
            ));
        }
        self.assert_mutable()?;
        let con = self.connection();
        let text: String =
            con.query_row("SELECT config_json FROM scan WHERE singleton=1", [], |r| {
                r.get(0)
            })?;
        let config: Value = serde_json::from_str(&text)?;
        let max_concurrency = config["speed"]["concurrency"]
            .as_i64()
            .ok_or_else(|| scan_error("stored effective config lacks a concurrency limit"))?;
        let inflight: i64 = con.query_row(
            "SELECT COUNT(*) FROM frontier WHERE state='inflight'",
            [],
            |r| r.get(0),
        )?;
        let limit = (limit.min(max_concurrency - inflight)).max(0);
        let claimed = self.in_transaction(true, || {
            let leases: Vec<Lease> = {
                let mut stmt = con.prepare(
                    "SELECT f.url_id, u.url, f.depth, f.queue_ordinal FROM frontier f JOIN urls u USING(url_id) WHERE f.state='queued' ORDER BY f.queue_ordinal LIMIT ?",
                )?;
                let mapped = stmt.query_map([limit], |r| {
                    Ok(Lease { url_id: r.get(0)?, url: r.get(1)?, depth: r.get(2)?, queue_ordinal: r.get(3)? })
                })?;
                mapped.collect::<Result<_, _>>()?
            };
            self.hit("before_claim");
            for lease in &leases {
                con.execute("UPDATE frontier SET state='inflight' WHERE url_id=?", [lease.url_id])?;
            }
            for lease in &leases {
                self.event(
                    "queue",
                    json!({"queue_ordinal": lease.queue_ordinal, "url_id": lease.url_id, "depth": lease.depth, "reason": "claimed", "count": 1}),
                )?;
            }
            self.event_coverage()?;
            Ok(leases)
        })?;
        self.hit("after_claim");
        Ok(claimed)
    }

    /// Requeue only after this object obtained the exclusive lifetime lock.
    fn recover_inflight(&self) -> Result<i64, StoreError> {
        self.assert_mutable()?;
        let con = self.connection();
        if !con
            .prepare("SELECT 1 FROM frontier WHERE state='inflight' LIMIT 1")?
            .exists([])?
        {
            return Ok(0);
        }
        self.in_transaction(true, || {
            let count: i64 = con.query_row(
                "SELECT COUNT(*) FROM frontier WHERE state='inflight'",
                [],
                |r| r.get(0),
            )?;
            con.execute_batch("UPDATE frontier SET state='queued' WHERE state='inflight'")?;
            self.event(
                "checkpoint",
                json!({"state": "recovered", "queued": count, "inflight": 0}),
            )?;
            self.event_coverage()?;
            Ok(count)
        })
    }

    /// Validate and store the runtime (circuit, throttle, elapsed) state.
    fn write_runtime(
        &self,
        runtime: &Map<String, Value>,
        lease_depth: i64,
    ) -> Result<(), StoreError> {
        let con = self.connection();
        let required = [
            "max_depth_reached",
            "elapsed_seconds",
            "circuit_timeout_streak",
            "circuit_server_error_streak",
            "crawl_delay_applied",
            "throttle",
        ];
        if runtime.len() != required.len() || !required.iter().all(|k| runtime.contains_key(*k)) {
            return Err(scan_error(
                "runtime state must have the exact child-C key set",
            ));
        }
        let mut throttle = runtime["throttle"].as_object().cloned().unwrap_or_default();
        let keys = |m: &Map<String, Value>, wanted: &[&str]| {
            m.len() == wanted.len() && wanted.iter().all(|k| m.contains_key(*k))
        };
        if keys(
            &throttle,
            &["delay_seconds", "concurrency", "consecutive_ok"],
        ) {
            throttle.insert("requests_used".into(), json!(0));
        }
        if !keys(
            &throttle,
            &[
                "delay_seconds",
                "concurrency",
                "consecutive_ok",
                "requests_used",
            ],
        ) {
            return Err(scan_error(
                "runtime throttle state must have the exact child-C key set",
            ));
        }
        let text: String =
            con.query_row("SELECT config_json FROM scan WHERE singleton=1", [], |r| {
                r.get(0)
            })?;
        let config = validate_config(&serde_json::from_str::<Value>(&text)?)?;
        let ceiling = config["speed"]["concurrency"].as_i64().unwrap_or(0);
        let crawl_delay = &runtime["crawl_delay_applied"];
        let invalid = [
            "max_depth_reached",
            "circuit_timeout_streak",
            "circuit_server_error_streak",
        ]
        .iter()
        .any(|n| !is_count(&runtime[*n]))
            || !finite_nonnegative(&runtime["elapsed_seconds"])
            || (!crawl_delay.is_null() && !finite_nonnegative(crawl_delay))
            || !finite_nonnegative(&throttle["delay_seconds"])
            || !throttle["concurrency"]
                .as_i64()
                .is_some_and(|c| (1..=ceiling).contains(&c))
            || !throttle["consecutive_ok"]
                .as_i64()
                .is_some_and(|c| (0..=2).contains(&c))
            || !is_count(&throttle["requests_used"]);
        if invalid {
            return Err(scan_error(
                "runtime state has invalid finite values or throttle bounds",
            ));
        }
        let (current_depth, current_elapsed): (i64, f64) = con.query_row(
            "SELECT max_depth_reached, elapsed_seconds FROM resume_state WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let depth = runtime["max_depth_reached"].as_i64().unwrap_or(0);
        let elapsed = runtime["elapsed_seconds"].as_f64().unwrap_or(0.0);
        if depth < current_depth.max(lease_depth) {
            return Err(scan_error(
                "runtime max depth must not regress below committed evidence",
            ));
        }
        if elapsed < current_elapsed {
            return Err(scan_error("runtime elapsed seconds must not regress"));
        }
        let mut stored = Map::new();
        stored.insert("schema_version".into(), json!("scan_throttle.v2"));
        stored.extend(throttle);
        con.execute(
            "UPDATE resume_state SET max_depth_reached=?, elapsed_seconds=?, circuit_timeout_streak=?, circuit_server_error_streak=?, crawl_delay_applied=?, throttle_state_json=? WHERE singleton=1",
            params![
                depth,
                elapsed,
                runtime["circuit_timeout_streak"].as_i64(),
                runtime["circuit_server_error_streak"].as_i64(),
                crawl_delay.as_f64(),
                dump(&Value::Object(stored))
            ],
        )?;
        Ok(())
    }

    /// Checkpoint the shared HTTP-attempt count without changing frontier state.
    fn record_request_count(&self, requests_used: i64) -> Result<(), StoreError> {
        self.assert_mutable()?;
        if requests_used < 0 {
            return Err(scan_error("request count must be a nonnegative integer"));
        }
        let con = self.connection();
        let text: String =
            con.query_row("SELECT config_json FROM scan WHERE singleton=1", [], |r| {
                r.get(0)
            })?;
        let config: Value = serde_json::from_str(&text)?;
        let maximum = match config.get("limits").and_then(|l| l.get("max_requests")) {
            None => 0,
            Some(v) => v
                .as_i64()
                .filter(|m| *m >= 0)
                .ok_or_else(|| scan_error("stored request budget is invalid"))?,
        };
        if maximum != 0 && requests_used > maximum {
            return Err(scan_error("request count exceeds the stored budget"));
        }
        self.in_transaction(true, || {
            let mut snapshot = self.resume_snapshot(false)?;
            let mut runtime = snapshot
                .remove("runtime")
                .and_then(|r| r.as_object().cloned())
                .unwrap_or_default();
            if let Some(throttle) = runtime.get_mut("throttle").and_then(Value::as_object_mut) {
                throttle.insert("requests_used".into(), json!(requests_used));
            }
            let depth = runtime["max_depth_reached"].as_i64().unwrap_or(0);
            self.write_runtime(&runtime, depth)?;
            self.event(
                "budget",
                json!({"kind": "http_requests", "limit": maximum, "used": requests_used}),
            )?;
            self.event_coverage()
        })
    }

    /// The stored per-path query-variant limit.
    fn stored_query_limit(&self) -> Result<i64, StoreError> {
        self.stored_query_limit_in_tx_free()
    }

    /// Reads the limit without needing a transaction (shared by [`seed_frontier`](Self::seed_frontier)).
    fn stored_query_limit_in_tx_free(&self) -> Result<i64, StoreError> {
        let con = self.connection();
        let text: String =
            con.query_row("SELECT config_json FROM scan WHERE singleton=1", [], |r| {
                r.get(0)
            })?;
        let limit = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|c| c.get("limits")?.get("max_query_variants_per_path").cloned())
            .ok_or_else(|| scan_error("stored effective config lacks a query-variant limit"))?;
        match limit.as_i64() {
            Some(n) if n >= 0 && limit.is_i64() => Ok(n),
            _ => Err(scan_error("stored query-variant limit is invalid")),
        }
    }

    /// Record why a run stopped; the WAL bound must not gate this one header row.
    fn interrupt(&self, reason: &str) -> Result<(), StoreError> {
        self.assert_mutable()?;
        self.in_transaction(false, || {
            self.connection().execute(
                "UPDATE scan SET lifecycle='interrupted', finish_reason=?, crawl_partial=1 WHERE singleton=1",
                [reason],
            )?;
            self.event("stop", json!({"reason": reason}))?;
            self.event_coverage()
        })
    }

    /// Record a deliberate URL-budget stop; `finish_capture` then finalizes it as finished.
    fn stop_by_budget(&self, reason: &str) -> Result<(), StoreError> {
        self.assert_mutable()?;
        self.in_transaction(false, || {
            self.connection().execute_batch(
                "UPDATE scan SET lifecycle='interrupted', finish_reason='stopped_by_budget', crawl_partial=1 WHERE singleton=1",
            )?;
            self.event("stop", json!({"reason": reason}))?;
            self.event_coverage()
        })
    }

    /// Checkpoint the WAL and switch to the DELETE journal; false when readers keep it busy.
    fn finalize_checkpoint(&self, timeout_seconds: f64) -> Result<bool, StoreError> {
        let con = self.connection();
        let deadline = Instant::now() + Duration::from_secs_f64(timeout_seconds);
        let prior: i64 = con.query_row("PRAGMA busy_timeout", [], |r| r.get(0))?;
        con.execute_batch("PRAGMA busy_timeout=0")?;
        let outcome = (|| -> Result<bool, StoreError> {
            loop {
                let busy: i64 =
                    con.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))?;
                if busy == 0 {
                    let mode: String = con
                        .query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0))
                        .unwrap_or_default();
                    if mode.eq_ignore_ascii_case("delete") {
                        return Ok(true);
                    }
                }
                let now = Instant::now();
                if now >= deadline {
                    return Ok(false);
                }
                std::thread::sleep((deadline - now).min(Duration::from_millis(50)));
            }
        })();
        con.execute_batch(&format!("PRAGMA busy_timeout={prior}"))?;
        outcome
    }

    /// Requeue recovered work; an empty frontier has only finalization left to do.
    fn resume_or_finalize(&self) -> Result<bool, StoreError> {
        self.assert_mutable()?;
        self.recover_inflight()?;
        let con = self.connection();
        if con
            .prepare("SELECT 1 FROM frontier WHERE state IN ('queued','inflight') LIMIT 1")?
            .exists([])?
        {
            return Ok(false);
        }
        self.finish_without_audit("capture_finished_no_audit", FINALIZATION_TIMEOUT_SECONDS)
    }

    /// Persist the derived click-depth and URL-query projections of a finished scan.
    ///
    /// The query projection is optional: a failure to build it never blocks finalization.
    fn persist_projections(&self) -> Result<(), StoreError> {
        let con = self.connection();
        crate::click_depth::persist(con, 5.0)?;
        let _ = crate::url_query_projection::persist(
            con,
            crate::url_query_projection::BUILD_BUDGET_SECONDS,
        );
        Ok(())
    }

    /// Finish only after a bounded WAL checkpoint produces a DELETE-journal file.
    /// `reason` defaults to `capture_finished_no_audit`; `timeout_seconds` defaults to `FINALIZATION_TIMEOUT_SECONDS`.
    fn finish_without_audit(&self, reason: &str, timeout_seconds: f64) -> Result<bool, StoreError> {
        self.assert_mutable()?;
        if !timeout_seconds.is_finite() || timeout_seconds <= 0.0 {
            return Err(scan_error("native finalization timeout must be positive"));
        }
        let con = self.connection();
        // Finalization is the remedy for a large WAL, so ordinary writer backpressure must not
        // reject it before its checkpoint attempt.
        con.execute_batch("BEGIN IMMEDIATE")?;
        let pending = con
            .prepare("SELECT 1 FROM frontier WHERE state IN ('queued','inflight') LIMIT 1")
            .and_then(|mut s| s.exists([]));
        match pending {
            Ok(false) => con.execute_batch("ROLLBACK")?,
            Ok(true) => {
                self.rollback();
                return Err(scan_error(
                    "cannot finish a scan with queued or inflight frontier work",
                ));
            }
            Err(e) => {
                self.rollback();
                return Err(e.into());
            }
        }
        let prior_state = |con: &Connection| -> Result<(String, Option<String>), StoreError> {
            Ok(con.query_row(
                "SELECT lifecycle, finish_reason FROM scan WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        };
        // An explicit stop is not a drained queue: the file may finalize, but the capture keeps
        // its interrupted lifecycle and reason instead of stamping a false finished_at (#712).
        let explicit_stop = |prior: &(String, Option<String>)| {
            prior.0 == "interrupted" && prior.1.as_deref() != Some("finalization_blocked")
        };
        if self.finalize_checkpoint(timeout_seconds)? {
            con.execute_batch("BEGIN IMMEDIATE")?;
            let outcome = (|| -> Result<bool, StoreError> {
                if explicit_stop(&prior_state(con)?) {
                    con.execute_batch("COMMIT")?;
                    return Ok(true);
                }
                con.execute(
                    "UPDATE scan SET lifecycle='finished', finish_reason=?, finished_at=? WHERE singleton=1",
                    params![reason, utc()?],
                )?;
                self.persist_projections()?;
                con.execute_batch("COMMIT")?;
                Ok(true)
            })();
            if outcome.is_err() {
                self.rollback();
            }
            return outcome;
        }
        // A reader blocked finalization. Retain the recoverable artifact but do not advertise it
        // as a finished one-file scan. An explicit prior stop keeps its own reason so a later
        // retry cannot read it as a merely blocked finalization (#712).
        let outcome = (|| -> Result<(), StoreError> {
            con.execute_batch("BEGIN IMMEDIATE")?;
            if !explicit_stop(&prior_state(con)?) {
                con.execute_batch(
                    "UPDATE scan SET lifecycle='interrupted', finish_reason='finalization_blocked' WHERE singleton=1",
                )?;
            }
            con.execute_batch("COMMIT")?;
            Ok(())
        })();
        if outcome.is_err() {
            self.rollback();
        }
        outcome.map(|()| false)
    }

    /// Finalize the file independently of collection completeness and audit availability.
    /// `reason` defaults to `finished`; `timeout_seconds` defaults to `FINALIZATION_TIMEOUT_SECONDS`.
    fn finish_capture(&self, reason: &str, timeout_seconds: f64) -> Result<bool, StoreError> {
        self.assert_mutable()?;
        if reason.is_empty() || !timeout_seconds.is_finite() || timeout_seconds <= 0.0 {
            return Err(scan_error(
                "invalid capture finalization reason or deadline",
            ));
        }
        let ready = self.finalize_checkpoint(timeout_seconds)?;
        let con = self.connection();
        con.execute_batch("BEGIN IMMEDIATE")?;
        let outcome = (|| -> Result<bool, StoreError> {
            let (prior_lifecycle, prior_reason): (String, Option<String>) = con.query_row(
                "SELECT lifecycle, finish_reason FROM scan WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let pending = con
                .prepare("SELECT 1 FROM frontier WHERE state IN ('queued','inflight') LIMIT 1")?
                .exists([])?;
            // A drained queue is finished work: partial evidence is already named by
            // crawl_partial and must not read as an interrupted capture (#712). An explicit
            // stop -- abort before work was accepted, cancel, or an error circuit -- keeps its
            // interrupted lifecycle even when nothing is left in the queue. Only a checkpoint
            // that merely ran out of deadline may retry into finished.
            let reason_text = prior_reason.as_deref();
            let budget_stop = reason_text == Some("stopped_by_budget");
            let stopped = prior_lifecycle == "interrupted"
                && !matches!(
                    reason_text,
                    Some("finalization_blocked") | Some("stopped_by_budget")
                );
            // A URL-budget stop leaves queued work by design; it is a finished capture.
            let finished = ready && (!pending || budget_stop) && !stopped;
            let lifecycle = if finished { "finished" } else { "interrupted" };
            let reason = if budget_stop {
                "stopped_by_budget"
            } else {
                reason
            };
            // A blocked checkpoint records why finalization could not run, but must not erase an
            // explicit stop: keeping its reason prevents a later retry from stamping a false
            // finished_at (#712).
            let blocked_reason = if stopped {
                reason
            } else {
                "finalization_blocked"
            };
            let finished_at = if finished { Some(utc()?) } else { None };
            con.execute(
                "UPDATE scan SET lifecycle=?,finish_reason=?,finished_at=? WHERE singleton=1",
                params![
                    lifecycle,
                    if ready { reason } else { blocked_reason },
                    finished_at
                ],
            )?;
            if finished {
                self.persist_projections()?;
            }
            con.execute_batch("COMMIT")?;
            Ok(ready)
        })();
        if outcome.is_err() {
            self.rollback();
        }
        outcome
    }

    /// Finalize a derived artifact without treating historical frontier work as resumable.
    fn finish_reanalysis(&self) -> Result<bool, StoreError> {
        let con = self.connection();
        let kind: String =
            con.query_row("SELECT source_kind FROM scan WHERE singleton=1", [], |r| {
                r.get(0)
            })?;
        if kind != "reanalysis" {
            return Err(scan_error(
                "only a derived artifact can use reanalysis finalization",
            ));
        }
        if !self.finalize_checkpoint(FINALIZATION_TIMEOUT_SECONDS)? {
            return Err(scan_error("derived reanalysis finalization was blocked"));
        }
        con.execute_batch("BEGIN IMMEDIATE")?;
        let outcome = (|| -> Result<bool, StoreError> {
            con.execute(
                "UPDATE scan SET lifecycle='finished',finish_reason='offline_reanalysis',finished_at=? WHERE singleton=1",
                [utc()?],
            )?;
            self.persist_projections()?;
            con.execute_batch("COMMIT")?;
            Ok(true)
        })();
        if outcome.is_err() {
            self.rollback();
        }
        outcome
    }

    /// Back up the live scan to `destination`: a consistent, journal-free copy that passes
    /// inspection, published atomically (hard link) and never over an existing file.
    /// `reserve_bytes` defaults to `SNAPSHOT_RESERVE_BYTES`, `temp_margin_bytes` to 0,
    /// `timeout_seconds` defaults to `BACKUP_TIMEOUT_SECONDS`, `cancelled` to unset.
    fn snapshot(
        &self,
        destination: &Path,
        reserve_bytes: i64,
        temp_margin_bytes: i64,
        timeout_seconds: f64,
        cancelled: Option<&dyn Fn() -> bool>,
    ) -> Result<PathBuf, StoreError> {
        let target = std::path::absolute(destination)?;
        let exists = |path: &Path| std::fs::symlink_metadata(path).is_ok();
        let already = || {
            scan_error(&format!(
                "snapshot target already exists: {}",
                target.display()
            ))
        };
        if exists(&target) {
            return Err(already());
        }
        if reserve_bytes < SNAPSHOT_RESERVE_BYTES
            || temp_margin_bytes < 0
            || !timeout_seconds.is_finite()
            || timeout_seconds <= 0.0
        {
            return Err(scan_error(
                "snapshot reserve, temporary margin and timeout must be bounded positive values",
            ));
        }
        let con = self.connection();
        let page_count: i64 = con.query_row("PRAGMA page_count", [], |r| r.get(0))?;
        let page_size: i64 = con.query_row("PRAGMA page_size", [], |r| r.get(0))?;
        let main_path: String = con.query_row(
            "SELECT file FROM pragma_database_list WHERE name='main'",
            [],
            |r| r.get(0),
        )?;
        let sibling = |suffix: &str| PathBuf::from(format!("{main_path}{suffix}"));
        let observed_margin: i64 = [sibling("-wal"), sibling("-shm")]
            .iter()
            .filter_map(|p| std::fs::metadata(p).ok())
            .map(|m| m.len() as i64)
            .sum();
        let required = page_count * page_size
            + observed_margin
            + reserve_bytes.max(0)
            + temp_margin_bytes.max(0);
        let parent = target.parent().map(Path::to_path_buf).unwrap_or_default();
        let free = free_bytes(&parent)?;
        if free < required {
            return Err(scan_error(&format!(
                "insufficient free space for native snapshot: need {required} bytes, have {free}"
            )));
        }
        let temporary = parent.join(format!(
            ".native-snapshot-{}.sqlite",
            uuid::Uuid::new_v4().simple()
        ));
        let outcome = (|| -> Result<(), StoreError> {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            let mut dest = Connection::open(&temporary)?;
            let deadline = Instant::now() + Duration::from_secs_f64(timeout_seconds);
            {
                let backup = rusqlite::backup::Backup::new(con, &mut dest)?;
                loop {
                    use rusqlite::backup::StepResult;
                    match backup.step(128)? {
                        StepResult::Done => break,
                        _ => {
                            if cancelled.is_some_and(|c| c()) || Instant::now() > deadline {
                                return Err(scan_error("native snapshot cancelled or timed out"));
                            }
                            std::thread::sleep(Duration::from_millis(10));
                        }
                    }
                }
            }
            dest.query_row("PRAGMA journal_mode=DELETE", [], |r| r.get::<_, String>(0))?;
            dest.close().map_err(|(_, e)| e)?;
            fsync_file(&temporary)?;
            crate::scan_schema::NativeScanSchemaMixin::inspect(&temporary, None)?;
            std::fs::hard_link(&temporary, &target).map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    already()
                } else {
                    e.into()
                }
            })?;
            fsync_directory(&parent)
        })();
        let _ = std::fs::remove_file(&temporary);
        match outcome {
            Ok(()) => Ok(target),
            Err(e) if e.to_string().starts_with("snapshot target already exists") => Err(e),
            Err(e) => Err(scan_error(&format!("cannot snapshot native scan: {e}"))),
        }
    }
}

/// Free bytes on the volume holding `dir`.
fn free_bytes(dir: &Path) -> Result<i64, StoreError> {
    let free = spiderhead_core::filesystem::free_space(dir)?;
    Ok(i64::try_from(free).unwrap_or(i64::MAX))
}

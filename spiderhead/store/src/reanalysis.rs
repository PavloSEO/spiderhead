//! Derived-scan storage for offline reanalysis: an atomic private copy of a
//! native capture, re-identified as a derived artifact that keeps the capture
//! provenance and can never resume collection.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::Connection;
use serde_json::{json, Map, Value};

use crate::audit_v2::audit_v2_path;
use crate::error::StoreError;
use crate::scan_artifact::ScanError;
use crate::scan_lease::NativeScanLease;

/// A scan-artifact rejection as a store error.
fn scan_error(message: impl Into<String>) -> StoreError {
    StoreError::ScanError(ScanError {
        message: message.into(),
    })
}
use crate::native_scan::NativeScan;
use crate::scan_artifact::open_scan;
use crate::scan_lease::utc;
use crate::scan_schema::WriterLock;

/// Wall-clock ceiling for the copy step (`BACKUP_TIMEOUT_SECONDS`).
const BACKUP_TIMEOUT_SECONDS: u64 = 60;
/// Required runtime provenance keys; the set is complete by contract.
const RUNTIME_KEYS: [&str; 5] = ["python", "sqlite", "httpx", "lxml", "beautifulsoup4"];

/// A derived writer over its private copy plus a read-only source connection.
///
/// The output path is published only by [`DerivedScan::publish`]; a handle that
/// is dropped without publishing removes its temporary files.
pub struct DerivedScan {
    writer: NativeScan,
    /// Read-only connection to the validated capture source.
    pub source: Connection,
    target: PathBuf,
    temporary: PathBuf,
    published: AtomicBool,
}

impl DerivedScan {
    /// The derived writer (mutable header/provenance/page mutations).
    pub fn writer(&mut self) -> &mut NativeScan {
        &mut self.writer
    }

    /// Field-split access: page mutations on the writer while reads continue
    /// against the validated source connection.
    pub fn split(&mut self) -> (&mut NativeScan, &Connection) {
        (&mut self.writer, &self.source)
    }

    /// The `context_items` payload of the derived provenance row.
    pub fn provenance(&self) -> Result<Map<String, Value>, StoreError> {
        let payload: String = self
            .writer
            .try_con()?
            .query_row(
                "SELECT payload_json FROM context_items \
                 WHERE kind='reanalysis_provenance' AND item_key='run'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| scan_error(format!("capture provenance is absent: {e}")))?;
        serde_json::from_str(&payload)
            .map_err(|e| scan_error(format!("capture provenance payload is invalid: {e}")))
    }

    /// Finalize the derived artifact and publish it at the requested output
    /// path (and its audit companion, when one was written next to it).
    pub fn publish(mut self) -> Result<PathBuf, StoreError> {
        if !self.writer.finish_reanalysis()? {
            return Err(scan_error("derived reanalysis finalization was blocked"));
        }
        {
            let con = self.writer.try_con()?;
            validate_derived(con)?;
        }
        self.writer.close();
        fsync_file(&self.temporary)?;
        let temporary_audit = audit_v2_path(&self.temporary);
        if temporary_audit.exists() {
            fsync_file(&temporary_audit)?;
        }
        // The publish hard link can only fail on an existing target; the temp
        // file name is unique within the same directory.
        std::fs::hard_link(&self.temporary, &self.target).map_err(|_| {
            scan_error(format!(
                "derived reanalysis output already exists: {}",
                self.target.display()
            ))
        })?;
        if temporary_audit.exists() {
            let target_audit = audit_v2_path(&self.target);
            std::fs::hard_link(&temporary_audit, &target_audit).map_err(StoreError::Io)?;
        }
        fsync_directory(&self.target)?;
        self.published.store(true, Ordering::SeqCst);
        Ok(self.target.clone())
    }
}

impl Drop for DerivedScan {
    fn drop(&mut self) {
        if !self.published.load(Ordering::SeqCst) {
            self.writer.close();
        }
        let audit = audit_v2_path(&self.temporary);
        let _ = std::fs::remove_file(&self.temporary);
        let _ = std::fs::remove_file(audit);
        for suffix in ["-wal", "-shm", ".writer.lock"] {
            let mut name = self
                .temporary
                .file_name()
                .unwrap_or_default()
                .to_os_string();
            name.push(suffix);
            let _ = std::fs::remove_file(self.temporary.with_file_name(name));
        }
    }
}

/// `scan_schema._validate_native`: artifact identity must survive the copy.
fn validate_derived(con: &Connection) -> Result<(), StoreError> {
    let application_id: i64 = con.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    let expected: i64 = crate::scan_artifact::APPLICATION_ID;
    if application_id != expected {
        return Err(scan_error("foreign application_id"));
    }
    let user_version: i64 = con.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if user_version == 2 {
        let version: Option<String> = con
            .query_row(
                "SELECT format_version FROM scan WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .map_err(StoreError::from)?;
        if version.as_deref() != Some("scan.v2") {
            return Err(scan_error(
                "scan user_version disagrees with its format_version",
            ));
        }
    }
    Ok(())
}

fn fsync_file(path: &Path) -> Result<(), StoreError> {
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}

fn fsync_directory(path: &Path) -> Result<(), StoreError> {
    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
    std::fs::File::open(&parent)?.sync_all()?;
    Ok(())
}

/// Build a derived artifact from one capture source without any network work.
///
/// Validation happens on the requested arguments before any copying; the copy
/// is created under a private temporary name next to the output and published
/// atomically by [`DerivedScan::publish`].
pub fn derived_scan(
    source_path: &Path,
    out: &Path,
    producer_version: &str,
    producer_revision: &str,
    runtime_versions: &Map<String, Value>,
) -> Result<DerivedScan, StoreError> {
    if producer_version.is_empty() {
        return Err(scan_error(
            "derived reanalysis producer version is required",
        ));
    }
    if producer_revision.len() != 40
        || !producer_revision
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Err(scan_error(
            "derived reanalysis producer revision must be a lowercase Git SHA",
        ));
    }
    check_runtime_versions(runtime_versions)?;
    let target = std::path::absolute(out)?;
    if std::fs::symlink_metadata(&target).is_ok() {
        return Err(scan_error(format!(
            "derived reanalysis output already exists: {}",
            target.display()
        )));
    }
    let target_audit = audit_v2_path(&target);
    if std::fs::symlink_metadata(&target_audit).is_ok() {
        return Err(scan_error(format!(
            "derived reanalysis audit output already exists: {}",
            target_audit.display()
        )));
    }
    let parent = target.parent().map(Path::to_path_buf).unwrap_or_default();
    std::fs::create_dir_all(&parent)?;
    let source = open_scan(source_path, false, None)?;
    let temporary = temp_file(&parent)?;
    copy_database(source_path, &temporary)?;
    let lock = WriterLock::acquire(&crate::history::lock_path(&temporary))?;
    let connection = crate::scan_schema::NativeScanSchemaMixin::connect_writer(&temporary)?;
    let mut writer = NativeScan::new(&temporary, connection, Some(lock.into_file()))?;
    prepare(
        &mut writer,
        &source,
        producer_version,
        producer_revision,
        runtime_versions,
    )?;
    Ok(DerivedScan {
        writer,
        source,
        target,
        temporary,
        published: AtomicBool::new(false),
    })
}

fn check_runtime_versions(values: &Map<String, Value>) -> Result<(), StoreError> {
    let complete = RUNTIME_KEYS.len() == values.len()
        && RUNTIME_KEYS.iter().all(|key| {
            values
                .get(*key)
                .and_then(Value::as_str)
                .is_some_and(|v| !v.is_empty())
        });
    if !complete {
        return Err(scan_error(
            "derived reanalysis requires complete runtime version provenance",
        ));
    }
    Ok(())
}

fn temp_file(parent: &Path) -> Result<PathBuf, StoreError> {
    loop {
        let mut name = std::ffi::OsString::from(".reanalysis-");
        name.push(uuid::Uuid::new_v4().simple().to_string());
        name.push(".sqlite");
        let path = parent.join(name);
        if std::fs::symlink_metadata(&path).is_err() {
            return Ok(path);
        }
    }
}

/// Copy the source database content under the private temporary name.
///
/// The copy runs on its own read-only connection: the validated source reader
/// may hold open read cursors, and VACUUM refuses to run inside a transaction.
fn copy_database(source_path: &Path, temporary: &Path) -> Result<(), StoreError> {
    let started = std::time::Instant::now();
    let source =
        Connection::open_with_flags(source_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(StoreError::from)?;
    let target = temporary.to_string_lossy().replace('\'', "''");
    source
        .execute_batch(&format!("VACUUM INTO '{target}'"))
        .map_err(|e| scan_error(format!("derived reanalysis backup failed: {e}")))?;
    if started.elapsed().as_secs() > BACKUP_TIMEOUT_SECONDS {
        return Err(scan_error("derived reanalysis backup timed out"));
    }
    Ok(())
}

/// Rewrite the copied capture header into a derived artifact: a fresh identity,
/// derived source kind, no resumable capability, and full capture provenance.
fn prepare(
    writer: &mut NativeScan,
    source: &Connection,
    producer_version: &str,
    producer_revision: &str,
    runtime_versions: &Map<String, Value>,
) -> Result<(), StoreError> {
    let con = writer.try_con()?;
    let parent = header_row(con)?;
    if !matches!(parent.source_kind.as_str(), "native" | "reanalysis") {
        return Err(scan_error(
            "offline reanalysis requires a native or derived capture source",
        ));
    }
    let audit_sha: Option<String> = source
        .query_row("SELECT sha256 FROM audit WHERE singleton=1", [], |r| {
            r.get(0)
        })
        .optional()
        .map_err(|e| scan_error(format!("audit header is unreadable: {e}")))?;
    let inherited: Option<String> = source
        .query_row(
            "SELECT payload_json FROM context_items WHERE kind='reanalysis_provenance' AND item_key='run'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let mut capture_uuid = parent.scan_uuid.clone();
    let mut capture_writer_version = parent.writer_version.clone();
    let mut capture_writer_revision = parent.writer_revision.clone();
    let mut capture_runtime_versions = parent.runtime_versions_json.clone();
    let mut capture_config_sha256 = sha256_hex(parent.config_json.as_bytes());
    let mut capture_run = json!({
        "lifecycle": parent.lifecycle,
        "finish_reason": parent.finish_reason,
        "crawl_partial": parent.crawl_partial,
        "created_at": parent.created_at,
        "finished_at": parent.finished_at,
    });
    if parent.source_kind == "reanalysis" {
        let payload = inherited
            .as_deref()
            .ok_or_else(|| scan_error("derived source is missing its capture provenance"))?;
        let original: Value = serde_json::from_str(payload)
            .map_err(|e| scan_error(format!("capture provenance payload is invalid: {e}")))?;
        capture_uuid = original["capture_scan_uuid"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        capture_writer_version = original["capture_writer_version"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        capture_writer_revision = original["capture_writer_revision"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        capture_runtime_versions = original["capture_runtime_versions_json"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        capture_config_sha256 = original["capture_config_sha256"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        capture_run = original["capture_run"].clone();
    }
    let mut capabilities: Value = serde_json::from_str(&parent.capabilities_json)
        .map_err(|e| scan_error(format!("capabilities payload is invalid: {e}")))?;
    if let Some(map) = capabilities.as_object_mut() {
        map.insert(
            "resume".into(),
            json!({"state": "unavailable", "reason": "derived artifacts cannot resume collection"}),
        );
        map.insert(
            "offline_reanalysis".into(),
            json!({"state": "complete", "reason": ""}),
        );
    }
    let limitations_raw: Vec<Value> = serde_json::from_str(&parent.limitations_json)
        .map_err(|e| scan_error(format!("limitations payload is invalid: {e}")))?;
    let mut limitations: Vec<Value> = limitations_raw
        .into_iter()
        .map(|value| {
            if value
                == json!("offline reanalysis and browser-network response capture are unavailable")
            {
                json!("browser-network response capture is unavailable")
            } else {
                value
            }
        })
        .collect();
    let pending: bool = con
        .query_row(
            "SELECT 1 FROM frontier WHERE state IN ('queued','inflight') LIMIT 1",
            [],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some();
    let crawl_partial = parent.crawl_partial != 0 || pending;
    let mut capabilities_out = capabilities;
    if pending && parent.crawl_partial == 0 {
        let reason = "source capture has unfinished frontier work";
        limitations.push(json!(reason));
        if let Some(map) = capabilities_out.as_object_mut() {
            for name in ["pages", "links"] {
                let unavailable = map
                    .get(name)
                    .and_then(|c| c.get("state"))
                    .and_then(Value::as_str)
                    == Some("unavailable");
                if !unavailable {
                    map.insert(
                        name.to_string(),
                        json!({"state": "partial", "reason": reason}),
                    );
                }
            }
        }
    }
    let dump = |value: &Value| {
        serde_json::to_string(value)
            .map(|mut text| {
                // Canonical compact form with sorted keys.
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Ok(sorted) = serde_json::to_string(&sorted_keys(&parsed)) {
                        text = sorted;
                    }
                }
                text
            })
            .unwrap_or_default()
    };
    con.execute("BEGIN IMMEDIATE", [])?;
    let now = utc()?;
    let updated = con.execute(
        "UPDATE scan SET scan_uuid=?1,source_kind='reanalysis',parent_scan_uuid=?2,writer_version=?3,\
         writer_revision=?4,runtime_versions_json=?5,created_at=?6,finished_at=NULL,lifecycle='running',\
         finish_reason='',pinned=0,evidence_revision=?7,capabilities_json=?8,limitations_json=?9,crawl_partial=?10 WHERE singleton=1",
        rusqlite::params![
            uuid::Uuid::new_v4().to_string(),
            parent.scan_uuid,
            producer_version,
            producer_revision,
            dump(&Value::Object(runtime_versions.clone())),
            now,
            parent.evidence_revision + 1,
            dump(&capabilities_out),
            dump(&Value::Array(limitations)),
            crawl_partial as i64,
        ],
    );
    let outcome: Result<(), StoreError> = updated.map_err(StoreError::from).and_then(|_| {
        con.execute("DELETE FROM audit", [])?;
        con.execute(
            "DELETE FROM context_items WHERE kind='reanalysis_provenance' AND item_key='run'",
            [],
        )?;
        let provenance = json!({
            "parent_scan_uuid": parent.scan_uuid,
            "capture_scan_uuid": capture_uuid,
            "source_evidence_revision": parent.evidence_revision,
            "derived_evidence_revision": parent.evidence_revision + 1,
            "source_audit_sha256": audit_sha,
            "source_writer_version": parent.writer_version,
            "source_writer_revision": parent.writer_revision,
            "source_runtime_versions_json": parent.runtime_versions_json,
            "source_config_sha256": sha256_hex(parent.config_json.as_bytes()),
            "capture_writer_version": capture_writer_version,
            "capture_writer_revision": capture_writer_revision,
            "capture_runtime_versions_json": capture_runtime_versions,
            "capture_config_sha256": capture_config_sha256,
            "capture_run": capture_run,
        });
        con.execute(
            "INSERT INTO context_items(kind,item_key,payload_version,payload_json,completeness,reason) \
             VALUES('reanalysis_provenance','run','scan_context.v1',?1,'complete','offline reanalysis')",
            rusqlite::params![dump(&provenance)],
        )?;
        Ok(())
    });
    if let Err(error) = outcome {
        let _ = con.execute("ROLLBACK", []);
        return Err(error);
    }
    con.execute("COMMIT", [])?;
    Ok(())
}

/// The `scan` singleton row, read by plain column names.
struct ParentHeader {
    scan_uuid: String,
    source_kind: String,
    writer_version: String,
    writer_revision: String,
    runtime_versions_json: String,
    config_json: String,
    capabilities_json: String,
    limitations_json: String,
    lifecycle: String,
    finish_reason: String,
    crawl_partial: i64,
    created_at: String,
    finished_at: Option<String>,
    evidence_revision: i64,
}

fn header_row(con: &Connection) -> Result<ParentHeader, StoreError> {
    con.query_row(
        "SELECT scan_uuid,source_kind,writer_version,writer_revision,runtime_versions_json,\
         config_json,capabilities_json,limitations_json,lifecycle,finish_reason,crawl_partial,\
         created_at,finished_at,evidence_revision FROM scan WHERE singleton=1",
        [],
        |row| {
            Ok(ParentHeader {
                scan_uuid: row.get(0)?,
                source_kind: row.get(1)?,
                writer_version: row.get(2)?,
                writer_revision: row.get(3)?,
                runtime_versions_json: row.get(4)?,
                config_json: row.get(5)?,
                capabilities_json: row.get(6)?,
                limitations_json: row.get(7)?,
                lifecycle: row.get(8)?,
                finish_reason: row.get(9)?,
                crawl_partial: row.get(10)?,
                created_at: row.get(11)?,
                finished_at: row.get(12)?,
                evidence_revision: row.get(13)?,
            })
        },
    )
    .map_err(|e| scan_error(format!("capture header is unreadable: {e}")))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(bytes);
    digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Recursively sort object keys for the canonical stored payload form.
fn sorted_keys(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: std::collections::BTreeMap<String, Value> = map
                .iter()
                .map(|(key, item)| (key.clone(), sorted_keys(item)))
                .collect();
            let mut out = Map::new();
            for (key, item) in sorted {
                out.insert(key, item);
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(sorted_keys).collect()),
        other => other.clone(),
    }
}

use rusqlite::OptionalExtension;

use std::collections::BTreeMap;

use indexmap::IndexMap;

/// Recorded resource observations keyed by `(kind, url, raw_url)`.
type PriorObservations =
    BTreeMap<(String, String, String), std::collections::VecDeque<(Option<i64>, String, String)>>;

/// One replayed page: the rebuilt page record plus its per-representation
/// observations and evidence facts, keyed by representation name.
pub struct PageReplay {
    /// The rebuilt page record (transport facts from the source row plus every
    /// body-derived field), ready for the pages-row update.
    pub page: Map<String, Value>,
    /// The document the page row points at.
    pub selected_document_id: Option<i64>,
    /// Link edges per representation.
    pub links: BTreeMap<String, Vec<Map<String, Value>>>,
    /// Forms per representation.
    pub forms: BTreeMap<String, Vec<Map<String, Value>>>,
    /// Resource declarations per representation.
    pub resources: BTreeMap<String, Vec<IndexMap<String, Value>>>,
    /// Document facts per representation: document_id, body state and reason,
    /// partial reasons, resource-inventory state, head links.
    pub representation_facts: BTreeMap<String, Map<String, Value>>,
}

/// Replace one page's derived observations with the replayed ones, keeping the
/// page's identity (ordinal) and carrying recorded resource measurements over.
pub fn replace_reparsed_page(
    writer: &mut NativeScan,
    replay: &PageReplay,
) -> Result<(), StoreError> {
    let url = replay
        .page
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let (url_id, page_ordinal, queue_ordinal) = {
        let con = writer.try_con()?;
        con.query_row(
            "SELECT p.url_id,p.page_ordinal,f.queue_ordinal FROM pages p \
             JOIN frontier f USING(url_id) \
             WHERE p.url_id=(SELECT url_id FROM urls WHERE url=?1)",
            rusqlite::params![url],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()
        .map_err(StoreError::from)?
        .ok_or_else(|| scan_error("reanalysis replay page is absent from the derived source copy"))
    }?;
    let lease = crate::scan_lease::Lease {
        url_id,
        url: url.clone(),
        depth: replay
            .page
            .get("crawl_depth")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        queue_ordinal,
    };
    let mut values = writer.page_row(&replay.page, &lease)?;
    values.remove("url_id");
    values.insert("page_ordinal".into(), json!(page_ordinal));
    values.insert(
        "document_id".into(),
        replay
            .selected_document_id
            .map(|id| json!(id))
            .unwrap_or(Value::Null),
    );

    fn apply(
        writer: &NativeScan,
        con: &Connection,
        values: &Map<String, Value>,
        replay: &PageReplay,
        url_id: i64,
        lease: &crate::scan_lease::Lease,
    ) -> Result<(), StoreError> {
        use crate::resources::put_declarations;
        use crate::scan_links::NativeScanGraph;

        let assignments: Vec<String> = values.keys().map(|name| format!("{name}=?")).collect();
        let ordered: Vec<Value> = values.values().cloned().collect();
        con.execute(
            &format!(
                "UPDATE pages SET {} WHERE url_id=?{}",
                assignments.join(","),
                values.len() + 1
            ),
            rusqlite::params_from_iter(
                ordered
                    .iter()
                    .map(json_to_sql)
                    .chain(std::iter::once(rusqlite::types::Value::Integer(url_id))),
            ),
        )
        .map_err(StoreError::from)?;
        for (representation, links) in &replay.links {
            let facts = replay
                .representation_facts
                .get(representation)
                .ok_or_else(|| scan_error("representation facts are absent for stored links"))?;
            let Some(document_id) = facts.get("document_id").and_then(Value::as_i64) else {
                continue;
            };
            con.execute(
                "DELETE FROM links WHERE source_url_id=?1 AND evidence_representation=?2",
                rusqlite::params![url_id, representation],
            )
            .map_err(StoreError::from)?;
            con.execute(
                "DELETE FROM forms WHERE page_url_id=?1 AND evidence_representation=?2",
                rusqlite::params![url_id, representation],
            )
            .map_err(StoreError::from)?;
            let forms = replay
                .forms
                .get(representation)
                .cloned()
                .unwrap_or_default();
            writer.write_observations(lease, Some(document_id), representation, links, &forms)?;
            let head_links = facts.get("head_links").cloned().unwrap_or(Value::Null);
            let parsed = if head_links.is_null() {
                Value::Null
            } else {
                json!({"head_links": head_links})
            };
            let item =
                crate::head_links::context_item(url_id, document_id, representation, &parsed);
            crate::native_context::put_context(
                con,
                item.as_object().expect("context item object"),
                None,
            )?;
            let prior: Vec<(String, String, String, Option<i64>, String, String)> = con
                .prepare(
                    "SELECT r.kind,u.url,r.raw_url,r.response_id,r.capture_state,r.reason \
                     FROM resource_refs r JOIN urls u ON u.url_id=r.resource_url_id \
                     WHERE r.page_url_id=?1 AND r.representation=?2 ORDER BY r.kind,r.ordinal",
                )?
                .query_map(rusqlite::params![url_id, representation], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                })
                .map_err(StoreError::from)?
                .collect::<Result<_, _>>()
                .map_err(StoreError::from)?;
            let declarations = replay
                .resources
                .get(representation)
                .cloned()
                .unwrap_or_default();
            let fetch_enabled: String = con
                .query_row("SELECT config_json FROM scan", [], |row| row.get(0))
                .map_err(StoreError::from)?;
            let fetch_enabled = serde_json::from_str::<Value>(&fetch_enabled)
                .map_err(|e| scan_error(format!("capture config is invalid: {e}")))?
                .get("resources")
                .and_then(|r| r.get("fetch"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let inventory_state = facts
                .get("resource_inventory_state")
                .and_then(Value::as_str)
                .unwrap_or("unavailable")
                .to_owned();
            let omitted = facts
                .get("resource_omitted")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            put_declarations(
                con,
                url_id,
                document_id,
                representation,
                &declarations,
                fetch_enabled,
                &inventory_state,
                omitted,
            )?;
            // Carry recorded HTTP observations over identical (kind, url, raw_url)
            // declarations; a cold not_fetched row resolves from one unambiguous
            // recorded response variant.
            let mut prior_by_key: PriorObservations = BTreeMap::new();
            for row in &prior {
                prior_by_key
                    .entry((row.0.clone(), row.1.clone(), row.2.clone()))
                    .or_default()
                    .push_back((row.3, row.4.clone(), row.5.clone()));
            }
            let mut statement = con.prepare(
                "SELECT r.resource_ref_id,r.kind,u.url,r.raw_url,r.resource_url_id,r.capture_state \
                 FROM resource_refs r JOIN urls u ON u.url_id=r.resource_url_id \
                 WHERE r.page_url_id=?1 AND r.representation=?2 ORDER BY r.kind,r.ordinal",
            )?;
            let rows = statement
                .query_map(rusqlite::params![url_id, representation], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                })
                .map_err(StoreError::from)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(StoreError::from)?;
            drop(statement);
            for (resource_ref_id, kind, url, raw_url, resource_url_id, capture_state) in rows {
                let observation = match prior_by_key
                    .get_mut(&(kind.clone(), url.clone(), raw_url.clone()))
                    .and_then(std::collections::VecDeque::pop_front)
                {
                    Some(saved) => saved,
                    None if capture_state == "not_fetched" => {
                        stored_resource(con, resource_url_id, &kind)?
                    }
                    None => continue,
                };
                con.execute(
                    "UPDATE resource_refs SET response_id=?1,capture_state=?2,reason=?3 WHERE resource_ref_id=?4",
                    rusqlite::params![observation.0, observation.1, observation.2, resource_ref_id],
                )
                .map_err(StoreError::from)?;
            }
            let partial = facts
                .get("partial_reasons")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            writer.partial_reasons(&partial)?;
        }
        Ok(())
    }

    let selected = replay
        .page
        .get("representation")
        .and_then(Value::as_str)
        .unwrap_or("static")
        .to_owned();
    let selected_state = replay
        .representation_facts
        .get(&selected)
        .and_then(|facts| facts.get("resource_inventory_state"))
        .and_then(Value::as_str)
        .unwrap_or("unavailable")
        .to_owned();

    {
        let con = writer.try_con()?;
        con.execute("BEGIN IMMEDIATE", [])?;
        let outcome = apply(writer, con, &values, replay, url_id, &lease);
        if let Err(error) = outcome {
            let _ = con.execute("ROLLBACK", []);
            return Err(error);
        }
        con.execute("COMMIT", [])?;
    }
    // Corpus summaries ride outside the row transaction: they are derived
    // state recomputed from the committed rows on every read.
    writer.sync_corpus(replay.selected_document_id, Some(&selected_state))?;
    Ok(())
}

/// Resolve a cold declaration only from an unambiguous recorded HTTP variant.
fn stored_resource(
    con: &Connection,
    url_id: i64,
    kind: &str,
) -> Result<(Option<i64>, String, String), StoreError> {
    let variants: Vec<String> = con
        .prepare(
            "SELECT DISTINCT variant_key FROM responses WHERE request_url_id=?1 AND purpose=?2 LIMIT 2",
        )?
        .query_map(rusqlite::params![url_id, kind], |row| row.get(0))
        .map_err(StoreError::from)?
        .collect::<Result<_, _>>()
        .map_err(StoreError::from)?;
    if variants.len() != 1 {
        return Ok((
            None,
            "not_fetched".to_owned(),
            if variants.is_empty() {
                "not_in_corpus".to_owned()
            } else {
                "not_in_corpus: ambiguous HTTP variants".to_owned()
            },
        ));
    }
    let response = con
        .query_row(
            "SELECT response_id,body_state,body_reason,effective_status_code FROM responses \
             WHERE request_url_id=?1 AND purpose=?2 AND variant_key=?3 ORDER BY request_ordinal DESC LIMIT 1",
            rusqlite::params![url_id, kind, variants[0]],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            },
        )
        .map_err(StoreError::from)?;
    match response.3 {
        Some(status) if (200..300).contains(&status) => {
            if response.1 == "complete" {
                Ok((response.0, "measured".to_owned(), String::new()))
            } else {
                Ok((response.0, "body_unavailable".to_owned(), response.2))
            }
        }
        _ => Ok((
            response.0,
            "fetch_failed".to_owned(),
            "resource response was not successful".to_owned(),
        )),
    }
}

/// Map one JSON value onto its SQLite storage class for a direct UPDATE.
fn json_to_sql(value: &Value) -> rusqlite::types::Value {
    use rusqlite::types::Value as Sql;
    match value {
        Value::Null => Sql::Null,
        Value::Bool(b) => Sql::Integer(i64::from(*b)),
        Value::Number(n) => n
            .as_i64()
            .map(Sql::Integer)
            .unwrap_or_else(|| Sql::Real(n.as_f64().unwrap_or_default())),
        Value::String(text) => Sql::Text(text.clone()),
        other => Sql::Text(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(result: Result<DerivedScan, StoreError>) -> String {
        match result {
            Ok(_) => panic!("expected an argument rejection"),
            Err(error) => error.to_string(),
        }
    }

    fn versions() -> Map<String, Value> {
        serde_json::from_str(
            r#"{"python":"3","sqlite":"3","httpx":"0","lxml":"5","beautifulsoup4":"4"}"#,
        )
        .unwrap_or_default()
    }

    #[test]
    fn rejects_incomplete_runtime_versions() {
        let map = serde_json::from_str(r#"{"python":"3"}"#).unwrap_or_default();
        assert_eq!(
            message(derived_scan(
                Path::new("/nonexistent-source.sqlite"),
                Path::new("/tmp/unused-out.sqlite"),
                "3.2.0",
                &"a".repeat(40),
                &map,
            )),
            "derived reanalysis requires complete runtime version provenance"
        );
    }

    #[test]
    fn rejects_a_short_producer_revision() {
        assert_eq!(
            message(derived_scan(
                Path::new("/nonexistent-source.sqlite"),
                Path::new("/tmp/unused-out.sqlite"),
                "3.2.0",
                "ZZZ",
                &versions(),
            )),
            "derived reanalysis producer revision must be a lowercase Git SHA"
        );
    }

    #[test]
    fn opens_the_capture_source_only_after_argument_validation() {
        assert!(message(derived_scan(
            Path::new("/nonexistent-source.sqlite"),
            Path::new("/tmp/unused-out.sqlite"),
            "3.2.0",
            &"a".repeat(40),
            &versions(),
        ))
        .starts_with("cannot read scan:"));
    }
}

//! Explicit, injected delivery of retained local monitor notices.
//!
//! There is deliberately no account, endpoint, timer, or transport implementation
//! here. A host that already has an authorized service destination injects the
//! small `send` callable. The local monitor remains quiet unless that host also
//! creates this adapter with `enabled=true` and calls it explicitly.

use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::{value, ProjectsError, ProjectsResult};

const SEND_LEASE_SECONDS: f64 = 300.0;

/// `DeliveryUnavailable`: a request cannot safely be delivered and must not claim success.
fn unavailable(message: &str) -> ProjectsError {
    crate::error::py_named("DeliveryUnavailable", message.to_owned())
}

fn sqlite<T>(result: Result<T, rusqlite::Error>) -> ProjectsResult<T> {
    result.map_err(|e| value(e.to_string()))
}

/// `deliveries` row: `(receipt, state, owner_id, lease_until, retry_authorized)`.
type DeliveryRow = (String, String, Option<String>, Option<f64>, i64);

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or_default()
}

/// Small private receipt store that survives adapter restart and retry.
pub struct DeliveryReceipts {
    /// SQLite path.
    pub path: PathBuf,
    /// `"{pid}:{uuid4hex}"` owner identity for send leases.
    pub owner_id: String,
}

impl DeliveryReceipts {
    /// Open or create the private receipt store at `path`.
    pub fn new(path: &Path) -> ProjectsResult<Self> {
        let receipts = Self {
            path: path.to_path_buf(),
            owner_id: format!("{}:{}", std::process::id(), uuid::Uuid::new_v4().simple()),
        };
        if let Some(parent) = path.parent() {
            #[cfg(unix)]
            {
                use spiderhead_core::filesystem::PrivateMode;
                let mut builder = fs::DirBuilder::new();
                builder.recursive(true).private_mode(0o700);
                builder.create(parent)?;
            }
            #[cfg(not(unix))]
            fs::create_dir_all(parent)?;
        }
        if path.exists() {
            use spiderhead_core::filesystem::StatCompat;
            if path.metadata()?.mode() & 0o077 != 0 {
                return Err(value("delivery receipt store must be private"));
            }
        }
        // Create the file private before SQLite does, so it never exists with wider modes.
        {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            spiderhead_core::filesystem::PrivateMode::private_mode(&mut options, 0o600);
            let _ = options.open(path);
        }
        let con = sqlite(Connection::open(path))?;
        sqlite(con.execute_batch(
            "CREATE TABLE IF NOT EXISTS deliveries (
                job_id TEXT NOT NULL, artifact_id TEXT NOT NULL, destination TEXT NOT NULL,
                receipt TEXT NOT NULL, state TEXT NOT NULL,
                owner_id TEXT, lease_until REAL,
                retry_authorized INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY(job_id, artifact_id, destination)
            )",
        ))?;
        let columns: Vec<String> = {
            let mut stmt = sqlite(con.prepare("PRAGMA table_info(deliveries)"))?;
            let mapped = sqlite(stmt.query_map([], |row| row.get::<_, String>(1)))?;
            mapped.filter_map(|r| r.ok()).collect()
        };
        for (name, declaration) in [
            ("owner_id", "TEXT"),
            ("lease_until", "REAL"),
            ("retry_authorized", "INTEGER NOT NULL DEFAULT 0"),
        ] {
            if !columns.iter().any(|c| c == name) {
                sqlite(con.execute_batch(&format!(
                    "ALTER TABLE deliveries ADD COLUMN {name} {declaration}"
                )))?;
            }
        }
        // Older releases reset every send exception to pending. Those rows have no
        // verification receipt, so treat them as ambiguous rather than replaying them.
        sqlite(con.execute(
            "UPDATE deliveries SET state='uncertain' WHERE state='pending' \
             AND retry_authorized=0",
            [],
        ))?;
        sqlite(con.execute(
            "UPDATE deliveries SET owner_id='legacy:unknown' \
             WHERE state='sending' AND owner_id IS NULL",
            [],
        ))?;
        #[cfg(unix)]
        {
            spiderhead_core::filesystem::set_permissions_mode(path, 0o600)?;
        }
        Ok(receipts)
    }

    /// Claim one delivery; expired sends become uncertain and are never replayed.
    /// Returns `(receipt, state)`.
    pub fn reserve(
        &self,
        job_id: &str,
        artifact_id: &str,
        destination: &str,
    ) -> ProjectsResult<(String, String)> {
        let instant = now();
        let mut con = sqlite(Connection::open(path(self)))?;
        let tx = sqlite(con.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate))?;
        let existing: Option<DeliveryRow> = sqlite(
            tx.query_row(
                "SELECT receipt,state,owner_id,lease_until,retry_authorized FROM deliveries \
                 WHERE job_id=? AND artifact_id=? AND destination=?",
                (job_id, artifact_id, destination),
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional(),
        )?;
        if let Some(existing) = existing {
            match existing.1.as_str() {
                "delivered" => {
                    sqlite(tx.commit())?;
                    return Ok((existing.0, "delivered".into()));
                }
                "sending" => {
                    if existing.3.is_some_and(|until| until > instant) {
                        sqlite(tx.commit())?;
                        return Ok((existing.0, "in_progress".into()));
                    }
                    sqlite(tx.execute(
                        "UPDATE deliveries SET state='uncertain',lease_until=NULL \
                         WHERE job_id=? AND artifact_id=? AND destination=? AND state='sending'",
                        (job_id, artifact_id, destination),
                    ))?;
                    sqlite(tx.commit())?;
                    return Ok((existing.0, "uncertain".into()));
                }
                "uncertain" => {
                    sqlite(tx.commit())?;
                    return Ok((existing.0, "uncertain".into()));
                }
                "pending" if existing.4 == 1 => {
                    sqlite(tx.execute(
                        "UPDATE deliveries SET state='sending',owner_id=?,lease_until=?,\
                         retry_authorized=0 WHERE job_id=? AND artifact_id=? AND destination=?",
                        (
                            self.owner_id.as_str(),
                            instant + SEND_LEASE_SECONDS,
                            job_id,
                            artifact_id,
                            destination,
                        ),
                    ))?;
                    sqlite(tx.commit())?;
                    return Ok((existing.0, "claimed".into()));
                }
                _ => {
                    sqlite(tx.execute(
                        "UPDATE deliveries SET state='uncertain',owner_id=NULL,lease_until=NULL,\
                         retry_authorized=0 WHERE job_id=? AND artifact_id=? AND destination=?",
                        (job_id, artifact_id, destination),
                    ))?;
                    sqlite(tx.commit())?;
                    return Ok((existing.0, "uncertain".into()));
                }
            }
        }
        let receipt = uuid::Uuid::new_v4().simple().to_string();
        sqlite(tx.execute(
            "INSERT INTO deliveries \
             (job_id,artifact_id,destination,receipt,state,owner_id,lease_until,retry_authorized) \
             VALUES(?,?,?,?,?,?,?,0)",
            (
                job_id,
                artifact_id,
                destination,
                receipt.as_str(),
                "sending",
                self.owner_id.as_str(),
                instant + SEND_LEASE_SECONDS,
            ),
        ))?;
        sqlite(tx.commit())?;
        Ok((receipt, "claimed".into()))
    }

    /// Persist an ambiguous send outcome without releasing it for retry.
    pub fn uncertain(
        &self,
        job_id: &str,
        artifact_id: &str,
        destination: &str,
        receipt: &str,
    ) -> ProjectsResult<()> {
        let con = sqlite(Connection::open(path(self)))?;
        sqlite(con.execute(
            "UPDATE deliveries SET state='uncertain',owner_id=NULL,lease_until=NULL, \
             retry_authorized=0 WHERE job_id=? AND artifact_id=? AND destination=? \
             AND receipt=? AND state='sending' AND owner_id=?",
            (
                job_id,
                artifact_id,
                destination,
                receipt,
                self.owner_id.as_str(),
            ),
        ))?;
        Ok(())
    }

    /// Resolve an uncertain receipt after an authoritative provider status check.
    ///
    /// `not_delivered` releases a later retry only when the caller has verified that no
    /// external delivery occurred. When a durable sending owner remains after lease expiry,
    /// the caller must also confirm that the owner process has stopped. `unknown` retains
    /// the uncertain state.
    pub fn reconcile(
        &self,
        receipt: &str,
        provider_status: &str,
        sender_stopped: bool,
    ) -> ProjectsResult<Value> {
        if !["delivered", "not_delivered", "unknown"].contains(&provider_status) {
            return Err(value(
                "provider_status must be delivered, not_delivered, or unknown",
            ));
        }
        if receipt.is_empty() {
            return Err(value("receipt must be non-empty text"));
        }
        let mut con = sqlite(Connection::open(path(self)))?;
        let tx = sqlite(con.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate))?;
        let mut row: (String, Option<f64>, Option<String>) = match sqlite(
            tx.query_row(
                "SELECT state,lease_until,owner_id FROM deliveries WHERE receipt=?",
                (receipt,),
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional(),
        )? {
            None => {
                let _ = tx.rollback();
                return Err(unavailable("delivery receipt was not found"));
            }
            Some(row) => row,
        };
        if row.0 == "sending" {
            if row.1.is_some_and(|until| until > now()) {
                sqlite(tx.commit())?;
                return Ok(json!({"receipt": receipt, "state": "in_progress"}));
            }
            sqlite(tx.execute(
                "UPDATE deliveries SET state='uncertain',lease_until=NULL \
                 WHERE receipt=? AND state='sending'",
                (receipt,),
            ))?;
            row = ("uncertain".into(), None, row.2);
        }
        if row.0 == "delivered" {
            sqlite(tx.commit())?;
            return Ok(json!({"receipt": receipt, "state": "delivered"}));
        }
        if row.0 != "uncertain" {
            let _ = tx.rollback();
            return Err(unavailable("receipt is not awaiting reconciliation"));
        }
        if provider_status == "unknown" {
            sqlite(tx.commit())?;
            return Ok(json!({"receipt": receipt, "state": "uncertain"}));
        }
        if provider_status == "not_delivered" && row.2.is_some() && !sender_stopped {
            sqlite(tx.commit())?;
            return Ok(json!({
                "receipt": receipt,
                "state": "uncertain",
                "reason": "the expired sending owner must be confirmed stopped before retry",
            }));
        }
        let state = if provider_status == "delivered" {
            "delivered"
        } else {
            "pending"
        };
        sqlite(tx.execute(
            "UPDATE deliveries SET state=?,owner_id=NULL,lease_until=NULL,retry_authorized=? \
             WHERE receipt=? AND state='uncertain'",
            (state, (provider_status == "not_delivered") as i64, receipt),
        ))?;
        sqlite(tx.commit())?;
        Ok(json!({"receipt": receipt, "state": state}))
    }

    /// Mark the claimed send as delivered; stale claims cannot confirm delivery.
    pub fn delivered(
        &self,
        job_id: &str,
        artifact_id: &str,
        destination: &str,
        receipt: &str,
    ) -> ProjectsResult<()> {
        let con = sqlite(Connection::open(path(self)))?;
        let changed = sqlite(con.execute(
            "UPDATE deliveries SET state='delivered',owner_id=NULL,lease_until=NULL \
             WHERE job_id=? AND artifact_id=? AND destination=? AND receipt=? \
             AND state='sending' AND owner_id=?",
            (
                job_id,
                artifact_id,
                destination,
                receipt,
                self.owner_id.as_str(),
            ),
        ))?;
        if changed == 1 {
            return Ok(());
        }
        let existing: Option<String> = sqlite(
            con.query_row(
                "SELECT state FROM deliveries WHERE job_id=? AND artifact_id=? \
                 AND destination=? AND receipt=?",
                (job_id, artifact_id, destination, receipt),
                |row| row.get(0),
            )
            .optional(),
        )?;
        if existing.as_deref() != Some("delivered") {
            return Err(unavailable("delivery receipt changed while sending"));
        }
        Ok(())
    }
}

fn path(receipts: &DeliveryReceipts) -> &Path {
    &receipts.path
}

/// Canonical compact JSON text for a payload.
fn canonical(value: &Value) -> String {
    crate::pyjson::dumps_compact(value)
}

/// The injected send callable: `(destination, payload, receipt)` — any failure means
/// the outcome is uncertain, never silently delivered.
pub type SendFn = dyn Fn(&str, &Value, &str) -> Result<(), String> + Send + Sync;

/// Send retained actionable/recovery notices once through an authorized adapter.
pub struct MonitorServiceDelivery<'a> {
    /// Project identity sent in every payload.
    pub project_uuid: String,
    /// The injected send callable.
    pub send: &'a SendFn,
    /// Whether delivery is enabled at all.
    pub enabled: bool,
    /// Whether recovery events are delivered too.
    pub include_recoveries: bool,
    /// The receipt store.
    pub receipts: &'a DeliveryReceipts,
    destinations: std::collections::BTreeSet<String>,
}

impl<'a> MonitorServiceDelivery<'a> {
    /// Build the adapter; identity and destinations are validated on construction.
    pub fn new(
        project_uuid: &str,
        allowed_destinations: &[String],
        receipts: &'a DeliveryReceipts,
        send: &'a SendFn,
        enabled: bool,
        include_recoveries: bool,
    ) -> ProjectsResult<Self> {
        let adapter = Self {
            project_uuid: project_uuid.to_owned(),
            send,
            enabled,
            include_recoveries,
            receipts,
            destinations: allowed_destinations.iter().cloned().collect(),
        };
        if adapter.project_uuid.is_empty() || adapter.project_uuid.chars().count() > 256 {
            return Err(value("monitor delivery requires a project identity"));
        }
        if adapter.destinations.is_empty() || adapter.destinations.iter().any(|d| d.is_empty()) {
            return Err(value("monitor delivery requires explicit destinations"));
        }
        Ok(adapter)
    }

    fn events(run: &Value, include_recoveries: bool) -> Vec<Value> {
        let mut events: Vec<Value> = run
            .get("alerts")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|alert| {
                json!({
                    "kind": "alert",
                    "url": alert.get("url").cloned().unwrap_or(Value::Null),
                    "changes": alert.get("changes").cloned().unwrap_or(Value::Null),
                })
            })
            .collect();
        if include_recoveries {
            for recovery in run
                .get("recoveries")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                events.push(json!({
                    "kind": "recovery",
                    "url": recovery.get("url").cloned().unwrap_or(Value::Null),
                    "change": recovery.get("change").cloned().unwrap_or(Value::Null),
                }));
            }
        }
        events
    }

    /// Deliver only retained, complete alert events and return durable receipts.
    pub fn deliver(&self, run: &Value, destination: &str) -> ProjectsResult<Value> {
        if !self.enabled {
            return Err(unavailable("monitor service delivery is disabled"));
        }
        if !self.destinations.contains(destination) {
            return Err(ProjectsError::Io(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "destination is not authorized for monitor delivery",
            )));
        }
        let coverage = run.get("coverage");
        if matches!(
            run.get("state").and_then(Value::as_str),
            Some("partial" | "failed")
        ) || (coverage.is_some_and(|c| c.is_object())
            && !coverage
                .and_then(|c| c.get("complete"))
                .is_some_and(|v| !crate::schedule::falsy(v)))
        {
            return Err(unavailable(
                "partial monitor evidence is retained but is not deliverable",
            ));
        }
        let scan_id = run.get("scan_id").and_then(Value::as_str);
        if scan_id.is_none_or(|id| id.is_empty()) {
            return Err(unavailable("monitor run lacks retained scan identity"));
        }
        let scan_id = scan_id.unwrap_or_default();
        let mut receipts: Vec<Value> = Vec::new();
        for event in Self::events(run, self.include_recoveries) {
            let payload = json!({
                "project_uuid": self.project_uuid,
                "scan_id": scan_id,
                "event": event,
            });
            let artifact_id = format!("{:x}", Sha256::digest(canonical(&payload).as_bytes()));
            let (receipt, state) =
                self.receipts
                    .reserve(&self.project_uuid, &artifact_id, destination)?;
            let kind = event["kind"].clone();
            if state == "delivered" {
                receipts.push(json!({
                    "receipt": receipt, "state": "delivered", "kind": kind,
                }));
                continue;
            }
            if state == "in_progress" || state == "uncertain" {
                receipts.push(json!({
                    "receipt": receipt, "state": state, "kind": kind,
                }));
                return Ok(json!({
                    "ok": false,
                    "delivery": state,
                    "receipts": receipts,
                    "action_required": if state == "in_progress" {
                        "wait for the active sender"
                    } else {
                        "verify the destination by receipt; reconcile as delivered or \
                         not_delivered before retry; an expired owner lease also requires \
                         sender_stopped=True"
                    },
                }));
            }
            let sent = (self.send)(destination, &payload, &receipt).and_then(|()| {
                self.receipts
                    .delivered(&self.project_uuid, &artifact_id, destination, &receipt)
                    .map_err(|e| e.to_string())
            });
            match sent {
                Ok(()) => {
                    receipts.push(json!({
                        "receipt": receipt, "state": "sent", "kind": kind,
                    }));
                }
                Err(_) => {
                    let _ = self.receipts.uncertain(
                        &self.project_uuid,
                        &artifact_id,
                        destination,
                        &receipt,
                    );
                    receipts.push(json!({
                        "receipt": receipt, "state": "uncertain", "kind": kind,
                    }));
                    return Ok(json!({
                        "ok": false,
                        "delivery": "uncertain",
                        "receipts": receipts,
                        "action_required": "verify the destination by receipt; reconcile as \
                             delivered or not_delivered before retry; an expired owner lease \
                             also requires sender_stopped=True",
                    }));
                }
            }
        }
        Ok(json!({
            "ok": true,
            "delivery": if receipts.is_empty() { "quiet" } else { "sent" },
            "receipts": receipts,
        }))
    }
}

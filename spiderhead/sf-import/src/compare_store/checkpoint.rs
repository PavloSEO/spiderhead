//! Comparison resume state: the SQLite job-state key/value helpers every phase
//! transition commits through, and the removal of a published job's
//! now-redundant work cache.

use std::os::unix::fs::MetadataExt;
use std::path::Path;

use rusqlite::Connection;
use serde_json::Value;

use super::shared::{py_json, LockFile};
use crate::compare::base::CompareError;

/// Read one checkpoint value, or `default` when the key is absent.
pub(crate) fn state_value(con: &Connection, key: &str) -> Result<Option<Value>, CompareError> {
    let mut statement = con
        .prepare("SELECT value FROM job_state WHERE key=?1")
        .map_err(|e| CompareError(e.to_string()))?;
    let mut rows = statement
        .query(rusqlite::params![key])
        .map_err(|e| CompareError(e.to_string()))?;
    match rows.next().map_err(|e| CompareError(e.to_string()))? {
        Some(row) => {
            let text: String = row.get(0).map_err(|e| CompareError(e.to_string()))?;
            let value = serde_json::from_str(&text).map_err(|e| CompareError(e.to_string()))?;
            Ok(Some(value))
        }
        None => Ok(None),
    }
}

/// Read one checkpoint value with a fallback used when the key is absent.
pub(crate) fn state_value_or(
    con: &Connection,
    key: &str,
    default: Value,
) -> Result<Value, CompareError> {
    Ok(state_value(con, key)?.unwrap_or(default))
}

/// Upsert one checkpoint value; callers commit alongside their batch writes so
/// a crash leaves the recorded cursor consistent with the stored rows.
pub(crate) fn set_state(con: &Connection, key: &str, value: &Value) -> Result<(), CompareError> {
    con.execute(
        "INSERT INTO job_state(key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        rusqlite::params![key, py_json(value)],
    )
    .map_err(|e| CompareError(e.to_string()))?;
    Ok(())
}

/// End the caller's transaction and immediately open the next one, so the
/// durable job always sits inside an explicit transaction like the oracle's
/// connection with its implicit-transaction isolation level.
pub(crate) fn commit_txn(con: &Connection) -> Result<(), CompareError> {
    if !con.is_autocommit() {
        con.execute_batch("COMMIT")
            .map_err(|e| CompareError(e.to_string()))?;
    }
    con.execute_batch("BEGIN")
        .map_err(|e| CompareError(e.to_string()))?;
    Ok(())
}

/// Remove a published job's now-redundant work cache when no writer holds it.
/// Every refusal path is silent: the cache is a convenience, not evidence.
pub(crate) fn discard_completed_checkpoint(work: &Path, job_id: &str) {
    if work.is_symlink() || !work.is_dir() {
        return;
    }
    let Ok(info) = std::fs::symlink_metadata(work) else {
        return;
    };
    let Ok(job_text) = std::fs::read_to_string(work.join("job.json")) else {
        return;
    };
    let Ok(job) = serde_json::from_str::<Value>(&job_text) else {
        return;
    };
    if job.get("job_id").and_then(Value::as_str) != Some(job_id) {
        return;
    }
    let lock_path = work.join(".writer.lock");
    let Ok(lock) = LockFile::open(&lock_path, false) else {
        return;
    };
    if lock.try_lock_exclusive().is_err() {
        return;
    }
    lock.unlock();
    drop(lock);
    let Ok(current) = std::fs::symlink_metadata(work) else {
        return;
    };
    if info.dev() == current.dev() && info.ino() == current.ino() {
        let _ = std::fs::remove_dir_all(work);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn memory_db() -> Connection {
        let con = Connection::open_in_memory().expect("memory db");
        con.execute_batch("CREATE TABLE job_state(key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .expect("schema");
        con
    }

    #[test]
    fn state_roundtrip_and_default() {
        let con = memory_db();
        assert_eq!(state_value(&con, "missing").expect("read"), None);
        set_state(&con, "phase", &json!("classify_before")).expect("write");
        set_state(&con, "count", &json!(42)).expect("write");
        set_state(&con, "phase", &json!("export")).expect("overwrite");
        assert_eq!(
            state_value_or(&con, "phase", json!("none")).expect("read"),
            json!("export")
        );
        assert_eq!(
            state_value_or(&con, "count", json!(0)).expect("read"),
            json!(42)
        );
        assert_eq!(
            state_value_or(&con, "absent", json!(0)).expect("read"),
            json!(0)
        );
    }

    #[test]
    fn discard_completed_checkpoint_refuses_foreign_jobs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).expect("mkdir");
        std::fs::write(
            work.join("job.json"),
            serde_json::to_string(&json!({"job_id": "other"})).expect("json"),
        )
        .expect("write");
        discard_completed_checkpoint(&work, "target");
        assert!(work.is_dir(), "foreign job cache must survive");
    }
}

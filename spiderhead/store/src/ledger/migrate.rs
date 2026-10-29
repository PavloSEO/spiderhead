//! Ledger migrations v0..v5 and the validated open path.
//!
//! `_migrate*`, `_MIGRATIONS`, `_migrate` and `open_ledger`.

use std::collections::BTreeSet;
use std::path::Path;

use rusqlite::Connection;

use crate::error::StoreError;
use crate::scan_artifact::loads;

use super::schema::*;

/// One legacy `finding_group` row: scan id, ref, check, value, count, urls JSON.
type GroupRow = (i64, String, String, Option<String>, Option<i64>, String);

/// Complete a staged `ledger.v0` stub into the full v1 schema.
///
/// `ledger.v0` is a bootstrap stub format an interrupted or hand-staged creation may leave: the
/// `SEOL` application id, a `user_version` of 0 and a one-row `ledger_meta` marker naming the
/// intended ledger UUID, its project/site binding and the writer that staged it.
pub fn migrate_0_to_1(con: &Connection) -> Result<(), StoreError> {
    let meta = {
        let mut stmt = con
            .prepare("SELECT format_version, ledger_uuid, created_at, writer_version, writer_revision, project_uuid, site_target FROM ledger_meta WHERE singleton=1")
            .map_err(|_| ledger_error("ledger.v0 stub is missing its ledger_meta marker"))?;
        let mut rows = stmt.query([])?;
        let row = rows
            .next()?
            .ok_or_else(|| ledger_error("ledger.v0 marker does not name ledger.v0"))?;
        let format: String = row.get(0)?;
        if format != "ledger.v0" {
            return Err(ledger_error("ledger.v0 marker does not name ledger.v0"));
        }
        let get = |i: usize| -> rusqlite::Result<String> { row.get(i) };
        (get(1)?, get(2)?, get(3)?, get(4)?, get(5)?, get(6)?)
    };
    con.execute_batch("DROP TABLE ledger_meta")?;
    for statement in ddl_statements() {
        con.execute_batch(&statement)?;
    }
    con.execute_batch("PRAGMA user_version=1")?;
    con.execute(
        "INSERT INTO ledger VALUES(1,?,?,?,?,?,?)",
        rusqlite::params![meta.0, "ledger.v1", meta.1, meta.2, meta.3, 0],
    )?;
    super::ingest::register_site(con, &meta.4, &meta.5, "primary", Some(&meta.1))?;
    Ok(())
}

/// Add immutable verification artifact/result bindings to a v1 ledger.
pub fn migrate_1_to_2(con: &Connection) -> Result<(), StoreError> {
    apply_v2_schema(con)?;
    con.execute(
        "UPDATE ledger SET format_version=? WHERE singleton=1",
        [FORMAT_VERSION],
    )?;
    con.execute_batch("PRAGMA user_version=2")?;
    Ok(())
}

/// Persist whether group members were fully retained by the source audit.
pub fn migrate_2_to_3(con: &Connection) -> Result<(), StoreError> {
    apply_v3_schema(con)?;
    // v2 did not retain this state for streaming scan.v2 audit companions. A migration cannot
    // reconstruct missing group members from their old ledger rows, so mark their full scope
    // unavailable rather than backfilling a deceptively complete denominator.
    con.execute(
        "UPDATE source_scan SET group_memberships_state='unavailable' WHERE format_version='scan.v2'",
        [],
    )?;
    con.execute_batch("PRAGMA user_version=3")?;
    Ok(())
}

/// Reserve canonical audit identities without inventing them for old rows.
pub fn migrate_3_to_4(con: &Connection) -> Result<(), StoreError> {
    apply_v4_schema(con)?;
    con.execute_batch("PRAGMA user_version=4")?;
    Ok(())
}

/// Retain each ordered source group once; reject inconsistent old copies.
pub fn migrate_4_to_5(con: &Connection) -> Result<(), StoreError> {
    apply_v5_schema(con)?;
    let rows: Vec<GroupRow> = {
        let mut stmt = con.prepare(
            "SELECT source_scan_id,group_ref,group_check,group_value,group_count,group_urls_json FROM finding_group ORDER BY source_scan_id,group_ref",
        )?;
        let mapped = stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?;
        mapped.collect::<Result<_, _>>()?
    };
    for (scan_id, group_ref, group_check, group_value, group_count, urls_json) in rows {
        let group = serde_json::json!({
            "group_id": group_ref,
            "check": group_check,
            "value": group_value,
            "count": group_count,
            "urls": loads(&urls_json, "group members")?,
        });
        let (_created, complete) = super::ingest::store_source_group(con, scan_id, &group, false)?;
        if !complete {
            con.execute(
                "UPDATE source_scan SET group_memberships_state='partial' WHERE source_scan_id=? AND group_memberships_state='complete'",
                [scan_id],
            )?;
        }
    }
    con.execute("UPDATE finding_group SET group_urls_json='[]'", [])?;
    con.execute_batch("PRAGMA user_version=5")?;
    Ok(())
}

fn user_version(con: &Connection) -> Result<i64, StoreError> {
    Ok(con.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

/// Apply known migrations inside the caller's write transaction.
///
/// Only a write-mode open reaches here: reading never mutates the file, an unknown/newer version
/// was refused before this point, and a raised migration error leaves the caller's transaction to
/// roll the file back unchanged.
pub fn migrate(con: &Connection) -> Result<(), StoreError> {
    let mut version = user_version(con)?;
    if !(0..=USER_VERSION).contains(&version) {
        return Err(ledger_error(format!(
            "unsupported ledger user_version {version}"
        )));
    }
    while version < USER_VERSION {
        match version {
            0 => migrate_0_to_1(con)?,
            1 => migrate_1_to_2(con)?,
            2 => migrate_2_to_3(con)?,
            3 => migrate_3_to_4(con)?,
            4 => migrate_4_to_5(con)?,
            _ => {
                return Err(ledger_error(format!(
                    "no ledger migration path from version {version}"
                )))
            }
        }
        let next = user_version(con)?;
        if next != version + 1 {
            return Err(ledger_error(format!(
                "ledger migration {version} did not advance the version"
            )));
        }
        version = next;
    }
    Ok(())
}

/// Return a validated ledger connection; the caller must drop it.
///
/// A read open never mutates the artifact. A write open is the explicit operation allowed to
/// migrate a known older version, inside one transaction; a newer/unknown version refuses
/// without touching the file.
pub fn open_ledger(path: &Path, write: bool) -> Result<Connection, StoreError> {
    runtime()?;
    let outcome = open_checked(path, write);
    match outcome {
        Err(StoreError::LedgerError(e)) => Err(StoreError::LedgerError(e)),
        Err(StoreError::Sqlite(e)) => Err(ledger_error(format!("cannot read ledger: {e}"))),
        Err(StoreError::Io(e)) => Err(ledger_error(format!("cannot read ledger: {e}"))),
        other => other,
    }
}

fn open_checked(path: &Path, write: bool) -> Result<Connection, StoreError> {
    let con = if write { writer(path)? } else { reader(path)? };
    let app_id: i64 = con.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    if app_id != APPLICATION_ID {
        return Err(ledger_error(format!(
            "foreign application_id {app_id}; expected {APPLICATION_ID} (SEOL)"
        )));
    }
    let version = user_version(&con)?;
    if write {
        if !(0..=USER_VERSION).contains(&version) {
            return Err(ledger_error(format!(
                "unsupported ledger user_version {version}; this build knows {USER_VERSION}"
            )));
        }
        if version != USER_VERSION {
            con.execute_batch("BEGIN IMMEDIATE")?;
            // Validation stays inside the transaction: a migration that produced an
            // inconsistent ledger rolls back untouched.
            let outcome = migrate(&con).and_then(|()| validate(&con));
            return match outcome {
                Ok(()) => {
                    con.execute_batch("COMMIT")?;
                    Ok(con)
                }
                Err(e) => {
                    let _ = con.execute_batch("ROLLBACK");
                    Err(e)
                }
            };
        }
    } else if version != USER_VERSION {
        return Err(ledger_error(format!(
            "unsupported ledger user_version {version}; this build knows {USER_VERSION}"
        )));
    }
    validate(&con)?;
    Ok(con)
}

/// Lifecycle states as a set, for membership checks.
pub fn lifecycle_set() -> BTreeSet<&'static str> {
    LIFECYCLE_STATES.iter().copied().collect()
}

/// Allowed next states of `state`.
pub fn transitions_from(state: &str) -> &'static [&'static str] {
    TRANSITIONS
        .iter()
        .find(|(s, _)| *s == state)
        .map_or(&[], |(_, t)| *t)
}

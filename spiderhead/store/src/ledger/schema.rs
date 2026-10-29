//! Ledger artifact format: DDL resources, migrations, validation and access.
//!
//! The bottom layer of the ledger package: it defines what a `ledger.v1` artifact is (the header
//! pragmas, the exact object inventory a valid database must match, and the additive v2..v5
//! migration path) and provides the only supported way to hold a connection ([`open_ledger`]).
//! The stable identity helpers ([`canonical_url`], [`finding_key`], [`occurrence_key`]) live here
//! because both other layers key rows through them.

use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use regex::Regex;
use rusqlite::{params_from_iter, Connection, OpenFlags};

use sha2::{Digest, Sha256};

use super::urlparts::urlsplit;
use crate::error::StoreError;
use crate::scan_artifact::loads;
use crate::schema_sql::{
    LEDGER_V1_SQL, LEDGER_V2_SQL, LEDGER_V3_SQL, LEDGER_V4_SQL, LEDGER_V5_SQL,
};

/// Package that carries the DDL resources.
pub const SQL_PACKAGE: &str = "spiderhead.storage";
/// `PRAGMA application_id` of a ledger: ASCII `SEOL`.
pub const APPLICATION_ID: i64 = 1397051212;
/// Current `PRAGMA user_version`.
pub const USER_VERSION: i64 = 5;
/// Format tag stored in the header row.
pub const FORMAT_VERSION: &str = "ledger.v1";
/// Read budget in seconds.
pub const READ_TIMEOUT_SECONDS: i64 = 30;
/// Largest stored JSON payload.
pub const MAX_PAYLOAD_BYTES: i64 = 8388608;
/// Domain tag of the finding key.
pub const KEY_FINDING: &str = "spiderhead.ledger-finding.v1";
/// Domain tag of the occurrence key.
pub const KEY_OCCURRENCE: &str = "spiderhead.ledger-occurrence.v1";
/// Regex source of a SHA-256 digest (full match).
pub const SHA256: &str = r"[0-9a-f]{64}\Z";
/// Regex source of a configuration fingerprint (full match).
pub const FINGERPRINT: &str = r"[0-9a-f]{16}\Z";
/// Regex source of a full lowercase Git commit SHA (full match).
pub const REVISION: &str = r"[0-9a-f]{40}\Z";

fn full_match(cell: &'static OnceLock<Regex>, body: &str, text: &str) -> bool {
    cell.get_or_init(|| Regex::new(&format!("^{body}$")).expect("static pattern"))
        .is_match(text)
}

/// True for a lowercase hex SHA-256.
pub fn is_sha256(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    full_match(&RE, "[0-9a-f]{64}", text)
}

/// True for a 16-digit lowercase hex fingerprint.
pub fn is_fingerprint(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    full_match(&RE, "[0-9a-f]{16}", text)
}

/// True for a full lowercase Git commit SHA.
pub fn is_revision(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    full_match(&RE, "[0-9a-f]{40}", text)
}

/// Lifecycle states, sorted for a stable order.
pub const LIFECYCLE_STATES: &[&str] = &[
    "detected",
    "false_positive_reviewed",
    "fix_reported",
    "persisting",
    "recheck_pending",
    "regressed",
    "resolved",
    "unverifiable",
    "verified",
];

/// Allowed lifecycle transitions per current state.
pub const TRANSITIONS: &[(&str, &[&str])] = &[
    (
        "detected",
        &[
            "false_positive_reviewed",
            "fix_reported",
            "unverifiable",
            "verified",
        ],
    ),
    (
        "verified",
        &[
            "false_positive_reviewed",
            "fix_reported",
            "recheck_pending",
            "unverifiable",
        ],
    ),
    (
        "fix_reported",
        &["recheck_pending", "unverifiable", "verified"],
    ),
    (
        "recheck_pending",
        &["persisting", "regressed", "resolved", "unverifiable"],
    ),
    ("resolved", &["recheck_pending", "regressed"]),
    (
        "persisting",
        &[
            "fix_reported",
            "recheck_pending",
            "regressed",
            "unverifiable",
        ],
    ),
    ("false_positive_reviewed", &["recheck_pending", "verified"]),
    (
        "unverifiable",
        &["fix_reported", "recheck_pending", "verified"],
    ),
    (
        "regressed",
        &["fix_reported", "recheck_pending", "unverifiable"],
    ),
];

/// States only a measured verification may set.
pub const MEASURED_OUTCOMES: &[&str] = &["persisting", "regressed", "resolved"];

/// The input cannot be used as a supported, consistent ledger artifact.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct LedgerError {
    /// Human-readable reason.
    pub message: String,
}

/// A [`LedgerError`] as a [`StoreError`].
pub fn ledger_error(message: impl Into<String>) -> StoreError {
    LedgerError {
        message: message.into(),
    }
    .into()
}

/// Refuse SQLite builds older than 3.31.
pub fn runtime() -> Result<(), StoreError> {
    if rusqlite::version_number() < 3_031_000 {
        return Err(ledger_error(format!(
            "ledger.v1 requires SQLite >= 3.31; this build uses {}",
            rusqlite::version()
        )));
    }
    Ok(())
}

/// The ledger.v1 bootstrap DDL.
pub fn schema() -> String {
    LEDGER_V1_SQL.to_string()
}

/// The additive v2 evidence binding, kept separate from the v1 bootstrap.
///
/// Keeping this delta separate lets an existing ledger.v1 migrate in one transaction while a
/// fresh ledger is built from the identical sequence. `sqlite_master` is deliberately compared
/// after applying this delta, so a hand-added evidence table is never silently accepted.
pub fn v2_schema() -> String {
    LEDGER_V2_SQL.to_string()
}

/// The additive source-scope state used by remediation coverage.
pub fn v3_schema() -> String {
    LEDGER_V3_SQL.to_string()
}

/// Add canonical audit identity for safely bridged legacy audit documents.
pub fn v4_schema() -> String {
    LEDGER_V4_SQL.to_string()
}

/// Run every `;`-separated statement of `sql` that has a first non-comment line,
/// matching `ddl_statements`' notion of a real statement: a piece led by comment
/// lines still runs when DDL follows them.
fn apply_pieces(con: &Connection, sql: &str, _skip_comment_led: bool) -> Result<(), StoreError> {
    for piece in sql.split(';') {
        let statement = piece
            .lines()
            .find(|line| !line.trim().is_empty() && !line.trim().starts_with("--"));
        if statement.is_none() {
            continue;
        }
        con.execute_batch(piece)?;
    }
    Ok(())
}

/// Apply the v1 -> v2 DDL one statement at a time inside a transaction.
pub fn apply_v2_schema(con: &Connection) -> Result<(), StoreError> {
    apply_pieces(con, &v2_schema(), true)
}

/// Apply the v2 -> v3 DDL.
pub fn apply_v3_schema(con: &Connection) -> Result<(), StoreError> {
    apply_pieces(con, &v3_schema(), true)
}

/// Apply the v3 -> v4 DDL.
pub fn apply_v4_schema(con: &Connection) -> Result<(), StoreError> {
    apply_pieces(con, &v4_schema(), true)
}

/// Apply the v4 -> v5 DDL.
pub fn apply_v5_schema(con: &Connection) -> Result<(), StoreError> {
    apply_pieces(con, LEDGER_V5_SQL, false)
}

/// Schema statements minus PRAGMAs, for transactional migration.
///
/// `executescript` commits any pending transaction first, so a migration that must roll back on
/// failure applies the DDL one statement at a time instead. Each piece is kept verbatim:
/// `sqlite_master.sql` stores embedded comments, so the migrated schema must textually match a
/// direct script run.
pub fn ddl_statements() -> Vec<String> {
    let mut statements = Vec::new();
    for piece in LEDGER_V1_SQL.split(';') {
        let first = piece
            .lines()
            .find(|line| !line.trim().is_empty() && !line.trim().starts_with("--"));
        if let Some(line) = first {
            if !line.trim().to_uppercase().starts_with("PRAGMA") {
                statements.push(piece.to_string());
            }
        }
    }
    statements
}

/// One `sqlite_master` entry: type, name, table name and DDL text.
pub type ObjectRow = (String, String, String, Option<String>);

/// Every schema object except SQLite's own, ordered by type and name.
pub fn objects(con: &Connection) -> Result<Vec<ObjectRow>, StoreError> {
    let mut stmt = con.prepare(
        "SELECT type, name, tbl_name, sql FROM sqlite_master WHERE name NOT GLOB 'sqlite_*' ORDER BY type, name",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The object inventory a valid ledger must match (v1 plus the v2..v5 deltas).
pub fn expected() -> Result<&'static Vec<ObjectRow>, StoreError> {
    static CELL: OnceLock<Vec<ObjectRow>> = OnceLock::new();
    if let Some(v) = CELL.get() {
        return Ok(v);
    }
    runtime()?;
    let con = Connection::open_in_memory()?;
    con.execute_batch(LEDGER_V1_SQL)?;
    apply_v2_schema(&con)?;
    apply_v3_schema(&con)?;
    apply_v4_schema(&con)?;
    apply_v5_schema(&con)?;
    let built = objects(&con)?;
    Ok(CELL.get_or_init(|| built))
}

/// SHA-256 of the compact JSON list of `parts` (non-ASCII kept).
pub fn key(parts: &[&str]) -> String {
    let encoded = serde_json::to_string(parts).unwrap_or_default();
    Sha256::digest(encoded.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Canonicalize an affected URL with the audit's own equality policy.
///
/// This delegates to `norm_url` (strip, lowercase scheme and host, drop trailing slash), the
/// same rule the analyzer uses for URL identity, so a recheck reproduces the same key without a
/// second normalization contract to drift. Query and fragment are preserved.
pub fn canonical_url(value: &str) -> Result<String, StoreError> {
    if value.trim().is_empty() {
        return Err(ledger_error("affected URL must be a nonempty string"));
    }
    let normalized = spiderhead_core::url_normalize::norm_url(Some(value))
        .map_err(|_| ledger_error("affected URL must be a nonempty string"))?;
    if normalized.is_empty() {
        return Err(ledger_error("affected URL must be a nonempty string"));
    }
    if normalized.chars().count() > 2048 {
        return Err(ledger_error(
            "affected URL exceeds the 2048-character bound",
        ));
    }
    Ok(normalized)
}

/// Stable logical identity of a check/problem at project/site scope.
pub fn finding_key(
    project_uuid: &str,
    site_host: &str,
    check: &str,
    subject_type: &str,
    subject_value: &str,
) -> String {
    key(&[
        KEY_FINDING,
        project_uuid,
        site_host,
        check,
        subject_type,
        subject_value,
    ])
}

/// Stable, versioned identity of one affected case.
///
/// Deliberately excludes severity, message, HTTP status, evidence value or hash, configuration,
/// producer build, timestamps, the run-local audit ordinal and the scan UUID: those describe an
/// *observation* of the case, never the case itself.
#[allow(clippy::too_many_arguments)]
pub fn occurrence_key(
    project_uuid: &str,
    site_host: &str,
    check: &str,
    subject_type: &str,
    subject_value: &str,
    representation: &str,
    discriminator_type: &str,
    discriminator_value: &str,
) -> String {
    key(&[
        KEY_OCCURRENCE,
        project_uuid,
        site_host,
        check,
        subject_type,
        subject_value,
        representation,
        discriminator_type,
        discriminator_value,
    ])
}

fn exists_row(con: &Connection, sql: &str, params: &[String]) -> Result<bool, StoreError> {
    let mut stmt = con.prepare(sql)?;
    let mut rows = stmt.query(params_from_iter(params.iter()))?;
    Ok(rows.next()?.is_some())
}

fn state_marks() -> (String, Vec<String>) {
    (
        vec!["?"; LIFECYCLE_STATES.len()].join(","),
        LIFECYCLE_STATES.iter().map(|s| s.to_string()).collect(),
    )
}

fn check_groups(con: &Connection) -> Result<(), StoreError> {
    let mut groups = con.prepare(
        "SELECT source_scan_id, group_ref, member_count, members_sha256 FROM source_group",
    )?;
    let mut rows = groups.query([])?;
    while let Some(group) = rows.next()? {
        let (scan_id, group_ref): (i64, String) = (group.get(0)?, group.get(1)?);
        let (expected_count, expected_sha): (i64, String) = (group.get(2)?, group.get(3)?);
        let mut digest = Sha256::new();
        let mut count: i64 = 0;
        let mut members = con.prepare(
            "SELECT ordinal,value_json FROM source_group_member WHERE source_scan_id=? AND group_ref=? ORDER BY ordinal",
        )?;
        let mut member_rows = members.query(rusqlite::params![scan_id, group_ref])?;
        while let Some(member) = member_rows.next()? {
            let ordinal: i64 = member.get(0)?;
            let text: String = member.get(1)?;
            if ordinal != count {
                return Err(ledger_error(
                    "source group members are incomplete or unordered",
                ));
            }
            let encoded = text.as_bytes();
            if encoded.len() as i64 > MAX_PAYLOAD_BYTES {
                return Err(ledger_error(
                    "source group member exceeds the ledger payload bound",
                ));
            }
            loads(&text, "source group member")?;
            digest.update((encoded.len() as u64).to_be_bytes());
            digest.update(encoded);
            count += 1;
        }
        let hex: String = digest
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if (count, hex) != (expected_count, expected_sha) {
            return Err(ledger_error("source group member count or digest differs"));
        }
    }
    Ok(())
}

/// Validate a ledger connection: object inventory, integrity, identity keys and bindings.
pub fn validate(con: &Connection) -> Result<(), StoreError> {
    if &objects(con)? != expected()? {
        return Err(ledger_error(
            "ledger schema differs: missing/changed tables, indexes or unexpected schema objects",
        ));
    }
    let quick: String = con.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    if quick != "ok" {
        return Err(ledger_error("ledger database failed quick_check"));
    }
    if exists_row(con, "PRAGMA foreign_key_check", &[])? {
        return Err(ledger_error(
            "ledger database has inconsistent foreign-key references",
        ));
    }
    check_groups(con)?;
    let headers: Vec<(String, String, String, i64)> = {
        let mut stmt = con.prepare(
            "SELECT format_version, ledger_uuid, writer_revision, ledger_revision FROM ledger",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        rows.collect::<Result<_, _>>()?
    };
    if headers.len() != 1 {
        return Err(ledger_error("ledger.v1 requires exactly one header"));
    }
    let (format_version, ledger_uuid, writer_revision, ledger_revision) = &headers[0];
    if format_version != FORMAT_VERSION {
        return Err(ledger_error("unsupported ledger format_version"));
    }
    if uuid::Uuid::parse_str(ledger_uuid).is_err() {
        return Err(ledger_error("invalid ledger UUID"));
    }
    if !is_revision(writer_revision) {
        return Err(ledger_error(
            "ledger writer build must be a full lowercase Git commit SHA",
        ));
    }
    if *ledger_revision < 0 {
        return Err(ledger_error("invalid ledger revision"));
    }
    let (marks, states) = state_marks();
    if exists_row(
        con,
        &format!("SELECT 1 FROM finding WHERE current_state NOT IN ({marks}) LIMIT 1"),
        &states,
    )? || exists_row(
        con,
        &format!("SELECT 1 FROM occurrence WHERE current_state NOT IN ({marks}) LIMIT 1"),
        &states,
    )? {
        return Err(ledger_error(
            "ledger contains an unsupported lifecycle state",
        ));
    }
    if exists_row(
        con,
        &format!(
            "SELECT 1 FROM decision WHERE state NOT IN ({marks}) OR actor='' OR reason='' OR decided_at_state!='known' OR decided_at IS NULL LIMIT 1"
        ),
        &states,
    )? {
        return Err(ledger_error("ledger contains an invalid lifecycle decision"));
    }
    if exists_row(
        con,
        "SELECT 1 FROM decision d LEFT JOIN verification_result vr ON vr.verification_result_id=d.verification_result_id WHERE d.verification_result_id IS NOT NULL AND (vr.verification_result_id IS NULL OR vr.outcome!=d.state OR (d.state IN ('resolved','persisting','regressed') AND vr.measured!=1)) LIMIT 1",
        &[],
    )? {
        return Err(ledger_error("ledger verification decision is missing measured result evidence"));
    }
    {
        let mut stmt = con.prepare("SELECT project_uuid, netloc, target FROM site")?;
        let mut rows = stmt.query([])?;
        while let Some(site) = rows.next()? {
            let (project_uuid, netloc, target): (String, String, String) =
                (site.get(0)?, site.get(1)?, site.get(2)?);
            if uuid::Uuid::parse_str(&project_uuid).is_err() {
                return Err(ledger_error("site row has an invalid project UUID"));
            }
            if netloc != urlsplit(&target).netloc.to_lowercase() {
                return Err(ledger_error("site row identity is inconsistent"));
            }
        }
    }
    {
        let mut stmt = con.prepare(
            "SELECT f.finding_key,f.subject_type,f.subject_value,c.check_key,s.project_uuid,s.host FROM finding f JOIN check_def c ON c.check_id=f.check_id JOIN site s ON s.site_id=f.site_id",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let stored: String = r.get(0)?;
            let want = finding_key(
                &r.get::<_, String>(4)?,
                &r.get::<_, String>(5)?,
                &r.get::<_, String>(3)?,
                &r.get::<_, String>(1)?,
                &r.get::<_, String>(2)?,
            );
            if stored != want {
                return Err(ledger_error(
                    "finding identity does not match its stored key",
                ));
            }
        }
    }
    {
        let mut stmt = con.prepare(
            "SELECT o.occurrence_key,o.subject_type,o.subject_value,o.representation,o.discriminator_type,o.discriminator_value,c.check_key,s.project_uuid,s.host FROM occurrence o JOIN check_def c ON c.check_id=o.check_id JOIN finding f ON f.finding_id=o.finding_id JOIN site s ON s.site_id=f.site_id",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let stored: String = r.get(0)?;
            let want = occurrence_key(
                &r.get::<_, String>(7)?,
                &r.get::<_, String>(8)?,
                &r.get::<_, String>(6)?,
                &r.get::<_, String>(1)?,
                &r.get::<_, String>(2)?,
                &r.get::<_, String>(3)?,
                &r.get::<_, String>(4)?,
                &r.get::<_, String>(5)?,
            );
            if stored != want {
                return Err(ledger_error(
                    "occurrence identity does not match its stored key",
                ));
            }
        }
    }
    if exists_row(
        con,
        "SELECT 1 FROM source_scan WHERE NOT (audit_sha256 GLOB '[0-9a-f][0-9a-f]*' AND length(audit_sha256)=64) OR NOT (scan_sha256 GLOB '[0-9a-f][0-9a-f]*' AND length(scan_sha256)=64) LIMIT 1",
        &[],
    )? {
        return Err(ledger_error("source scan binding has an invalid digest"));
    }
    if exists_row(
        con,
        "SELECT 1 FROM source_scan WHERE canonical_audit_sha256 IS NOT NULL AND (canonical_audit_sha256 GLOB '*[^0-9a-f]*' OR length(canonical_audit_sha256)!=64) LIMIT 1",
        &[],
    )? {
        return Err(ledger_error("source scan has an invalid canonical audit digest"));
    }
    if exists_row(
        con,
        "SELECT 1 FROM source_scan WHERE group_memberships_state NOT IN ('complete','partial','unavailable') LIMIT 1",
        &[],
    )? {
        return Err(ledger_error("source scan has an invalid group-membership scope state"));
    }
    if exists_row(
        con,
        "SELECT 1 FROM observation WHERE (observed_at IS NULL) != (observed_at_state='unknown') LIMIT 1",
        &[],
    )? {
        return Err(ledger_error("observation time state disagrees with its stored value"));
    }
    Ok(())
}

/// Open a ledger read-only (query-only, foreign keys on).
pub fn reader(path: &Path) -> Result<Connection, StoreError> {
    let resolved = std::fs::canonicalize(path)?;
    Ok(spiderhead_core::sqlite::open_readonly(
        &resolved,
        Duration::from_secs(5),
        &[
            "PRAGMA trusted_schema=OFF",
            "PRAGMA query_only=ON",
            "PRAGMA foreign_keys=ON",
        ],
    )?)
}

/// Open an existing ledger for writing (never creates the file).
pub fn writer(path: &Path) -> Result<Connection, StoreError> {
    let absolute = std::path::absolute(path)?;
    let con = Connection::open_with_flags(
        &absolute,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    con.busy_timeout(Duration::from_secs(5))?;
    con.execute_batch(
        "PRAGMA trusted_schema=OFF; PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL",
    )?;
    Ok(con)
}

pub use super::migrate::{
    lifecycle_set, migrate, migrate_0_to_1, migrate_1_to_2, migrate_2_to_3, migrate_3_to_4,
    migrate_4_to_5, open_ledger, transitions_from,
};

#[cfg(test)]
mod tests {
    use super::*;

    // A piece led by comment lines must still run its DDL.
    #[test]
    fn comment_led_piece_still_runs() {
        let con = Connection::open_in_memory().unwrap();
        apply_pieces(&con, "-- migrates things\nCREATE TABLE t(x);", true).unwrap();
        let n: i64 = con
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='t'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
        // A piece that is only comments stays skipped.
        apply_pieces(&con, "-- nothing else", true).unwrap();
    }
}

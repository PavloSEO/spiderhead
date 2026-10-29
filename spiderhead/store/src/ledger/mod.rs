//! Versioned remediation ledger: stable finding/occurrence identity over saved scans.
//!
//! The ledger is a separate local SQLite artifact (`ledger.v1`) bound to a project/site identity.
//! It ingests validated `scan.v1`/`scan.v2` artifacts read-only and records which findings were
//! observed, where each occurrence lives, and every later observation of the same case.
//!
//! Identity rules (docs/LEDGER.md):
//!
//! * a *finding* is the logical problem at project/site scope: one check at one subject (a
//!   canonical URL, or the explicit `scope:site` subject for an audit-wide finding);
//! * an *occurrence* is where that problem manifests: one check at one affected URL or non-URL
//!   subject, under one representation, with a typed discriminator separating firings on the same
//!   URL;
//! * *affected URLs* are finding membership, counted separately so findings, occurrences and
//!   distinct affected URLs never collapse into one number;
//! * an *observation* is one immutable sighting of an occurrence bound to an exact source scan
//!   revision. Re-ingesting the same source revision is idempotent; a new revision appends.
//!
//! The package is split by concern: `schema` (artifact format, migrations, access), `ingest`
//! (registration, ingest, lifecycle decisions) and `queries` (summaries, case pages, reports).

pub mod ingest;
pub mod lifecycle;
pub mod migrate;
pub mod queries;
pub mod reports;
pub mod rows;
pub mod schema;
pub mod specs;
pub mod urlparts;

use std::path::Path;

use rusqlite::Connection;

use crate::error::StoreError;

/// A ledger given either as a path (opened for this call) or as an already open connection.
#[derive(Clone, Copy)]
pub enum LedgerRef<'a> {
    /// Path of a ledger file.
    Path(&'a Path),
    /// An open, validated connection.
    Conn(&'a Connection),
}

impl<'a> From<&'a Path> for LedgerRef<'a> {
    fn from(path: &'a Path) -> Self {
        LedgerRef::Path(path)
    }
}

impl<'a> From<&'a Connection> for LedgerRef<'a> {
    fn from(con: &'a Connection) -> Self {
        LedgerRef::Conn(con)
    }
}

/// A connection that is either owned (closed on drop) or borrowed.
pub enum Handle<'a> {
    /// Opened here.
    Owned(Connection),
    /// Supplied by the caller.
    Borrowed(&'a Connection),
}

impl std::ops::Deref for Handle<'_> {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        match self {
            Handle::Owned(c) => c,
            Handle::Borrowed(c) => c,
        }
    }
}

impl<'a> LedgerRef<'a> {
    /// Open (or borrow) the connection; `write` selects the migrating writer for a path.
    pub fn open(self, write: bool) -> Result<Handle<'a>, StoreError> {
        match self {
            LedgerRef::Path(p) => Ok(Handle::Owned(schema::open_ledger(p, write)?)),
            LedgerRef::Conn(c) => Ok(Handle::Borrowed(c)),
        }
    }
}

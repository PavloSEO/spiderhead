//! Durable verdict cache for multi-step bot verification.
//!
//! The retained artifact's schema is fixed, so verdicts live in a sidecar SQLite file
//! (`<source>.verify`) next to it. A verdict row is keyed by `(bot, ip, method)` and
//! carries the evidence origin (`ip-list`, `rdns+forward`, `asn`), a timestamp and its
//! own TTL. A second run of the same store serves fresh verdicts without touching the
//! network; an interrupted run resumes from the rows already persisted.
//!
//! Only decisive outcomes are ever stored (`verified`, `spoofed`, `ptr_failed`):
//! `unverified`, `unsupported` and environment failures are state, not evidence. A
//! missing or unreadable cache is a cache miss, never an error of the run.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::error::Result;

/// Seconds a stored verdict stays fresh; one day bounds how stale a changed range
/// list or a moved crawler can leave the cache.
pub const VERDICT_TTL: f64 = 86_400.0;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS verdicts(
 bot TEXT NOT NULL,ip TEXT NOT NULL,method TEXT NOT NULL,
 status TEXT NOT NULL,hostname TEXT,detail TEXT NOT NULL,
 source TEXT NOT NULL,checked_at REAL NOT NULL,ttl REAL NOT NULL,
 PRIMARY KEY(bot,ip,method));
";

/// Where `source`'s sidecar lives.
pub fn path_for(source: &str) -> PathBuf {
    PathBuf::from(format!("{source}.verify"))
}

/// A stored verdict row.
#[derive(Debug, Clone)]
pub struct Row {
    pub bot: String,
    pub ip: String,
    /// Step that decided (`ip_list`, `dns_reverse_forward`, `asn`).
    pub method: String,
    /// Status name (`verified`, `spoofed`, `ptr_failed`).
    pub status: String,
    pub hostname: Option<String>,
    pub detail: String,
    /// Evidence origin (`ip-list`, `rdns+forward`, `asn`).
    pub source: String,
    pub checked_at: f64,
    pub ttl: f64,
}

impl Row {
    /// JSON form for diagnostics.
    pub fn to_json(&self) -> Value {
        json!({
            "bot": self.bot, "ip": self.ip, "method": self.method,
            "status": self.status, "hostname": self.hostname,
            "detail": self.detail, "source": self.source,
            "checked_at": self.checked_at, "ttl": self.ttl,
        })
    }
}

/// The sidecar database of one artifact.
pub struct VerdictCache {
    pub path: PathBuf,
}

fn connect(path: &Path) -> Result<Connection> {
    let con = Connection::open(path)?;
    con.execute_batch(SCHEMA)?;
    Ok(con)
}

impl VerdictCache {
    /// The cache of `source` (`<source>.verify`).
    pub fn for_source(source: &str) -> Self {
        Self {
            path: path_for(source),
        }
    }

    /// Fresh verdict rows keyed by `(bot, ip)`; the newest row wins when several
    /// methods judged the same pair.
    pub fn fresh(&self, now: f64) -> Result<Vec<Row>> {
        let con = connect(&self.path)?;
        let mut stmt = con.prepare(
            "SELECT bot,ip,method,status,hostname,detail,source,checked_at,ttl \
             FROM verdicts WHERE ?-checked_at<ttl",
        )?;
        let rows = stmt.query_map(params![now], |r| {
            Ok(Row {
                bot: r.get(0)?,
                ip: r.get(1)?,
                method: r.get(2)?,
                status: r.get(3)?,
                hostname: r.get(4)?,
                detail: r.get(5)?,
                source: r.get(6)?,
                checked_at: r.get(7)?,
                ttl: r.get(8)?,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }

    /// Upsert verdict rows; a newer row replaces the stored one for its key.
    pub fn store(&self, rows: &[Row], now: f64) -> Result<usize> {
        if rows.is_empty() {
            return Ok(0);
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let con = connect(&self.path)?;
        let mut insert = con.prepare(
            "INSERT INTO verdicts VALUES(?,?,?,?,?,?,?,?,?) \
             ON CONFLICT(bot,ip,method) DO UPDATE SET \
             status=excluded.status,hostname=excluded.hostname,detail=excluded.detail, \
             source=excluded.source,checked_at=excluded.checked_at,ttl=excluded.ttl",
        )?;
        let mut written = 0usize;
        for row in rows {
            insert.execute(params![
                row.bot,
                row.ip,
                row.method,
                row.status,
                row.hostname,
                row.detail,
                row.source,
                now,
                row.ttl,
            ])?;
            written += 1;
        }
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(status: &str) -> Row {
        Row {
            bot: "Googlebot".into(),
            ip: "66.249.66.1".into(),
            method: "dns_reverse_forward".into(),
            status: status.into(),
            hostname: Some("crawl-66-249-66-1.googlebot.com".into()),
            detail: "PTR and forward resolution confirm the same IP".into(),
            source: "rdns+forward".into(),
            checked_at: 0.0,
            ttl: VERDICT_TTL,
        }
    }

    #[test]
    fn a_fresh_verdict_round_trips_and_an_old_one_expires() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("logs.sqlite");
        std::fs::write(&source, b"").unwrap();
        let cache = VerdictCache::for_source(source.to_str().unwrap());
        assert!(cache.fresh(100.0).unwrap().is_empty());
        cache.store(&[row("verified")], 100.0).unwrap();
        let hits = cache.fresh(200.0).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].status, "verified");
        assert_eq!(hits[0].method, "dns_reverse_forward");
        assert!(cache.fresh(100.0 + VERDICT_TTL + 1.0).unwrap().is_empty());
        cache.store(&[row("spoofed")], 200.0).unwrap();
        assert_eq!(cache.fresh(300.0).unwrap()[0].status, "spoofed");
    }
}

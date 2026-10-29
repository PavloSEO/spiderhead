//! `_DiskIssues` + `_DiskIssueResults`: pre-aggregation findings and final
//! active/suppressed result rows kept off the heap.

use rusqlite::{params, Connection};
use serde_json::Value;
use spiderhead_formats::models::Issue;
use std::path::PathBuf;

use super::{
    dedupe_merge, issue_from_state, issue_state, normalize_slice, temp_sqlite, unlink, DiskError,
};
use crate::normalize::Object;

/// `_DiskIssues`: ordered pre-aggregation findings kept off the heap.
pub struct DiskIssues {
    path: PathBuf,
    con: Connection,
    next_ordinal: i64,
    closed: bool,
}

fn severity_rank(severity: &str) -> i64 {
    match severity {
        "critical" => 0,
        "warning" => 1,
        "notice" => 2,
        _ => 3,
    }
}

impl DiskIssues {
    /// Create the backing store.
    pub fn new() -> Result<Self, DiskError> {
        let (path, con) = temp_sqlite("seohead-audit-issues-")?;
        con.execute_batch(
            "CREATE TABLE issues (ordinal INTEGER PRIMARY KEY, check_id TEXT NOT NULL, \
             target_url TEXT, target_sort TEXT NOT NULL, severity_rank INTEGER NOT NULL, \
             value_json TEXT NOT NULL);\
             CREATE INDEX issues_order ON issues(severity_rank, check_id, target_sort, ordinal)",
        )?;
        Ok(Self {
            path,
            con,
            next_ordinal: 0,
            closed: false,
        })
    }

    /// `issues.append(issue)` — `str(issue.target_url)` for `target_sort`.
    pub fn append(&mut self, issue: &Issue) -> Result<(), DiskError> {
        let ordinal = self.next_ordinal;
        self.next_ordinal += 1;
        let target_sort = match &issue.target_url {
            Some(u) => u.clone(),
            None => "None".to_owned(),
        };
        self.con.execute(
            "INSERT INTO issues VALUES (?,?,?,?,?,?)",
            params![
                ordinal,
                issue.check,
                issue.target_url,
                target_sort,
                severity_rank(&issue.severity),
                serde_json::to_string(&issue_state(issue))?,
            ],
        )?;
        Ok(())
    }

    /// Number of rows.
    pub fn len(&self) -> i64 {
        self.con
            .query_row("SELECT COUNT(*) FROM issues", [], |r| r.get(0))
            .unwrap_or(0)
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Iterate in insertion order.
    pub fn iter(&self) -> Vec<Issue> {
        let mut stmt = match self
            .con
            .prepare("SELECT value_json FROM issues ORDER BY ordinal")
        {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map([], |r| r.get::<_, String>(0))
            .map(|rows| {
                rows.filter_map(Result::ok)
                    .filter_map(|t| serde_json::from_str::<Object>(&t).ok())
                    .map(|o| issue_from_state(&o))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `remove_check`: delete every row of one check id.
    pub fn remove_check(&self, check_id: &str) {
        let _ = self
            .con
            .execute("DELETE FROM issues WHERE check_id=?", params![check_id]);
    }

    /// `has_check`: whether a row exists for the check id.
    pub fn has_check(&self, check_id: &str) -> bool {
        self.con
            .query_row(
                "SELECT 1 FROM issues WHERE check_id=? LIMIT 1",
                params![check_id],
                |_| Ok(()),
            )
            .is_ok()
    }

    /// `iter_deduped_sorted`: severity -> check -> target (plus the image
    /// source/scope JSON keys) order, merging consecutive same-key issues
    /// that carry a target URL.
    pub fn iter_deduped_sorted(&self) -> Vec<Issue> {
        let mut stmt = match self.con.prepare(
            "SELECT value_json FROM issues ORDER BY severity_rank, check_id, target_sort, \
             CASE WHEN check_id GLOB 'IMG_*' AND json_type(value_json,'$.details.source_page')='text' \
             THEN json_extract(value_json,'$.details.source_page') ELSE '' END, \
             CASE WHEN check_id GLOB 'IMG_*' AND json_type(value_json,'$.details.source_page')='text' \
             AND json_type(value_json,'$.details.target_scope')='text' \
             THEN COALESCE(json_extract(value_json,'$.details.target_scope'),'') ELSE '' END, ordinal",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let issues: Vec<Issue> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map(|rows| {
                rows.filter_map(Result::ok)
                    .filter_map(|t| serde_json::from_str::<Object>(&t).ok())
                    .map(|o| issue_from_state(&o))
                    .collect()
            })
            .unwrap_or_default();
        dedupe_merge(issues)
    }

    /// Close and unlink.
    pub fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        unlink(&self.path);
    }
}

impl Drop for DiskIssues {
    fn drop(&mut self) {
        self.close();
    }
}

/// `_DiskIssueResults`: final active and suppressed findings plus the
/// implausible-target ledger, all as sqlite rows.
pub struct DiskIssueResults {
    path: PathBuf,
    con: Connection,
    active_ordinal: i64,
    suppressed_ordinal: i64,
    closed: bool,
}

impl DiskIssueResults {
    /// Create the backing store.
    pub fn new() -> Result<Self, DiskError> {
        let (path, con) = temp_sqlite("seohead-audit-final-findings-")?;
        con.execute_batch(
            "CREATE TABLE active (ordinal INTEGER PRIMARY KEY, check_id TEXT NOT NULL, \
             severity TEXT NOT NULL, target_url TEXT, value_json TEXT NOT NULL);\
             CREATE TABLE suppressed (ordinal INTEGER PRIMARY KEY, check_id TEXT NOT NULL, \
             severity TEXT NOT NULL, target_url TEXT, value_json TEXT NOT NULL);\
             CREATE TABLE implausible_targets (check_id TEXT NOT NULL, target_url TEXT NOT NULL, \
             PRIMARY KEY(check_id, target_url)) WITHOUT ROWID",
        )?;
        Ok(Self {
            path,
            con,
            active_ordinal: 0,
            suppressed_ordinal: 0,
            closed: false,
        })
    }

    /// `append_active(issue)`.
    pub fn append_active(&mut self, issue: &Issue) -> Result<(), DiskError> {
        self.con.execute(
            "INSERT INTO active VALUES (?,?,?,?,?)",
            params![
                self.active_ordinal,
                issue.check,
                issue.severity,
                issue.target_url,
                serde_json::to_string(&issue_state(issue))?,
            ],
        )?;
        self.active_ordinal += 1;
        Ok(())
    }

    /// `append_suppressed(issue)` — an arbitrary finding mapping.
    pub fn append_suppressed(&mut self, issue: &Object) -> Result<(), DiskError> {
        let check = issue
            .get("check")
            .map(crate::pyvalue::str_or_dumps)
            .unwrap_or_default();
        let severity = issue
            .get("severity")
            .map(crate::pyvalue::str_or_dumps)
            .unwrap_or_default();
        // reference binds the raw value: str -> TEXT, number -> INT/REAL,
        // bool -> INT, None -> NULL (containers would raise and are bound
        // as NULL here instead).
        let target = match issue.get("target_url") {
            Some(Value::String(s)) => rusqlite::types::Value::Text(s.clone()),
            Some(Value::Number(n)) => n.as_i64().map_or_else(
                || rusqlite::types::Value::Real(n.as_f64().unwrap_or(0.0)),
                rusqlite::types::Value::Integer,
            ),
            Some(Value::Bool(b)) => rusqlite::types::Value::Integer(i64::from(*b)),
            _ => rusqlite::types::Value::Null,
        };
        self.con.execute(
            "INSERT INTO suppressed VALUES (?,?,?,?,?)",
            params![
                self.suppressed_ordinal,
                check,
                severity,
                target,
                serde_json::to_string(issue)?,
            ],
        )?;
        self.suppressed_ordinal += 1;
        Ok(())
    }

    /// `remember_implausible_targets`: record the page-level targets a
    /// non-image check claimed — withheld-finding support for aggregation.
    pub fn remember_implausible_targets(&mut self, issue: &Issue, image_targeted: bool) {
        if image_targeted {
            return;
        }
        let mut targets: Vec<String> = Vec::new();
        if let Some(t) = issue.target_url.as_ref().filter(|t| !t.is_empty()) {
            targets.push(t.clone());
        }
        for loc in &issue.locations {
            if let Some(u) = loc.get("url").filter(|u| crate::pyvalue::truthy(u)) {
                let s = crate::pyvalue::str_or_dumps(u);
                if !targets.contains(&s) {
                    targets.push(s);
                }
            }
        }
        for target in targets {
            let _ = self.con.execute(
                "INSERT OR IGNORE INTO implausible_targets VALUES (?,?)",
                params![issue.check, target],
            );
        }
    }

    /// Iterate active findings in insertion order.
    pub fn iter(&self) -> Vec<Issue> {
        let mut stmt = match self
            .con
            .prepare("SELECT value_json FROM active ORDER BY ordinal")
        {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map([], |r| r.get::<_, String>(0))
            .map(|rows| {
                rows.filter_map(Result::ok)
                    .filter_map(|t| serde_json::from_str::<Object>(&t).ok())
                    .map(|o| issue_from_state(&o))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Active row count.
    pub fn len(&self) -> i64 {
        self.active_ordinal
    }

    /// Whether there are no active rows.
    pub fn is_empty(&self) -> bool {
        self.active_ordinal == 0
    }

    /// `results[i]` on active findings — the row at the raw ordinal; misses
    /// (including negatives) surface as `None` like the reference `IndexError`.
    pub fn index(&self, index: i64) -> Option<Issue> {
        self.con
            .query_row(
                "SELECT value_json FROM active WHERE ordinal=?",
                params![index],
                |r| r.get::<_, String>(0),
            )
            .ok()
            .and_then(|t| serde_json::from_str::<Object>(&t).ok())
            .map(|o| issue_from_state(&o))
    }

    /// `results[start:stop]` for step-1 slices (`.indices(len)` normalized).
    pub fn slice(&self, start: i64, stop: i64) -> Vec<Issue> {
        let (s, e) = normalize_slice(start, stop, self.len());
        let mut stmt = match self.con.prepare(
            "SELECT value_json FROM active WHERE ordinal>=? AND ordinal<? ORDER BY ordinal",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![s, e], |r| r.get::<_, String>(0))
            .map(|rows| {
                rows.filter_map(Result::ok)
                    .filter_map(|t| serde_json::from_str::<Object>(&t).ok())
                    .map(|o| issue_from_state(&o))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `iter_suppressed` — suppressed finding mappings in insertion order.
    pub fn iter_suppressed(&self) -> Vec<Object> {
        let mut stmt = match self
            .con
            .prepare("SELECT value_json FROM suppressed ORDER BY ordinal")
        {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map([], |r| r.get::<_, String>(0))
            .map(|rows| {
                rows.filter_map(Result::ok)
                    .filter_map(|t| serde_json::from_str::<Object>(&t).ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Suppressed row count.
    pub fn suppressed_len(&self) -> i64 {
        self.suppressed_ordinal
    }

    /// `suppressed[i]` — the row at the raw ordinal.
    pub fn suppressed_index(&self, index: i64) -> Option<Object> {
        self.con
            .query_row(
                "SELECT value_json FROM suppressed WHERE ordinal=?",
                params![index],
                |r| r.get::<_, String>(0),
            )
            .ok()
            .and_then(|t| serde_json::from_str::<Object>(&t).ok())
    }

    /// `suppressed[start:stop]` for step-1 slices.
    pub fn suppressed_slice(&self, start: i64, stop: i64) -> Vec<Object> {
        let (s, e) = normalize_slice(start, stop, self.suppressed_len());
        let mut stmt = match self.con.prepare(
            "SELECT value_json FROM suppressed WHERE ordinal>=? AND ordinal<? ORDER BY ordinal",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![s, e], |r| r.get::<_, String>(0))
            .map(|rows| {
                rows.filter_map(Result::ok)
                    .filter_map(|t| serde_json::from_str::<Object>(&t).ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `implausible_counts`: `(check_id, count)` pairs.
    pub fn implausible_counts(&self) -> Vec<(String, i64)> {
        let mut stmt = match self
            .con
            .prepare("SELECT check_id, COUNT(*) FROM implausible_targets GROUP BY check_id")
        {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    /// Close and unlink.
    pub fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        unlink(&self.path);
    }
}

impl Drop for DiskIssueResults {
    fn drop(&mut self) {
        self.close();
    }
}

//! An opened `scan.v1` the URL table and details panel read through.
//!
//! reference reference: `_open` in the reference suite (identity and
//! schema checks) plus `issue_selection.materialize` (temp tables the issue
//! filter and the `issue_count` column read). The audit side streams through
//! `spiderhead_store::audit_v2::AuditV2Reader`; the file stays read-only and only
//! the temp database is written.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use spiderhead_store::audit_v2::{audit_v2_path, AuditV2Reader};

use super::columns::{required_page_columns, PAGE_BUDGET_SECONDS, PROGRESS_OPS};
use super::filters::QueryError;

/// Anything the data layer can fail with; the command surface serializes it.
#[derive(Debug, thiserror::Error)]
pub enum DataError {
    /// Query/reason-code error (invalid input, unsupported schema, …).
    #[error("{0}")]
    Query(#[from] QueryError),
    /// Store-layer error (audit reader, light opener).
    #[error("{0}")]
    Store(#[from] spiderhead_store::error::StoreError),
    /// Raw SQLite error.
    #[error("{0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Issue-selection failure distinguishing unmeasured checks from missing audits.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct IssueSelectionError {
    /// reference-style reason: `issue_check_unmeasured` | `issue_index_missing`.
    pub reason_code: &'static str,
    /// Detail naming the check(s) or the failed reader.
    pub message: String,
}

/// An open read-only scan plus the lazily built issue selection.
pub struct ScanSession {
    /// Read-only connection (light open); the UI thread owns it.
    pub con: Connection,
    /// Canonical path the session was opened on.
    pub path: PathBuf,
    /// `source` object of the reference query result.
    pub source: Map<String, Value>,
    /// True when the `pages_status` index exists (status_code sorts indexed).
    pub status_index: bool,
    /// Fingerprint of the last issue materialization: (checks, severity).
    issue_state: Option<(BTreeSet<String>, Option<String>)>,
    /// Audit companion missing → issue columns are unavailable, not zero.
    audit_missing: bool,
}

impl ScanSession {
    /// Short human name of the scan: its folder, or the file stem when the
    /// file has a distinctive name (`scan.sqlite` itself says nothing).
    pub fn display_name(&self) -> String {
        let stem = self.path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let folder = self
            .path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("");
        if stem == "scan" && !folder.is_empty() {
            folder.to_string()
        } else {
            stem.to_string()
        }
    }

    /// Open and check identity/schema generation without validating contents.
    pub fn open(path: &Path) -> Result<Self, DataError> {
        if !path.is_file() {
            return Err(QueryError::unavailable("cannot_open", "scan file does not exist").into());
        }
        // `mode=ro` keeps the main schema read-only while temp tables (the issue
        // materialization) still work — `SQLITE_OPEN_READ_ONLY` would block them.
        // Identity comes from pragmas: the store's strict opener also runs
        // scan-config credential checks that belong to a later slice.
        let uri = format!("{}?mode=ro", file_uri(path));
        let con = rusqlite::Connection::open_with_flags(
            &uri,
            rusqlite::OpenFlags::SQLITE_OPEN_URI | rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
        )
        .map_err(|e| QueryError::unavailable("cannot_open", e.to_string()))?;
        let app_id: i64 = con.query_row("PRAGMA application_id", [], |r| r.get(0))?;
        if app_id != 1_397_051_208 {
            return Err(QueryError::unavailable(
                "not_a_scan",
                "file is not a SpiderHead scan (application_id)",
            )
            .into());
        }
        let version: i64 = con.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if !(1..=2).contains(&version) {
            return Err(QueryError::invalid(
                "unsupported_schema",
                format!("unsupported scan user_version {version}"),
            )
            .into());
        }
        let row = con
            .query_row(
                "SELECT scan_uuid,format_version,source_kind,evidence_revision,lifecycle,
                    finish_reason,crawl_partial,corpus_partial,capabilities_json
             FROM scan WHERE singleton=1",
                [],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, i64>(6)?,
                        r.get::<_, i64>(7)?,
                        r.get::<_, String>(8)?,
                    ))
                },
            )
            .map_err(|e| {
                DataError::Query(QueryError::unavailable(
                    "not_a_scan",
                    format!("file is not a readable SpiderHead scan: {e}"),
                ))
            })?;
        let expected = format!("scan.v{version}");
        if row.1 != expected {
            return Err(QueryError::invalid(
                "unsupported_schema",
                "scan header disagrees with its schema version",
            )
            .into());
        }
        let have: BTreeSet<String> = {
            let mut st = con.prepare("PRAGMA table_info(pages)")?;
            let rows = st.query_map([], |r| r.get::<_, String>(1))?;
            rows.collect::<Result<_, _>>()?
        };
        let has_urls = con
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='urls'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .optional()
            .unwrap_or_default()
            .is_some();
        let required: BTreeSet<&str> = required_page_columns().into_iter().collect();
        if !required.is_subset(&have.iter().map(String::as_str).collect()) || !has_urls {
            return Err(QueryError::invalid(
                "unsupported_schema",
                "scan pages table lacks required columns",
            )
            .into());
        }
        let status_index = con
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='index' AND name='pages_status'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .optional()
            .unwrap_or_default()
            .is_some();
        let mut source = Map::new();
        source.insert("scan_uuid".into(), json!(row.0));
        source.insert("format_version".into(), json!(row.1));
        source.insert("source_kind".into(), json!(row.2));
        source.insert("evidence_revision".into(), json!(row.3));
        source.insert("lifecycle".into(), json!(row.4));
        source.insert("finish_reason".into(), json!(row.5));
        source.insert("crawl_partial".into(), json!(row.6 != 0));
        source.insert("corpus_partial".into(), json!(row.7 != 0));
        let pages_cap = serde_json::from_str::<Value>(&row.8)
            .ok()
            .and_then(|v| v.get("pages").cloned());
        source.insert("pages_coverage".into(), pages_cap.unwrap_or(Value::Null));
        let audit_missing = !audit_v2_path(path).is_file();
        Ok(Self {
            con,
            path: path.to_path_buf(),
            source,
            status_index,
            issue_state: None,
            audit_missing,
        })
    }

    /// Whether an audit.v2 companion sits next to the scan.
    pub fn audit_available(&self) -> bool {
        !self.audit_missing
    }

    /// Stream saved findings into `temp.issue_urls`/`temp.issue_url_counts` —
    /// the tables `issue_count` and the `issue` filter read. A no-arg call
    /// materializes every finding; check/severity narrow the selection.
    /// Port of `issue_selection.materialize` over the streaming reader.
    pub fn materialize_issues(
        &mut self,
        checks: &[String],
        severity: Option<&str>,
    ) -> Result<(), IssueSelectionError> {
        let wanted: BTreeSet<String> = checks.iter().map(|c| issue_code(c)).collect();
        let state_key = (wanted.clone(), severity.map(str::to_owned));
        if self.issue_state.as_ref() == Some(&state_key) {
            return Ok(());
        }
        if self.audit_missing {
            return Err(IssueSelectionError {
                reason_code: "issue_index_missing",
                message: "saved findings are unavailable: no audit.v2 companion".into(),
            });
        }
        // `new` validates identity, schema and (with verify_binding) the scan binding.
        // verify_binding=false: binding verification opens the scan through the
        // strict native validator (a later store slice); the companion's own
        // schema/hash/ordering is still fully checked here.
        let reader = AuditV2Reader::new(&self.path, false, true, Some(PAGE_BUDGET_SECONDS))
            .map_err(|e| IssueSelectionError {
                reason_code: "issue_index_missing",
                message: format!("saved findings are unavailable: {e}"),
            })?;
        let header = reader.header.clone();
        let run = header.get("run").cloned().unwrap_or(Value::Null);
        fn code_of(v: &Value) -> &str {
            v.get("id").and_then(Value::as_str).unwrap_or("")
        }
        let skipped: Vec<String> = run
            .get("checks_skipped")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|row| !wanted.is_empty() && wanted.contains(&issue_code(code_of(row))))
            .map(|row| {
                let reason = row.get("reason").and_then(Value::as_str).unwrap_or("");
                format!("{}: {}", code_of(row), reason)
            })
            .collect();
        if !skipped.is_empty() {
            return Err(IssueSelectionError {
                reason_code: "issue_check_unmeasured",
                message: skipped.join("; "),
            });
        }
        let coverage = header
            .get("summary")
            .and_then(|s| s.get("check_coverage"))
            .cloned()
            .unwrap_or(Value::Null);
        let ids_of = |key: &str| -> BTreeSet<String> {
            coverage
                .get(key)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(issue_code)
                .collect()
        };
        let disabled = ids_of("checks_disabled_ids");
        let blocked: Vec<String> = wanted.intersection(&disabled).cloned().collect();
        if !blocked.is_empty() {
            return Err(IssueSelectionError {
                reason_code: "issue_check_unmeasured",
                message: format!(
                    "requested check(s) disabled in saved audit: {}",
                    blocked.join(", ")
                ),
            });
        }
        let mut measured: BTreeSet<String> = ids_of("checks_silent_ids")
            .intersection(&wanted)
            .cloned()
            .collect();
        // Temp tables only: the main file stays read-only via `mode=ro`; a
        // `query_only=ON` pragma would block these writes too, so it is not set.
        self.con
            .execute_batch(
                "CREATE TEMP TABLE IF NOT EXISTS issue_urls(url_id INTEGER PRIMARY KEY);
                 CREATE TEMP TABLE IF NOT EXISTS issue_url_counts(
                     url_id INTEGER PRIMARY KEY, n INTEGER NOT NULL) WITHOUT ROWID;
                 CREATE TEMP TABLE IF NOT EXISTS issue_groups(group_id TEXT PRIMARY KEY) WITHOUT ROWID;
                 DELETE FROM issue_urls;
                 DELETE FROM issue_url_counts;
                 DELETE FROM issue_groups;",
            )
            .map_err(|e| IssueSelectionError {
                reason_code: "issue_index_missing",
                message: format!("cannot prepare the issue selection: {e}"),
            })?;
        let issues = reader
            .iter_collection("/issues")
            .map_err(|e| IssueSelectionError {
                reason_code: "issue_index_missing",
                message: format!("saved findings are unavailable: {e}"),
            })?;
        for issue in &issues {
            let check = issue.get("check").and_then(Value::as_str).unwrap_or("");
            let check_code = issue_code(check);
            if !wanted.is_empty() && wanted.contains(&check_code) {
                measured.insert(check_code.clone());
            }
            if !wanted.is_empty() && !wanted.contains(&check_code) {
                continue;
            }
            if let Some(sev) = severity {
                if issue.get("severity").and_then(Value::as_str) != Some(sev) {
                    continue;
                }
            }
            let source_page = match issue.get("details") {
                Some(d) if check.starts_with("IMG_") && d.is_object() => d
                    .get("source_page")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                _ => None,
            };
            let mut named: BTreeSet<String> = BTreeSet::new();
            if let Some(url) = issue.get("target_url").and_then(Value::as_str) {
                if !url.is_empty() {
                    named.insert(url.to_owned());
                }
            }
            if let Some(url) = source_page {
                if !url.is_empty() {
                    named.insert(url);
                }
            }
            for url in &named {
                self.con
                    .execute(
                        "INSERT INTO issue_url_counts(url_id,n) SELECT url_id,1 FROM urls \
                         WHERE url=?1 ON CONFLICT(url_id) DO UPDATE SET n=n+1",
                        params![url],
                    )
                    .map_err(sql_issue)?;
                self.con
                    .execute(
                        "INSERT OR IGNORE INTO issue_urls SELECT url_id FROM urls WHERE url=?1",
                        params![url],
                    )
                    .map_err(sql_issue)?;
            }
            if let Some(group) = issue.get("group_id").and_then(Value::as_str) {
                if !group.is_empty() {
                    self.con
                        .execute(
                            "INSERT OR IGNORE INTO issue_groups VALUES(?1)",
                            params![group],
                        )
                        .map_err(sql_issue)?;
                }
            }
        }
        let missing: Vec<String> = wanted.difference(&measured).cloned().collect();
        if !missing.is_empty() {
            return Err(IssueSelectionError {
                reason_code: "issue_check_unmeasured",
                message: format!(
                    "requested check(s) absent from saved audit measurement inventory: {}",
                    missing.join(", ")
                ),
            });
        }
        let groups = reader
            .iter_collection("/groups")
            .map_err(|e| IssueSelectionError {
                reason_code: "issue_index_missing",
                message: format!("saved findings are unavailable: {e}"),
            })?;
        for group in &groups {
            let Some(group_id) = group.get("group_id").and_then(Value::as_str) else {
                return Err(IssueSelectionError {
                    reason_code: "issue_index_missing",
                    message: "group entry lacks group_id".into(),
                });
            };
            let member: Option<i64> = self
                .con
                .query_row(
                    "SELECT 1 FROM issue_groups WHERE group_id=?1",
                    params![group_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sql_issue)?;
            if member.is_none() {
                continue;
            }
            let urls: Vec<String> = group
                .get("urls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            for url in urls {
                self.con
                    .execute(
                        "INSERT OR IGNORE INTO issue_urls SELECT url_id FROM urls WHERE url=?1",
                        params![url],
                    )
                    .map_err(sql_issue)?;
            }
        }
        self.issue_state = Some(state_key);
        Ok(())
    }

    /// Wall-clock deadline guard for one statement (reference `_Budget`).
    pub fn budget(&self, seconds: f64) {
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs_f64(seconds.max(0.0));
        self.con.progress_handler(
            PROGRESS_OPS,
            Some(move || std::time::Instant::now() >= deadline),
        );
    }

    /// Stop the deadline guard.
    pub fn clear_budget(&self) {
        self.con.progress_handler(0, None::<fn() -> bool>);
    }
}

/// `file:` URI for a scan path — every byte outside RFC 3986 unreserved is
/// percent-encoded so `?`, `#` or `%` in a filename cannot corrupt the URI
/// query/fragment. Path separators stay literal.
pub(crate) fn file_uri(path: &Path) -> String {
    let mut out = String::from("file:");
    for &b in path.to_string_lossy().as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0xF) as usize] as char);
            }
        }
    }
    out
}

fn sql_issue(e: rusqlite::Error) -> IssueSelectionError {
    IssueSelectionError {
        reason_code: "issue_index_missing",
        message: format!("cannot build the issue selection: {e}"),
    }
}

/// Normalise a check identifier: casefolded, `-` → `_` (reference parity).
pub fn issue_code(value: &str) -> String {
    value.to_lowercase().replace('-', "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uri_percent_encodes_specials() {
        let uri = file_uri(Path::new("/tmp/a?b#c%d/scan.sqlite"));
        assert_eq!(uri, "file:/tmp/a%3Fb%23c%25d/scan.sqlite");
    }

    // regression (review 2026-10-27): the file: URI carried the raw path, so
    // `?`, `#` or `%` in the scan's directory name was parsed as URI syntax.
    #[test]
    fn scan_in_a_uri_special_directory_still_opens() {
        let src =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../oracle/raw-scans/basic/scan.sqlite");
        let dir = std::env::temp_dir().join(format!("desk-uri ?#%{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("scan.sqlite");
        std::fs::copy(&src, &path).unwrap();
        ScanSession::open(&path).expect("a scan under a URI-special path must open");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

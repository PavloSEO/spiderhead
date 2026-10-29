//! Select saved finding URLs in temporary SQLite tables; the scan remains read-only.

use std::collections::BTreeSet;
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use crate::audit_v2::{audit_v2_path, AuditV2Reader};
use crate::error::StoreError;
use crate::inputs::{resolve_audit_source, AuditSource};

/// Requested saved checks were explicitly unmeasured, never an empty clean selection.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct UnmeasuredChecks {
    /// Human-readable reason naming the check(s).
    pub message: String,
}

/// Normalise a check identifier: casefolded, with `-` replaced by `_`.
pub fn code(value: &str) -> String {
    value.to_lowercase().replace('-', "_")
}

fn str_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn sorted_codes<'a>(values: impl Iterator<Item = &'a str>) -> BTreeSet<String> {
    values.map(code).collect()
}

/// Stream validated findings and group members into a bounded-memory URL selection.
///
/// audit.v2 collections stream; the compatibility JSON reader retains its finite
/// byte limit. No index is persisted and no finding is recalculated. Two temp
/// tables result: ``issue_urls`` (the selection) and ``issue_url_counts``, one
/// row per URL with the number of matching issues that name it (group members
/// are rollups of one finding and add to the selection only, never the count).
pub fn materialize(
    con: &Connection,
    path: &Path,
    checks: &[String],
    severity: Option<&str>,
    timeout_seconds: f64,
) -> Result<(), StoreError> {
    let wanted: BTreeSet<String> = checks.iter().map(|c| code(c)).collect();
    let source = if audit_v2_path(path).exists() {
        AuditSource::Streaming(Box::new(AuditV2Reader::new(
            path,
            true,
            true,
            Some(timeout_seconds),
        )?))
    } else {
        resolve_audit_source(&Value::String(path.display().to_string()))?.0
    };
    let header = source.header();
    // A streaming collection when the reader has one, else the header's inline array.
    let rows = |pointer: &str, inline: Option<&Value>| -> Result<Vec<Value>, StoreError> {
        match source.reader() {
            Some(reader) if reader.collections.contains_key(pointer) => {
                reader.iter_collection(pointer)
            }
            _ => Ok(inline
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()),
        }
    };
    let run = header.get("run").cloned().unwrap_or(Value::Null);
    let skipped_rows = rows("/run/checks_skipped", run.get("checks_skipped"))?;
    let skipped: Vec<&Value> = skipped_rows
        .iter()
        .filter(|row| !wanted.is_empty() && wanted.contains(&code(str_field(row, "id"))))
        .collect();
    if !skipped.is_empty() {
        let message = skipped
            .iter()
            .map(|row| format!("{}: {}", str_field(row, "id"), str_field(row, "reason")))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(UnmeasuredChecks { message }.into());
    }
    let coverage = header
        .get("summary")
        .and_then(|s| s.get("check_coverage"))
        .cloned()
        .unwrap_or(Value::Null);
    let disabled: BTreeSet<String> = sorted_codes(
        coverage
            .get("checks_disabled_ids")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str),
    );
    let blocked: Vec<String> = wanted.intersection(&disabled).cloned().collect();
    if !blocked.is_empty() {
        return Err(UnmeasuredChecks {
            message: format!(
                "requested check(s) disabled in saved audit: {}",
                blocked.join(", ")
            ),
        }
        .into());
    }
    let mut measured: BTreeSet<String> = sorted_codes(
        coverage
            .get("checks_silent_ids")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str),
    )
    .intersection(&wanted)
    .cloned()
    .collect();

    con.execute_batch("PRAGMA query_only=OFF")?;
    let materialized = materialize_inner(con, &source, header, &mut measured, &wanted, severity);
    let restore = con.execute_batch("PRAGMA query_only=ON");
    if let Err(e) = restore {
        return materialized.and(Err(e.into()));
    }
    materialized
}

/// The writable phase of [`materialize`]: the caller restores `query_only=ON`
/// whether this returns `Ok` or `Err`, so the scan never stays writable.
fn materialize_inner(
    con: &Connection,
    source: &AuditSource,
    header: &Value,
    measured: &mut BTreeSet<String>,
    wanted: &BTreeSet<String>,
    severity: Option<&str>,
) -> Result<(), StoreError> {
    let rows = |pointer: &str, inline: Option<&Value>| -> Result<Vec<Value>, StoreError> {
        match source.reader() {
            Some(reader) if reader.collections.contains_key(pointer) => {
                reader.iter_collection(pointer)
            }
            _ => Ok(inline
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()),
        }
    };
    con.execute_batch(
        "PRAGMA temp_store=FILE;
         CREATE TEMP TABLE IF NOT EXISTS issue_urls(url_id INTEGER PRIMARY KEY);
         CREATE TEMP TABLE IF NOT EXISTS issue_url_counts(
             url_id INTEGER PRIMARY KEY, n INTEGER NOT NULL) WITHOUT ROWID;
         CREATE TEMP TABLE IF NOT EXISTS issue_groups(group_id TEXT PRIMARY KEY) WITHOUT ROWID;",
    )?;
    let issues = rows("/issues", header.get("issues"))?;
    for issue in &issues {
        let check = str_field(issue, "check");
        let check_code = code(check);
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
        let details = issue.get("details");
        let source_page = match details {
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
            con.execute(
                "INSERT INTO issue_url_counts(url_id,n) SELECT url_id,1 FROM urls WHERE url=?1 \
                 ON CONFLICT(url_id) DO UPDATE SET n=n+1",
                params![url],
            )?;
            con.execute(
                "INSERT OR IGNORE INTO issue_urls SELECT url_id FROM urls WHERE url=?1",
                params![url],
            )?;
        }
        if let Some(group) = issue.get("group_id").and_then(Value::as_str) {
            if !group.is_empty() {
                con.execute(
                    "INSERT OR IGNORE INTO issue_groups VALUES(?1)",
                    params![group],
                )?;
            }
        }
    }
    let missing: Vec<String> = wanted.difference(measured).cloned().collect();
    if !missing.is_empty() {
        return Err(UnmeasuredChecks {
            message: format!(
                "requested check(s) absent from saved audit measurement inventory: {}",
                missing.join(", ")
            ),
        }
        .into());
    }
    let groups = rows("/groups", header.get("groups"))?;
    for group in &groups {
        let Some(group_id) = group.get("group_id").and_then(Value::as_str) else {
            return Err(StoreError::Invalid("group entry lacks group_id".into()));
        };
        let member: Option<i64> = con
            .query_row(
                "SELECT 1 FROM issue_groups WHERE group_id=?1",
                params![group_id],
                |r| r.get(0),
            )
            .optional()?;
        if member.is_none() {
            continue;
        }
        let members: Vec<Value> = match source.reader().and_then(|r| r.group_members(group)) {
            Some(members) => members.to_vec()?,
            None => group
                .get("urls")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        };
        for url in members.iter().filter_map(Value::as_str) {
            con.execute(
                "INSERT OR IGNORE INTO issue_urls SELECT url_id FROM urls WHERE url=?1",
                params![url],
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every return path must restore PRAGMA query_only, or the caller's connection keeps it off.
    #[test]
    fn query_only_is_restored_after_an_unmeasured_error() {
        let dir = std::env::temp_dir().join(format!(
            "spiderhead-issue-sel-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let audit = dir.join("audit.json");
        std::fs::write(
            &audit,
            "{\"run\":{\"checks_skipped\":[]},\"summary\":{\"check_coverage\":\
             {\"checks_disabled_ids\":[],\"checks_silent_ids\":[]}},\"issues\":[],\"groups\":[]}",
        )
        .unwrap();
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch("PRAGMA query_only=ON").unwrap();
        let err = materialize(&con, &audit, &["ZZZ".to_owned()], None, 5.0);
        assert!(err.is_err());
        let ro: bool = con
            .query_row("PRAGMA query_only", [], |r| r.get(0))
            .unwrap();
        assert!(ro, "query_only must be restored after the error");
    }
}

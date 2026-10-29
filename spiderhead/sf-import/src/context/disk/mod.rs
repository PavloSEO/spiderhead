//! Disk-backed stores behind `disk_backed_pages=True`.
//!
//! re-iterable sqlite-backed collections that keep the audit corpus off the
//! heap while preserving the in-memory observable behavior.

mod groups;
mod issues;
mod pages;

pub use groups::DiskGroups;
pub use issues::{DiskIssueResults, DiskIssues};
pub use pages::DiskPages;

use rusqlite::Connection;
use serde_json::{json, Value};
use spiderhead_formats::models::{Issue, Page};
use std::path::PathBuf;
use thiserror::Error;

use crate::normalize::Object;

/// Disk-store failure surface.
#[derive(Debug, Error)]
pub enum DiskError {
    /// sqlite failure.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    /// JSON encode/decode failure.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// URL normalization failure.
    #[error("url normalize failed: {0}")]
    Norm(#[from] spiderhead_core::url_normalize::NormUrlError),
}

fn temp_sqlite(prefix: &str) -> Result<(PathBuf, Connection), DiskError> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("{prefix}-{}-{seq}.sqlite", std::process::id()));
    let con = Connection::open(&path)?;
    Ok((path, con))
}

fn unlink(path: &PathBuf) {
    let _ = std::fs::remove_file(path);
}

/// `Page` reconstruction from a stored `state_json` blob.
fn page_from_state(url: &str, status_code: Option<i64>, state: &Object) -> Page {
    let mut page = Page::new(url);
    page.status_code = status_code;
    page.status = state
        .get("status")
        .and_then(Value::as_str)
        .map(str::to_owned);
    page.content_type = state
        .get("content_type")
        .and_then(Value::as_str)
        .map(str::to_owned);
    page.indexability = state
        .get("indexability")
        .and_then(Value::as_str)
        .map(str::to_owned);
    page.indexability_status = state
        .get("indexability_status")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if let Some(Value::Object(m)) = state.get("metrics") {
        page.metrics = m.clone();
    }
    for (dst, key) in [
        (&mut page.issues, "issues"),
        (&mut page.issue_ids, "issue_ids"),
        (&mut page.suppressed_issue_ids, "suppressed_issue_ids"),
    ] {
        if let Some(Value::Array(items)) = state.get(key) {
            *dst = items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
        }
    }
    page
}

/// The `state_json` blob for a page.
fn page_state(page: &Page) -> Object {
    let mut state = Object::new();
    state.insert(
        "status".into(),
        page.status.clone().map_or(Value::Null, Value::String),
    );
    state.insert(
        "content_type".into(),
        page.content_type.clone().map_or(Value::Null, Value::String),
    );
    state.insert(
        "indexability".into(),
        page.indexability.clone().map_or(Value::Null, Value::String),
    );
    state.insert(
        "indexability_status".into(),
        page.indexability_status
            .clone()
            .map_or(Value::Null, Value::String),
    );
    state.insert("metrics".into(), Value::Object(page.metrics.clone()));
    let str_list = |v: &[String]| Value::Array(v.iter().cloned().map(Value::String).collect());
    state.insert("issues".into(), str_list(&page.issues));
    state.insert("issue_ids".into(), str_list(&page.issue_ids));
    state.insert(
        "suppressed_issue_ids".into(),
        str_list(&page.suppressed_issue_ids),
    );
    state
}

/// `Issue` reconstruction from a stored `value_json` blob.
fn issue_from_state(v: &Object) -> Issue {
    let mut issue = Issue::new(
        v.get("check").and_then(Value::as_str).unwrap_or(""),
        v.get("severity").and_then(Value::as_str).unwrap_or(""),
        v.get("source").and_then(Value::as_str).unwrap_or(""),
        v.get("message").and_then(Value::as_str).unwrap_or(""),
    );
    issue.id = v.get("id").and_then(Value::as_str).map(str::to_owned);
    issue.fingerprint = v
        .get("fingerprint")
        .and_then(Value::as_str)
        .map(str::to_owned);
    issue.target_url = v
        .get("target_url")
        .and_then(Value::as_str)
        .map(str::to_owned);
    issue.status_code = v.get("status_code").and_then(Value::as_i64);
    if let Some(n) = v.get("occurrences_count").and_then(Value::as_i64) {
        issue.occurrences_count = n;
    }
    if let Some(Value::Array(items)) = v.get("locations") {
        issue.locations = items
            .iter()
            .filter_map(|x| x.as_object().cloned())
            .collect();
    }
    if let Some(Value::Object(d)) = v.get("details") {
        issue.details = d.clone();
    }
    issue.group_id = v.get("group_id").and_then(Value::as_str).map(str::to_owned);
    issue.fix_hint = v.get("fix_hint").and_then(Value::as_str).map(str::to_owned);
    if let Some(Value::Object(e)) = v.get("evidence") {
        issue.evidence = e.clone();
    }
    issue
}

/// The `value_json` blob for an issue (`issue.__dict__` shape).
fn issue_state(issue: &Issue) -> Value {
    json!({
        "check": issue.check,
        "severity": issue.severity,
        "source": issue.source,
        "message": issue.message,
        "id": issue.id,
        "fingerprint": issue.fingerprint,
        "target_url": issue.target_url,
        "status_code": issue.status_code,
        "occurrences_count": issue.occurrences_count,
        "locations": issue.locations,
        "details": issue.details,
        "group_id": issue.group_id,
        "fix_hint": issue.fix_hint,
        "evidence": issue.evidence,
    })
}

/// reference `index.indices(len)` start/stop normalization for step-1 slices.
/// reference `index.indices(len)` start/stop normalization for step-1 slices.
fn normalize_slice(start: i64, stop: i64, len: i64) -> (i64, i64) {
    let s = if start < 0 {
        (len + start).max(0)
    } else {
        start.min(len)
    };
    let e = if stop < 0 {
        (len + stop).max(0)
    } else {
        stop.min(len)
    };
    (s, e)
}

/// The `iter_deduped_sorted` merge: yield the sorted stream, folding each
/// issue into the pending one when it carries a target URL and shares the
/// pending key.
pub(crate) fn dedupe_merge(issues: Vec<Issue>) -> Vec<Issue> {
    let mut out: Vec<Issue> = Vec::new();
    let mut current: Option<Issue> = None;
    let mut current_key = None;
    for issue in issues {
        let key = issue.dedupe_key();
        if let (Some(cur), Some(ckey)) = (current.as_mut(), &current_key) {
            if issue.target_url.is_some() && key == *ckey {
                cur.merge(&issue);
                continue;
            }
            out.push(cur.clone());
        }
        current = Some(issue);
        current_key = Some(key);
    }
    if let Some(cur) = current {
        out.push(cur);
    }
    out
}

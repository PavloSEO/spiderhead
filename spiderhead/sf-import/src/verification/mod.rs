//! Targeted verification of saved audit findings against a measured URL subset.
//!
//! page or an unmeasured check is never a fix. Documents are plain JSON maps; audit.v2
//! sources go through [`spiderhead_store::audit_v2::AuditV2Reader`].

mod classify;
mod identity;
mod iso;
mod markdown;
mod select;

pub use classify::classify;
pub use identity::{
    digest, offline_observation_gap, results_policy_fingerprint, scan_identity, source_identity,
};
pub use markdown::{markdown, python_dumps_indent};
pub use select::{compact_after_source, select, select_source};

use serde_json::{Map, Value};
use spiderhead_core::finding_policy::{
    DEPTH_FINDING_CHECKS, GRAPH_WIDE_FINDING_CHECKS, UNLINKED_FINDING_CHECKS,
};
use spiderhead_formats::check_registry;
use std::collections::HashSet;

/// JSON object map shorthand used across the module.
pub(crate) type Object = Map<String, Value>;

/// Hard cap on selected URLs one verification run may fetch.
pub const MAX_URLS: usize = 500;
/// Hard cap on selected findings.
pub const MAX_FINDINGS: usize = 5_000;

/// Checks judging a page beyond itself (reference `_NONLOCAL`): unlinked/graph-wide/depth
/// policy checks plus the literal set of duplicate and templated families.
pub(crate) const NONLOCAL_EXTRA: &[&str] = &[
    "INLINK_BOILERPLATE_ONLY",
    "TITLE_DUPLICATE",
    "DESC_DUPLICATE",
    "H1_DUPLICATE",
    "DUPLICATE_BY_HASH",
    "NEAR_DUPLICATE",
    "TITLE_TEMPLATED",
];

/// Checks whose verdict is the HTTP status itself (reference `_STATUS_CHECKS`).
pub(crate) const STATUS_CHECKS: &[&str] = &[
    "BROKEN_PAGE_4XX",
    "SERVER_ERROR_5XX",
    "NO_RESPONSE",
    "BLOCKED_BY_ROBOTS",
    "BAD_REDIRECT_TYPE",
];

/// Bounds a targeted collection is allowed to change (reference `_TARGET_BOUNDS`).
pub(crate) const TARGET_BOUNDS: &[&str] = &["limits.max_urls", "limits.max_depth"];

/// Error carrying the exact text the reference baseline raises as `ValueError`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct VerifyError(pub String);

pub(crate) fn fail<T>(text: impl Into<String>) -> Result<T, VerifyError> {
    Err(VerifyError(text.into()))
}

/// reference truthiness for JSON values: `None`/`False`/`0`/`""`/`[]`/`{}` are falsy.
pub(crate) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

/// reference `==` between two JSON values: bools compare as numbers (`True == 1`),
/// numbers across int/float forms (`1 == 1.0`), containers recurse element-wise.
pub fn py_eq(a: &Value, b: &Value) -> bool {
    let as_num = |v: &Value| -> Option<f64> {
        match v {
            Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            Value::Number(n) => n.as_f64(),
            _ => None,
        }
    };
    match (a, b) {
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| py_eq(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| py_eq(v, w)))
        }
        _ => match (as_num(a), as_num(b)) {
            (Some(x), Some(y)) => x == y,
            _ => a == b,
        },
    }
}

/// Whether `value` occurs in `hay` under reference `==` (the `in` operator on a list).
pub(crate) fn py_contains(hay: &[Value], value: &Value) -> bool {
    hay.iter().any(|h| py_eq(h, value))
}

/// `obj[key]` when it is a JSON object, like the reference `isinstance(x, Mapping)` guard.
pub(crate) fn get_mapping<'a>(obj: &'a Object, key: &str) -> Option<&'a Object> {
    obj.get(key).and_then(Value::as_object)
}

/// `", ".join(sorted(items))` used inside error messages.
pub(crate) fn sorted_join(items: &HashSet<String>) -> String {
    let mut v: Vec<&String> = items.iter().collect();
    v.sort();
    v.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
}

/// `sorted(map(str, items))` rendered as a reference list literal inside an f-string.
pub(crate) fn sorted_list_repr(items: &[Value]) -> String {
    use crate::pyvalue::str_or_dumps;
    use spiderhead_parse::py_str::py_repr;
    let mut strs: Vec<String> = items.iter().map(str_or_dumps).collect();
    strs.sort();
    let inner = strs
        .iter()
        .map(|s| py_repr(s))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{inner}]")
}

/// Lowercase hex of a digest.
pub(crate) fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A baseline or "after" audit: a plain document or an audit.v2 companion reader.
#[derive(Clone, Copy)]
pub enum AuditSource<'a> {
    /// A saved audit.json document.
    Document(&'a Object),
    /// A validated audit.v2 companion.
    Reader(&'a spiderhead_store::audit_v2::AuditV2Reader),
}

impl AuditSource<'_> {
    /// All rows of one collection, materialized (documents read the key directly).
    pub(crate) fn iter_collection(&self, pointer: &str) -> Result<Vec<Value>, VerifyError> {
        match self {
            Self::Reader(reader) => reader
                .iter_collection(pointer)
                .map_err(|e| VerifyError(e.to_string())),
            Self::Document(doc) => Ok(doc
                .get(pointer.trim_start_matches('/'))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()),
        }
    }
}

/// reference `_local_issue`: a page-local check with a non-heuristic source and no group.
pub(crate) fn local_issue(issue: &Object) -> bool {
    let check = issue.get("check").cloned().unwrap_or(Value::Null);
    let nonlocal: Vec<&str> = UNLINKED_FINDING_CHECKS
        .iter()
        .chain(GRAPH_WIDE_FINDING_CHECKS)
        .chain(DEPTH_FINDING_CHECKS)
        .chain(NONLOCAL_EXTRA)
        .copied()
        .collect();
    if let Some(c) = check.as_str() {
        if nonlocal.contains(&c) {
            return false;
        }
    }
    let source = check
        .as_str()
        .and_then(|c| check_registry::builtin().ok().and_then(|r| r.get(c)))
        .map(|m| m.source.clone())
        .unwrap_or_default();
    if source.starts_with("inlinks:")
        || source.starts_with("sitemap")
        || source.starts_with("heuristic")
        || source == "SF-derived+heuristic"
    {
        return false;
    }
    !issue.get("group_id").is_some_and(truthy)
}

/// The unique key `_unique_rows` checks for one row kind.
pub(crate) fn row_key(row: &Object, kind: &str) -> Vec<Value> {
    if kind == "page" {
        vec![row.get("url").cloned().unwrap_or(Value::Null)]
    } else {
        vec![
            row.get("check").cloned().unwrap_or(Value::Null),
            row.get("target_url").cloned().unwrap_or(Value::Null),
        ]
    }
}

/// reference `_unique_rows`: reject duplicate keys and duplicate finding IDs.
pub(crate) fn unique_rows(rows: &[&Object], kind: &str) -> Result<(), VerifyError> {
    let mut keys: Vec<Vec<Value>> = Vec::new();
    let mut ids: Vec<Value> = Vec::new();
    for row in rows {
        let key = row_key(row, kind);
        if keys
            .iter()
            .any(|k| k.len() == key.len() && k.iter().zip(&key).all(|(a, b)| py_eq(a, b)))
        {
            return fail(format!("selected audit has duplicate {kind} keys"));
        }
        keys.push(key);
        if kind == "finding" {
            if let Some(id) = row.get("id").filter(|v| truthy(v)) {
                if py_contains(&ids, id) {
                    return fail("selected audit has duplicate finding IDs");
                }
                ids.push(id.clone());
            }
        }
    }
    Ok(())
}

//! Structured, append-only project event log with bounded, paginated reading
//!
//! Events live in `events.jsonl` beside `log.md`; appending never edits earlier lines and
//! reading never writes. `log.md` stays the human narrative, maintained by `journal`.

use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::error::{io, ProjectsError, ProjectsResult};
use crate::workspace::load as workspace_load;
use crate::{locks, pyjson, pytime};

/// Event file name under the project root.
pub const FILE: &str = "events.jsonl";
/// Allowed `source` values.
pub const SOURCES: &[&str] = &["agent", "user", "scans", "app"];
/// Allowed `actor` values.
pub const ACTORS: &[&str] = &["user", "agent", "schedule"];
/// Maximum event text length.
pub const MAX_TEXT: usize = 2_000;
/// Maximum `query` length.
pub const MAX_QUERY: usize = 200;
/// Maximum retained events.
pub const MAX_EVENTS: usize = 10_000;
/// Maximum file bytes.
pub const MAX_BYTES: u64 = 8 * 1024 * 1024;
/// Maximum page limit.
pub const MAX_LIMIT: i64 = 200;
/// Maximum follow wait.
pub const MAX_FOLLOW_SECONDS: i64 = 30;

fn event(value: &Value) -> ProjectsResult<Value> {
    let object = value.as_object().filter(|o| {
        o.keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>()
            == ["sequence", "occurred_at", "source", "actor", "text"]
                .into_iter()
                .collect()
    });
    let Some(object) = object else {
        return Err(crate::error::value(
            "project event has an unsupported shape",
        ));
    };
    let sequence = object["sequence"].as_i64();
    if object["sequence"].is_boolean() || sequence.is_none_or(|s| s < 1) {
        return Err(crate::error::value("project event sequence is invalid"));
    }
    if !SOURCES.contains(&object["source"].as_str().unwrap_or_default())
        || !ACTORS.contains(&object["actor"].as_str().unwrap_or_default())
    {
        return Err(crate::error::value(
            "project event source or actor is unsupported",
        ));
    }
    if object["occurred_at"].as_str().is_none() || object["text"].as_str().is_none() {
        return Err(crate::error::value("project event fields are invalid"));
    }
    let occurred = pytime::fromisoformat(
        &object["occurred_at"]
            .as_str()
            .unwrap_or_default()
            .replace('Z', "+00:00"),
    )
    .map_err(|_| crate::error::value("project event time must be RFC3339 UTC"))?;
    if !occurred.is_aware() {
        return Err(crate::error::value(
            "project event time must be RFC3339 UTC",
        ));
    }
    text(object["text"].as_str().unwrap_or_default())?;
    Ok(value.clone())
}

pub(crate) fn text(raw: &str) -> ProjectsResult<String> {
    if raw.trim().is_empty() || raw.len() > MAX_TEXT {
        return Err(crate::error::value(format!(
            "event text must be 1..{MAX_TEXT} characters"
        )));
    }
    if raw.contains('\x00') {
        return Err(crate::error::value("event text must not contain NUL bytes"));
    }
    Ok(raw.to_owned())
}

fn path(root: &Path) -> ProjectsResult<PathBuf> {
    let path = root.join(FILE);
    if path.is_symlink() {
        return Err(crate::error::value("project event log is unsafe"));
    }
    Ok(path)
}

fn parse(stream: &mut std::fs::File) -> ProjectsResult<Vec<Value>> {
    let size = stream.seek(SeekFrom::End(0)).map_err(ProjectsError::Io)?;
    if size > MAX_BYTES {
        return Err(crate::error::value(
            "project event log exceeds its byte limit",
        ));
    }
    stream.seek(SeekFrom::Start(0)).map_err(ProjectsError::Io)?;
    let mut rows = Vec::new();
    let reader = BufReader::new(stream);
    for (index, line) in reader.lines().enumerate() {
        let number = index + 1;
        let line = line.map_err(ProjectsError::Io)?;
        let row: Value = serde_json::from_str(&line).map_err(|_| {
            crate::error::value(format!("project event line {number} is not valid JSON"))
        })?;
        rows.push(event(&row)?);
    }
    Ok(rows)
}

fn append_locked(root: &Path, source: &str, actor: &str, text: &str) -> ProjectsResult<Value> {
    let path = path(root)?;
    let mut stream = io(std::fs::OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(&path))?;
    locks::lock_exclusive_wait(&stream).map_err(ProjectsError::Io)?;
    let result = (|| -> ProjectsResult<Value> {
        let rows = parse(&mut stream)?;
        if rows.len() >= MAX_EVENTS {
            return Err(crate::error::value("project event log is full"));
        }
        let event = json!({
            "sequence": rows.len() + 1,
            "occurred_at": pytime::zulu(),
            "source": source,
            "actor": actor,
            "text": text,
        });
        stream.seek(SeekFrom::End(0)).map_err(ProjectsError::Io)?;
        stream
            .write_all((pyjson::dumps_compact(&event) + "\n").as_bytes())
            .map_err(ProjectsError::Io)?;
        stream.flush().map_err(ProjectsError::Io)?;
        Ok(json!({"ok": true, "event": event}))
    })();
    let _ = locks::unlock(&stream);
    result
}

/// `append`: append one validated event and return it with its sequence number.
pub fn append(directory: &Path, source: &str, actor: &str, text: &str) -> ProjectsResult<Value> {
    if !SOURCES.contains(&source) || !ACTORS.contains(&actor) {
        return Err(crate::error::value("event source or actor is unsupported"));
    }
    let text = self::text(text)?.trim().to_owned();
    let (root, _project) = workspace_load(directory)?;
    let _guard = crate::runtime::project_write_lock(&root, 5.0)?;
    append_locked(&root, source, actor, &text)
}

/// `append_locked` for callers already owning the project write lock.
pub fn append_within_lock(
    root: &Path,
    source: &str,
    actor: &str,
    text: &str,
) -> ProjectsResult<Value> {
    append_locked(root, source, actor, text)
}

/// `page`: read a newest-first page, optionally waiting for events after a revision.
#[allow(clippy::too_many_arguments)]
pub fn page(
    directory: &Path,
    offset: i64,
    limit: i64,
    source: Option<&str>,
    query: &str,
    since_revision: Option<i64>,
    follow: bool,
    wait_seconds: i64,
) -> ProjectsResult<Value> {
    if offset < 0 || !(1..=MAX_LIMIT).contains(&limit) {
        return Err(crate::error::value(format!(
            "offset must be nonnegative and limit must be 1..{MAX_LIMIT}"
        )));
    }
    if let Some(s) = source {
        if !SOURCES.contains(&s) {
            return Err(crate::error::value("event source filter is unsupported"));
        }
    }
    if query.len() > MAX_QUERY {
        return Err(crate::error::value(format!(
            "query must be at most {MAX_QUERY} characters"
        )));
    }
    if let Some(rev) = since_revision {
        if rev < 0 {
            return Err(crate::error::value(
                "since_revision must be a nonnegative integer",
            ));
        }
    }
    if !(0..=MAX_FOLLOW_SECONDS).contains(&wait_seconds) {
        return Err(crate::error::value(format!(
            "wait_seconds must be within 0..{MAX_FOLLOW_SECONDS}"
        )));
    }
    if follow && since_revision.is_none() {
        return Err(crate::error::value("follow requires since_revision"));
    }
    let (root, _project) = workspace_load(directory)?;
    let path = path(&root)?;

    let snapshot = |path: &Path| -> ProjectsResult<Value> {
        let rows = if path.exists() {
            let mut stream = io(std::fs::File::open(path))?;
            locks::lock_shared(&stream).map_err(ProjectsError::Io)?;
            let rows = parse(&mut stream);
            let _ = locks::unlock(&stream);
            rows?
        } else {
            Vec::new()
        };
        let revision = rows
            .last()
            .and_then(|r| r["sequence"].as_i64())
            .unwrap_or(0);
        let needle = query.to_lowercase();
        let matched: Vec<Value> = rows
            .iter()
            .rev()
            .filter(|row| {
                since_revision.is_none_or(|rev| row["sequence"].as_i64().unwrap_or(0) > rev)
                    && source.is_none_or(|s| row["source"].as_str() == Some(s))
                    && (needle.is_empty()
                        || row["text"]
                            .as_str()
                            .unwrap_or_default()
                            .to_lowercase()
                            .contains(&needle))
            })
            .cloned()
            .collect();
        let total = matched.len() as i64;
        Ok(json!({
            "ok": true,
            "revision": revision,
            "not_modified": since_revision.is_some_and(|rev| revision <= rev),
            "total": total,
            "items": matched
                .iter()
                .skip(offset as usize)
                .take(limit as usize)
                .cloned()
                .collect::<Vec<_>>(),
            "pagination": {
                "total": total,
                "offset": offset,
                "limit": limit,
                "next_offset": if offset + limit < total { json!(offset + limit) } else { Value::Null },
                "previous_offset": if offset > 0 { json!(std::cmp::max(0, offset - limit)) } else { Value::Null },
            },
        }))
    };

    let mut result = snapshot(&path)?;
    if follow && result["not_modified"].as_bool().unwrap_or(false) && wait_seconds > 0 {
        let deadline = pytime::monotonic() + wait_seconds as f64;
        while pytime::monotonic() < deadline {
            let rest = (deadline - pytime::monotonic()).clamp(0.0, 0.25);
            std::thread::sleep(std::time::Duration::from_secs_f64(rest));
            result = snapshot(&path)?;
            if !result["not_modified"].as_bool().unwrap_or(false) {
                break;
            }
        }
    }
    Ok(result)
}

//! Bounded project action history for pending, denied and completed operations
//!
//! Actions live in `actions.jsonl` with a contiguous sequence; readers take an immutable
//! snapshot so a writer never blocks them, and pending rows resolve atomically in place.

use spiderhead_core::filesystem::{file_identity, link_identity, NoFollow};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};

use serde_json::{json, Value};

use crate::error::{io, ProjectsError, ProjectsResult};
use crate::workspace::load as workspace_load;
use crate::{locks, pyjson, pytime};
use spiderhead_core::filesystem::atomic_write_bytes;

/// Action file name under the project root.
pub const FILE: &str = "actions.jsonl";
/// Maximum retained actions.
pub const MAX_ACTIONS: usize = 10_000;
/// Maximum file bytes.
pub const MAX_BYTES: u64 = 8 * 1024 * 1024;
/// Maximum page limit.
pub const MAX_LIMIT: i64 = 200;
/// Maximum text field length.
pub const MAX_TEXT: usize = 1_000;
/// Allowed `client` values.
pub const CLIENTS: &[&str] = &["cli", "mcp", "system"];
/// Allowed `outcome` values.
pub const OUTCOMES: &[&str] = &[
    "pending", "applied", "approved", "denied", "stale", "refused", "deleted",
];

const KEYS: &[&str] = &[
    "sequence",
    "occurred_at",
    "client",
    "action",
    "outcome",
    "detail",
    "proposal_id",
    "operation_id",
];

// The process interface: set once at startup.
static INTERFACE: AtomicU8 = AtomicU8::new(2); // 0=cli, 1=mcp, 2=library/other

/// `core.runlog.set_interface`: name the running face for journaling.
pub fn set_interface(name: &str) {
    INTERFACE.store(
        match name {
            "cli" => 0,
            "mcp" => 1,
            _ => 2,
        },
        Ordering::Relaxed,
    );
}

/// `current_interface`: resolve the process interface; unknown values become `system`.
pub fn current_interface() -> &'static str {
    match INTERFACE.load(Ordering::Relaxed) {
        0 => "cli",
        1 => "mcp",
        _ => "system",
    }
}

fn bounded_text(raw: &str, label: &str, maximum: usize, empty: bool) -> ProjectsResult<String> {
    if raw.len() > maximum || (!empty && raw.trim().is_empty()) {
        return Err(crate::error::value(format!("{label} must be bounded text")));
    }
    if raw.contains('\x00') {
        return Err(crate::error::value(format!(
            "{label} must not contain NUL bytes"
        )));
    }
    Ok(raw.to_owned())
}

fn validate(row: &Value) -> ProjectsResult<Value> {
    let object = row.as_object().filter(|o| {
        let keys: std::collections::BTreeSet<&str> = o.keys().map(String::as_str).collect();
        let full: std::collections::BTreeSet<&str> = KEYS.iter().copied().collect();
        let mut without_op = full.clone();
        without_op.remove("operation_id");
        keys == full || keys == without_op
    });
    let Some(object) = object else {
        return Err(crate::error::value(
            "project action has an unsupported shape",
        ));
    };
    if object["sequence"].as_i64().is_none_or(|s| s < 1) || object["sequence"].is_boolean() {
        return Err(crate::error::value("project action sequence is invalid"));
    }
    if !CLIENTS.contains(&object["client"].as_str().unwrap_or_default())
        || !OUTCOMES.contains(&object["outcome"].as_str().unwrap_or_default())
    {
        return Err(crate::error::value(
            "project action client or outcome is unsupported",
        ));
    }
    bounded_text(
        object["action"].as_str().unwrap_or_default(),
        "action",
        128,
        false,
    )?;
    bounded_text(
        object["detail"].as_str().unwrap_or_default(),
        "detail",
        MAX_TEXT,
        true,
    )?;
    for field in ["proposal_id", "operation_id"] {
        if !object.get(field).unwrap_or(&Value::Null).is_null() {
            bounded_text(object[field].as_str().unwrap_or_default(), field, 64, false)?;
        }
    }
    let stamp = pytime::fromisoformat(
        &object["occurred_at"]
            .as_str()
            .unwrap_or_default()
            .replace('Z', "+00:00"),
    )
    .map_err(|_| crate::error::value("project action time must be RFC3339 UTC"))?;
    if !stamp.is_utc() {
        return Err(crate::error::value(
            "project action time must be RFC3339 UTC",
        ));
    }
    let mut normalized = object.clone();
    normalized.insert(
        "operation_id".into(),
        object.get("operation_id").cloned().unwrap_or(Value::Null),
    );
    Ok(Value::Object(normalized))
}

pub(crate) fn path(root: &Path) -> ProjectsResult<PathBuf> {
    let path = root.join(FILE);
    if path.is_symlink() {
        return Err(crate::error::value("project action log is unsafe"));
    }
    Ok(path)
}

fn lock(root: &Path) -> ProjectsResult<std::fs::File> {
    let file = locks::open_lock(&root.join(format!(".{FILE}.lock")), true)
        .map_err(|_| crate::error::value("project action log is busy or locking is unavailable"))?;
    locks::lock_exclusive(&file)
        .map_err(|_| crate::error::value("project action log is busy or locking is unavailable"))?;
    Ok(file)
}

pub(crate) fn parse<R: BufRead>(reader: R, size: u64) -> ProjectsResult<Vec<Value>> {
    if size > MAX_BYTES {
        return Err(crate::error::value(
            "project action log exceeds its byte limit",
        ));
    }
    let mut rows = Vec::new();
    for (index, line) in reader.lines().enumerate() {
        let number = index + 1;
        let line = line.map_err(ProjectsError::Io)?;
        let row: Value = serde_json::from_str(&line).map_err(|e| {
            crate::error::value(format!("project action line {number} is invalid: {e}"))
        })?;
        let row = validate(&row).map_err(|e| {
            crate::error::value(format!("project action line {number} is invalid: {e}"))
        })?;
        if row["sequence"].as_i64() != Some(rows.len() as i64 + 1) {
            return Err(crate::error::value(format!(
                "project action line {number} has a noncontiguous sequence"
            )));
        }
        if rows.len() >= MAX_ACTIONS {
            return Err(crate::error::value(
                "project action log exceeds its entry limit",
            ));
        }
        rows.push(row);
    }
    Ok(rows)
}

/// `_read_snapshot_file`: one bounded immutable snapshot of an already immutable file.
fn read_snapshot_file(path: &Path) -> ProjectsResult<Vec<Value>> {
    if path.is_symlink() {
        return Err(crate::error::value("project action log is unsafe"));
    }
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .no_follow()
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(ProjectsError::Io(e)),
    };
    let before = file.metadata().map_err(ProjectsError::Io)?;
    let before_id = file_identity(&file).map_err(ProjectsError::Io)?;
    let named_id = link_identity(path)
        .map_err(|_| crate::error::value("project action log changed during read"))?;
    if (before_id.0, before_id.1) != (named_id.0, named_id.1) {
        return Err(crate::error::value(
            "project action log changed during read",
        ));
    }
    if !before.is_file() || before_id.2 != 1 {
        return Err(crate::error::value("project action log is unsafe"));
    }
    if before.len() > MAX_BYTES {
        return Err(crate::error::value(
            "project action log exceeds its byte limit",
        ));
    }
    let mut content = Vec::with_capacity(before.len() as usize);
    let mut handle = &file;
    let mut remaining = before.len();
    while remaining > 0 {
        let mut chunk = vec![0u8; remaining.min(64 * 1024) as usize];
        let read = handle.read(&mut chunk).map_err(ProjectsError::Io)?;
        if read == 0 {
            return Err(crate::error::value(
                "project action log changed during read",
            ));
        }
        content.extend_from_slice(&chunk[..read]);
        remaining -= read as u64;
    }
    let after = file.metadata().map_err(ProjectsError::Io)?;
    let after_id = file_identity(&file).map_err(ProjectsError::Io)?;
    if (before_id.0, before_id.1, before.len()) != (after_id.0, after_id.1, after.len()) {
        return Err(crate::error::value(
            "project action log changed during read",
        ));
    }
    if !content.is_empty() && !content.ends_with(b"\n") {
        return Err(crate::error::value(
            "project action log is busy; retry the read",
        ));
    }
    let text = String::from_utf8(content).map_err(|_| {
        crate::error::value("project action log is busy or invalid; retry the read")
    })?;
    parse(BufReader::new(text.as_bytes()), text.len() as u64)
}

fn read_snapshot(root: &Path, path: &Path) -> ProjectsResult<Vec<Value>> {
    let lock_path = root.join(format!(".{FILE}.lock"));
    let held = if lock_path.exists() || lock_path.is_symlink() {
        let file = locks::open_lock(&lock_path, false)
            .map_err(|_| crate::error::value("project action log is busy; retry the read"))?;
        locks::lock_exclusive(&file)
            .map_err(|_| crate::error::value("project action log is busy; retry the read"))?;
        Some(file)
    } else {
        None
    };
    let result = read_snapshot_file(path);
    if let Some(file) = held {
        let _ = locks::unlock(&file);
    }
    result
}

/// `record`: append one action with a server-selected client value.
pub fn record(
    directory: &Path,
    client: &str,
    action: &str,
    outcome: &str,
    detail: &str,
    proposal_id: Option<&str>,
    operation_id: Option<&str>,
) -> ProjectsResult<Value> {
    if !CLIENTS.contains(&client) || !OUTCOMES.contains(&outcome) {
        return Err(crate::error::value(
            "project action client or outcome is unsupported",
        ));
    }
    let action = bounded_text(action, "action", 128, false)?;
    let detail = bounded_text(detail, "detail", MAX_TEXT, true)?;
    let proposal_id = proposal_id
        .map(|p| bounded_text(p, "proposal_id", 64, false))
        .transpose()?;
    let (root, _project) = workspace_load(directory)?;
    record_root(
        &root,
        client,
        &action,
        outcome,
        &detail,
        proposal_id.as_deref(),
        operation_id,
    )
}

/// `record_root`: append to a previously validated project directory.
pub fn record_root(
    root: &Path,
    client: &str,
    action: &str,
    outcome: &str,
    detail: &str,
    proposal_id: Option<&str>,
    operation_id: Option<&str>,
) -> ProjectsResult<Value> {
    if !CLIENTS.contains(&client) || !OUTCOMES.contains(&outcome) {
        return Err(crate::error::value(
            "project action client or outcome is unsupported",
        ));
    }
    let action = bounded_text(action, "action", 128, false)?;
    let detail = bounded_text(detail, "detail", MAX_TEXT, true)?;
    let proposal_id = proposal_id
        .map(|p| bounded_text(p, "proposal_id", 64, false))
        .transpose()?;
    let operation_id = operation_id
        .map(|p| bounded_text(p, "operation_id", 64, false))
        .transpose()?;
    let path = path(root)?;
    let _guard = crate::runtime::project_write_lock(root, 5.0)?;
    let held = lock(root)?;
    let result = (|| -> ProjectsResult<Value> {
        let mut stream = io(std::fs::OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&path))?;
        let size = stream.metadata().map_err(ProjectsError::Io)?.len();
        let rows = parse(BufReader::new(&mut stream), size)?;
        if rows.len() >= MAX_ACTIONS {
            return Err(crate::error::value("project action log is full"));
        }
        let row = json!({
            "sequence": rows.len() + 1,
            "occurred_at": pytime::zulu(),
            "client": client,
            "action": action,
            "outcome": outcome,
            "detail": detail,
            "proposal_id": proposal_id,
            "operation_id": operation_id,
        });
        let line = pyjson::dumps_compact(&row) + "\n";
        let size = stream.metadata().map_err(ProjectsError::Io)?.len();
        if size + line.len() as u64 > MAX_BYTES {
            return Err(crate::error::value("project action log is full"));
        }
        stream
            .write_all(line.as_bytes())
            .map_err(ProjectsError::Io)?;
        stream.flush().map_err(ProjectsError::Io)?;
        Ok(json!({"ok": true, "action": row}))
    })();
    let _ = locks::unlock(&held);
    result
}

/// `resolve_pending`: replace one reserved pending row with its terminal result atomically.
pub fn resolve_pending(
    directory: &Path,
    sequence: i64,
    outcome: &str,
    detail: &str,
    proposal_id: Option<&str>,
    operation_id: Option<&str>,
) -> ProjectsResult<Value> {
    let (root, _project) = workspace_load(directory)?;
    resolve_pending_root(&root, sequence, outcome, detail, proposal_id, operation_id)
}

/// `resolve_pending_root`: resolve inside an already validated root.
pub fn resolve_pending_root(
    root: &Path,
    sequence: i64,
    outcome: &str,
    detail: &str,
    proposal_id: Option<&str>,
    operation_id: Option<&str>,
) -> ProjectsResult<Value> {
    if sequence < 1 {
        return Err(crate::error::value(
            "action sequence must be a positive integer",
        ));
    }
    if !OUTCOMES.contains(&outcome) || outcome == "pending" {
        return Err(crate::error::value(
            "action resolution requires a terminal outcome",
        ));
    }
    let detail = bounded_text(detail, "detail", MAX_TEXT, true)?;
    let proposal_id = proposal_id
        .map(|p| bounded_text(p, "proposal_id", 64, false))
        .transpose()?;
    let operation_id = operation_id
        .map(|p| bounded_text(p, "operation_id", 64, false))
        .transpose()?;
    let path = path(root)?;
    let _guard = crate::runtime::project_write_lock(root, 5.0)?;
    let held = lock(root)?;
    let result = (|| -> ProjectsResult<Value> {
        if !path.exists() {
            return Err(crate::error::value("pending action log row is missing"));
        }
        let text = io(std::fs::read_to_string(&path))?;
        let mut rows = parse(BufReader::new(text.as_bytes()), text.len() as u64)?;
        let index = rows
            .iter()
            .position(|row| row["sequence"].as_i64() == Some(sequence))
            .ok_or_else(|| crate::error::value("pending action log row is missing"))?;
        if let Some(p) = &proposal_id {
            if rows[index]["proposal_id"].as_str() != Some(p.as_str()) {
                return Err(crate::error::value(
                    "pending action proposal identity does not match",
                ));
            }
        }
        if let Some(o) = &operation_id {
            if rows[index]["operation_id"].as_str() != Some(o.as_str()) {
                return Err(crate::error::value(
                    "pending action operation identity does not match",
                ));
            }
        }
        if rows[index]["outcome"].as_str() != Some("pending") {
            if rows[index]["outcome"].as_str() == Some(outcome) {
                return Ok(json!({
                    "ok": true,
                    "action": rows[index],
                    "already_resolved": true,
                }));
            }
            return Err(crate::error::value("action log row is already terminal"));
        }
        let mut row = rows[index].clone();
        row["outcome"] = json!(outcome);
        row["detail"] = json!(detail);
        rows[index] = row.clone();
        let content: String = rows
            .iter()
            .map(|item| pyjson::dumps_compact(item) + "\n")
            .collect();
        if content.len() as u64 > MAX_BYTES {
            return Err(crate::error::value(
                "resolved action log exceeds its byte limit",
            ));
        }
        atomic_write_bytes(&path, content.as_bytes(), None, None, true)
            .map_err(ProjectsError::Io)?;
        Ok(json!({"ok": true, "action": row}))
    })();
    let _ = locks::unlock(&held);
    result
}

/// `record_interface_action`: append an action using the process-owned interface setting.
pub fn record_interface_action(
    directory: &Path,
    action: &str,
    outcome: &str,
    detail: &str,
    proposal_id: Option<&str>,
    operation_id: Option<&str>,
) -> ProjectsResult<Value> {
    record(
        directory,
        current_interface(),
        action,
        outcome,
        detail,
        proposal_id,
        operation_id,
    )
}

/// `page`: read a bounded newest-first page with client, outcome and text filters.
pub fn page(
    directory: &Path,
    offset: i64,
    limit: i64,
    client: Option<&str>,
    outcome: Option<&str>,
    query: &str,
) -> ProjectsResult<Value> {
    if offset < 0 || !(1..=MAX_LIMIT).contains(&limit) {
        return Err(crate::error::value(format!(
            "offset must be nonnegative and limit must be 1..{MAX_LIMIT}"
        )));
    }
    if let Some(c) = client {
        if !CLIENTS.contains(&c) {
            return Err(crate::error::value(
                "project action client filter is unsupported",
            ));
        }
    }
    if let Some(o) = outcome {
        if !OUTCOMES.contains(&o) {
            return Err(crate::error::value(
                "project action outcome filter is unsupported",
            ));
        }
    }
    let query = bounded_text(query, "query", 200, true)?;
    let (root, _project) = workspace_load(directory)?;
    let path = path(&root)?;
    let mut rows = None;
    for attempt in 0..2 {
        match read_snapshot(&root, &path) {
            Ok(r) => {
                rows = Some(r);
                break;
            }
            Err(e) => {
                let msg = e.to_string();
                if !msg.contains("busy") && !msg.contains("changed during read") {
                    return Err(e);
                }
                if attempt == 1 {
                    return Err(e);
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
    let rows = rows.unwrap_or_default();
    let needle = query.to_lowercase();
    let matched: Vec<Value> = rows
        .iter()
        .rev()
        .filter(|row| {
            client.is_none_or(|c| row["client"].as_str() == Some(c))
                && outcome.is_none_or(|o| row["outcome"].as_str() == Some(o))
                && (needle.is_empty()
                    || format!(
                        "{} {} {}",
                        row["action"].as_str().unwrap_or_default(),
                        row["detail"].as_str().unwrap_or_default(),
                        row["proposal_id"].as_str().unwrap_or_default()
                    )
                    .to_lowercase()
                    .contains(&needle))
        })
        .cloned()
        .collect();
    let total = matched.len() as i64;
    Ok(json!({
        "ok": true,
        "revision": rows.last().and_then(|r| r["sequence"].as_i64()).unwrap_or(0),
        "total": total,
        "items": matched.iter().skip(offset as usize).take(limit as usize).cloned().collect::<Vec<_>>(),
        "pagination": {
            "total": total,
            "offset": offset,
            "limit": limit,
            "next_offset": if offset + limit < total { json!(offset + limit) } else { Value::Null },
        },
    }))
}

/// `latest_agent_activity`: latest recorded MCP activity; never claims a live client.
pub fn latest_agent_activity(directory: &Path) -> ProjectsResult<Value> {
    let result = page(directory, 0, MAX_LIMIT, Some("mcp"), None, "")?;
    let latest = result["items"].as_array().and_then(|i| i.first()).cloned();
    Ok(json!({
        "last_seen_at": latest.as_ref().map_or(Value::Null, |l| l["occurred_at"].clone()),
        "source": if latest.is_some() { json!("action_log") } else { Value::Null },
        "heartbeat_source": if latest.as_ref().is_some_and(|l| l["action"].as_str() == Some("agent.heartbeat")) {
            json!("mcp_agent_heartbeat")
        } else {
            Value::Null
        },
    }))
}

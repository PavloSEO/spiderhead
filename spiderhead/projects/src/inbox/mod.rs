//! Durable, local notes and proposed goals for an agent-owned project

use spiderhead_core::filesystem::PrivateMode;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::{json, Map, Value};

use crate::error::{io, ProjectsError, ProjectsResult};
use crate::workspace::load as workspace_load;

mod public;
pub use public::*;

/// Document format.
pub const FORMAT: &str = "spiderhead.project-inbox.v1";
/// Byte ceiling for `inbox.json`.
pub const MAX_BYTES: u64 = 4 * 1024 * 1024;
/// Entry text ceiling.
pub const MAX_TEXT: usize = 8_000;
/// Entry count ceiling.
pub const MAX_ENTRIES: usize = 10_000;
/// Page ceiling.
pub const MAX_PAGE: usize = 100;

fn consumer_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[a-z][a-z0-9._/-]{0,127}\z").unwrap())
}

fn reference_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?:goal|task|scan|finding|section):[A-Za-z0-9._/-]{1,128}\z").unwrap()
    })
}

fn goal_id_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"inbox:[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\z").unwrap()
    })
}

fn text(value: &Value, name: &str, maximum: usize) -> ProjectsResult<String> {
    let original = value.as_str();
    match original {
        Some(raw) if !raw.trim().is_empty() && raw.chars().count() <= maximum => {
            Ok(raw.trim().to_owned())
        }
        _ => Err(crate::error::value(format!(
            "{name} must be nonempty text of at most {maximum} characters"
        ))),
    }
}

fn text_arg(raw: &str, name: &str, maximum: usize) -> ProjectsResult<String> {
    text(&json!(raw), name, maximum)
}

fn consumer(value: &str) -> ProjectsResult<String> {
    if !consumer_re().is_match(value) || value.contains("..") {
        return Err(crate::error::value(
            "consumer must be a bounded stable local identifier",
        ));
    }
    Ok(value.to_owned())
}

fn reference(value: &Value) -> bool {
    let Some(text) = value.as_str() else {
        return false;
    };
    if reference_re().is_match(text) || goal_id_re().is_match(text) {
        return true;
    }
    if !text.starts_with("task:") || text.len() > 256 {
        return false;
    }
    let item_id = &text["task:".len()..];
    let item_id = if let Some(rest) = item_id.strip_prefix("site:") {
        let Some((site_id, item_id)) = rest.split_once('/') else {
            return false;
        };
        match uuid::Uuid::parse_str(site_id) {
            Ok(u) if u.to_string() == site_id => item_id,
            _ => return false,
        }
    } else {
        item_id
    };
    crate::coverage::schema::identifier(&json!(item_id)).is_ok()
}

fn references(value: Option<&Value>) -> ProjectsResult<Vec<String>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let Some(list) = value.as_array() else {
        return Err(crate::error::value(
            "references must be a list of at most 20 stable project references",
        ));
    };
    if list.len() > 20 {
        return Err(crate::error::value(
            "references must be a list of at most 20 stable project references",
        ));
    }
    if list.iter().any(|item| !reference(item)) {
        return Err(crate::error::value(
            "references contain an invalid project reference",
        ));
    }
    let mut seen = Vec::new();
    for item in list {
        let s = item.as_str().unwrap_or_default().to_owned();
        if !seen.contains(&s) {
            seen.push(s);
        }
    }
    Ok(seen)
}

/// `_stamp`: ISO-8601 text that parses to an aware UTC datetime.
fn stamp(value: &Value, label: &str) -> ProjectsResult<()> {
    let Some(raw) = value.as_str() else {
        return Err(crate::error::value(label));
    };
    let parsed = crate::pytime::fromisoformat(&raw.replace('Z', "+00:00"))
        .map_err(|_| crate::error::value(label))?;
    if !parsed.is_aware() || !parsed.is_utc() {
        return Err(crate::error::value(label));
    }
    Ok(())
}

fn triage_receipt(value: &Value) -> ProjectsResult<()> {
    const LABEL: &str = "project inbox triage receipt";
    let Some(object) = value.as_object() else {
        return Err(crate::error::value(format!(
            "{LABEL} has an unsupported shape"
        )));
    };
    let required: std::collections::BTreeSet<&str> = ["kind", "reason", "actor", "recorded_at"]
        .into_iter()
        .collect();
    let optional: std::collections::BTreeSet<&str> =
        ["task_ids", "goal_id", "competitors"].into_iter().collect();
    let keys: std::collections::BTreeSet<&str> = object.keys().map(String::as_str).collect();
    if keys
        .iter()
        .any(|k| !required.contains(k) && !optional.contains(k))
        || !required.is_subset(&keys)
    {
        return Err(crate::error::value(format!(
            "{LABEL} has an unsupported shape"
        )));
    }
    if !["task", "goal", "competitor", "blocked", "rejected"]
        .contains(&value["kind"].as_str().unwrap_or_default())
    {
        return Err(crate::error::value(format!("{LABEL} has an invalid kind")));
    }
    text(&value["reason"], "triage reason", 512)?;
    text(&value["actor"], "triage actor", 128)?;
    if value["recorded_at"].as_str().is_none() {
        return Err(crate::error::value(format!(
            "{LABEL} has an invalid timestamp"
        )));
    }
    stamp(
        &value["recorded_at"],
        &format!("{LABEL} has an invalid timestamp"),
    )?;
    let kind = value["kind"].as_str().unwrap_or_default();
    if let Some(task_ids) = object.get("task_ids") {
        let invalid = task_ids.as_array().is_none_or(|ids| {
            ids.is_empty()
                || ids.len() > 20
                || ids
                    .iter()
                    .map(|i| i.as_str().unwrap_or_default())
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != ids.len()
                || ids
                    .iter()
                    .any(|item| item.as_str().is_none_or(|s| !s.starts_with("custom:")))
        });
        if invalid {
            return Err(crate::error::value(format!("{LABEL} has invalid task ids")));
        }
    }
    if let Some(goal_id) = object.get("goal_id") {
        if goal_id.as_str().is_none_or(|g| !g.starts_with("inbox:")) {
            return Err(crate::error::value(format!(
                "{LABEL} has an invalid goal id"
            )));
        }
    }
    if let Some(competitors) = object.get("competitors") {
        let invalid = competitors.as_array().is_none_or(|c| {
            c.is_empty()
                || c.len() > 20
                || {
                    let mut seen: Vec<&Value> = Vec::new();
                    c.iter().any(|item| {
                        let dup = seen.contains(&item);
                        seen.push(item);
                        dup
                    })
                }
                || c.iter().any(|item| !item.is_string())
        });
        if invalid {
            return Err(crate::error::value(format!(
                "{LABEL} has invalid competitors"
            )));
        }
    }
    let expected: std::collections::BTreeSet<&str> = match kind {
        "task" => ["task_ids"].into_iter().collect(),
        "goal" => ["goal_id"].into_iter().collect(),
        "competitor" => ["competitors"].into_iter().collect(),
        _ => Default::default(),
    };
    let present: std::collections::BTreeSet<&str> = optional
        .iter()
        .filter(|k| object.contains_key(**k))
        .copied()
        .collect();
    if present != expected {
        return Err(crate::error::value(format!(
            "{LABEL} does not match its kind"
        )));
    }
    Ok(())
}

fn entry(value: &Value) -> ProjectsResult<Value> {
    const LABEL: &str = "project inbox entry";
    let required: std::collections::BTreeSet<&str> = [
        "id",
        "kind",
        "text",
        "references",
        "author_role",
        "created_at",
        "delivery",
        "goal_state",
    ]
    .into_iter()
    .collect();
    let optional: std::collections::BTreeSet<&str> = ["triage", "accepted_at", "completed_at"]
        .into_iter()
        .collect();
    let Some(object) = value.as_object() else {
        return Err(crate::error::value(format!(
            "{LABEL} has an unsupported shape"
        )));
    };
    let keys: std::collections::BTreeSet<&str> = object.keys().map(String::as_str).collect();
    if keys
        .iter()
        .any(|k| !required.contains(k) && !optional.contains(k))
        || !required.is_subset(&keys)
    {
        return Err(crate::error::value(format!(
            "{LABEL} has an unsupported shape"
        )));
    }
    if value["id"]
        .as_str()
        .is_none_or(|id| !id.starts_with("inbox:"))
    {
        return Err(crate::error::value(format!("{LABEL} has an invalid id")));
    }
    let kind = value["kind"].as_str().unwrap_or_default();
    if !["note", "proposed_goal", "question"].contains(&kind) {
        return Err(crate::error::value(format!("{LABEL} has an invalid kind")));
    }
    text(&value["text"], "entry text", MAX_TEXT)?;
    references(object.get("references"))?;
    if !["specialist", "agent"].contains(&value["author_role"].as_str().unwrap_or_default()) {
        return Err(crate::error::value(format!(
            "{LABEL} has an invalid author role"
        )));
    }
    stamp(
        &value["created_at"],
        &format!("{LABEL} has an invalid timestamp"),
    )?;
    for key in ["accepted_at", "completed_at"] {
        if object.contains_key(key) {
            stamp(&value[key], &format!("{LABEL} has an invalid timestamp"))?;
        }
    }
    let goal_state = &value["goal_state"];
    if !(goal_state.is_null()
        || ["proposed", "accepted", "completed"].contains(&goal_state.as_str().unwrap_or_default()))
    {
        return Err(crate::error::value(format!(
            "{LABEL} has an invalid goal state"
        )));
    }
    if kind != "proposed_goal" && !goal_state.is_null() {
        return Err(crate::error::value(
            "only proposed goals can have a goal state",
        ));
    }
    if kind == "proposed_goal" && goal_state.is_null() {
        return Err(crate::error::value("proposed goals need a goal state"));
    }
    let delivery = value["delivery"]
        .as_object()
        .filter(|d| d.len() <= 100)
        .ok_or_else(|| crate::error::value("project inbox delivery state is invalid"))?;
    for (name, receipt) in delivery {
        consumer(name)?;
        let receipt = receipt
            .as_object()
            .filter(|r| r.keys().all(|k| k == "read_at" || k == "acknowledged_at"))
            .ok_or_else(|| crate::error::value("project inbox delivery receipt is invalid"))?;
        if receipt.values().any(|stamp| !stamp.is_string()) {
            return Err(crate::error::value(
                "project inbox delivery receipt is invalid",
            ));
        }
    }
    let triage = object
        .get("triage")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if object
        .get("triage")
        .is_some_and(|t| !t.is_array() || t.as_array().unwrap().len() > 20)
        || triage.len() > 20
    {
        return Err(crate::error::value(
            "project inbox triage history is invalid",
        ));
    }
    for outcome in &triage {
        triage_receipt(outcome)?;
    }
    Ok(value.clone())
}

fn document(root: &Path, project: &Value) -> ProjectsResult<Value> {
    let path = root.join("inbox.json");
    if !path.exists() {
        return Ok(json!({
            "format": FORMAT,
            "revision": 0,
            "project_uuid": project["project_uuid"],
            "entries": [],
        }));
    }
    let meta = io(std::fs::symlink_metadata(&path))?;
    if meta.file_type().is_symlink() || !meta.is_file() || meta.len() > MAX_BYTES {
        return Err(crate::error::value(
            "project inbox is missing, unsafe, or exceeds its byte limit",
        ));
    }
    let document: Value = serde_json::from_str(
        &std::fs::read_to_string(&path)
            .map_err(|_| crate::error::value("project inbox is not valid JSON"))?,
    )
    .map_err(|_| crate::error::value("project inbox is not valid JSON"))?;
    let object = document.as_object().filter(|o| {
        o.keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>()
            == ["format", "revision", "project_uuid", "entries"]
                .into_iter()
                .collect()
    });
    if object.is_none() {
        return Err(crate::error::value(
            "project inbox has an unsupported shape",
        ));
    }
    if document["format"].as_str() != Some(FORMAT)
        || document["project_uuid"] != project["project_uuid"]
    {
        return Err(crate::error::value(
            "project inbox belongs to a different project or format",
        ));
    }
    if document["revision"].as_i64().is_none() || document["revision"].as_i64().unwrap() < 0 {
        return Err(crate::error::value("project inbox has an invalid revision"));
    }
    let entries = document["entries"]
        .as_array()
        .filter(|e| e.len() <= MAX_ENTRIES)
        .ok_or_else(|| crate::error::value("project inbox has too many entries"))?;
    let mut ids = std::collections::BTreeSet::new();
    for item in entries {
        let checked = entry(item)?;
        if !ids.insert(checked["id"].as_str().unwrap_or_default().to_owned()) {
            return Err(crate::error::value("project inbox has duplicate entry ids"));
        }
    }
    Ok(document)
}

fn write(root: &Path, document: &Value) -> ProjectsResult<()> {
    let payload = crate::pyjson::dumps_indent2(document) + "\n";
    if payload.len() as u64 > MAX_BYTES {
        return Err(crate::error::value("project inbox exceeds its byte limit"));
    }
    spiderhead_core::filesystem::atomic_write_bytes(
        &root.join("inbox.json"),
        payload.as_bytes(),
        None,
        Some(0o600),
        true,
    )
    .map_err(ProjectsError::Io)
}

fn read_document(directory: &Path) -> ProjectsResult<(PathBuf, Value)> {
    let (root, project) = workspace_load(directory)?;
    let document = document(&root, &project)?;
    Ok((root, document))
}

/// `_transaction`: hold `.inbox.lock` for a read-modify-write pass.
fn transaction<T>(
    directory: &Path,
    expected_revision: Option<i64>,
    body: impl FnOnce(&Path, &mut Value) -> ProjectsResult<T>,
) -> ProjectsResult<T> {
    let (root, project) = workspace_load(directory)?;
    let lock_path = root.join(".inbox.lock");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .private_mode(0o600)
        .open(&lock_path)
        .map_err(ProjectsError::Io)?;
    crate::locks::lock_exclusive_wait(&file).map_err(ProjectsError::Io)?;
    let outcome = (|| {
        let mut document = document(&root, &project)?;
        if let Some(expected) = expected_revision {
            if document["revision"].as_i64() != Some(expected) {
                return Err(crate::error::value(format!(
                    "inbox revision conflict: current revision is {}",
                    document["revision"]
                )));
            }
        }
        let before = document.clone();
        let result = body(&root, &mut document)?;
        if document != before {
            let revision = document["revision"].as_i64().unwrap_or(0) + 1;
            document["revision"] = json!(revision);
            write(&root, &document)?;
        }
        Ok(result)
    })();
    let _ = crate::locks::unlock(&file);
    outcome
}

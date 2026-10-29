//! Project journal: one write path for crawls, reports, checklist attempts and agent
//! actions.
//!
//! Every entry is a validated structured event in `events.jsonl` and one bullet in
//! `log.md`. Automatic hooks call [`note`] best-effort so a journal problem never fails the
//! step it describes.

use std::io::Write;
use std::path::Path;

use serde_json::{json, Value};

use crate::error::{io, ProjectsError, ProjectsResult};
use crate::workspace::load as workspace_load;

fn bullet(event: &Value, source: &str, actor: &str) -> String {
    let text: String = event["text"]
        .as_str()
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "- {} · {}/{} · {}\n",
        event["occurred_at"].as_str().unwrap_or_default(),
        source,
        actor,
        text
    )
}

/// `record`: append one event and mirror it to `log.md`.
pub fn record(directory: &Path, source: &str, actor: &str, text: &str) -> ProjectsResult<Value> {
    let (root, _project) = workspace_load(directory)?;
    let _guard = crate::runtime::project_write_lock(&root, 5.0)?;
    if !crate::event_log::SOURCES.contains(&source) || !crate::event_log::ACTORS.contains(&actor) {
        return Err(crate::error::value("event source or actor is unsupported"));
    }
    let text = crate::event_log::text(text)?.trim().to_owned();
    let event = crate::event_log::append_within_lock(&root, source, actor, &text)?["event"].clone();
    let mut stream = io(std::fs::OpenOptions::new()
        .append(true)
        .open(root.join("log.md")))?;
    io(stream.write_all(bullet(&event, source, actor).as_bytes()))?;
    Ok(json!({"ok": true, "event": event}))
}

/// `note`: best-effort record for automatic hooks; a failure is swallowed.
pub fn note(directory: &Path, source: &str, actor: &str, text: &str) {
    match record(directory, source, actor, text) {
        Ok(_) | Err(ProjectsError::Io(_)) | Err(ProjectsError::Value(_)) => {}
        Err(_) => {}
    }
}

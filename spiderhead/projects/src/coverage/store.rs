//! `coverage.json` disk reads and writer-lock diagnostics.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::document::validate_document;
use super::schema::MAX_BYTES;
use crate::error::{io, ProjectsResult};

/// `_read`: the only code that reads `coverage.json` from disk.
pub fn read(root: &Path, project: &Value) -> ProjectsResult<Option<Value>> {
    let path = root.join("coverage.json");
    if !path.exists() && !path.is_symlink() {
        return Ok(None);
    }
    let meta = io(path.symlink_metadata())?;
    if meta.file_type().is_symlink() || !meta.is_file() || meta.len() > MAX_BYTES as u64 {
        return Err(crate::error::value(
            "coverage.json is unsafe or exceeds its byte limit",
        ));
    }
    let text = io(std::fs::read_to_string(&path))?;
    let document: Value = serde_json::from_str(&text)
        .map_err(|_| crate::error::value("coverage.json is not valid JSON"))?;
    validate_document(&document, project).map(Some)
}

/// `_busy_reason`: name the live holder of the writer lock.
pub fn busy_reason(root: &Path, lock: &PathBuf) -> String {
    let holder = crate::run_observation::live_holder(root).ok().flatten();
    if let Some(holder) = holder {
        return format!(
            "coverage writer is busy: {holder} is running on this project; wait for it to finish"
        );
    }
    let alive = std::fs::read_to_string(lock)
        .ok()
        .and_then(|text| text.trim().parse::<i32>().ok())
        .is_some_and(crate::run_observation::pid_exists);
    if !alive {
        return "coverage writer is busy; no live holder is recorded, so .coverage.lock is a \
                leftover from an interrupted process"
            .to_owned();
    }
    let pid = std::fs::read_to_string(lock)
        .ok()
        .and_then(|t| t.trim().parse::<i32>().ok())
        .unwrap_or_default();
    format!("coverage writer is busy: process pid {pid} holds .coverage.lock")
}

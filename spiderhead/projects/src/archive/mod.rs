//! Portable project archive: one zip with a SHA-256 manifest, restorable on any OS
//!
//! Project-local archive set: project and settings metadata, bounded action/event
//! histories, inbox/coverage/export stores, top-level SQLite stores, and `scans/` and
//! `reports/` artifacts.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use rusqlite::OptionalExtension;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::error::{value, ProjectsResult};
use crate::workspace::{directory as safe_directory, load as workspace_load};

mod scratch;
pub use scratch::*;

/// Archive manifest format.
pub const ARCHIVE_FORMAT: &str = "spiderhead.project-archive.v1";
/// Archive version.
pub const ARCHIVE_VERSION: i64 = 1;
/// Manifest member name.
pub const MANIFEST: &str = "manifest.json";
const FILES: &[&str] = &[
    "project.json",
    "log.md",
    "inbox.json",
    "coverage.json",
    "events.jsonl",
    "sources.json",
    "finding-views.json",
    "project-settings.json",
    "settings-proposals.json",
    "actions.jsonl",
    "schedule.json",
    "schedule-state.json",
    "exports.json",
];
const PROJECT_STATE: &[&str] = &[
    "events.jsonl",
    "sources.json",
    "finding-views.json",
    "project-settings.json",
    "settings-proposals.json",
    "actions.jsonl",
    "schedule.json",
    "schedule-state.json",
    "exports.json",
];
const DIRECTORIES: &[&str] = &["scans", "reports"];
const SQLITE: &[&str] = &[".sqlite", ".db"];

/// `_CREDENTIAL` name check.
fn credential_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    [
        "secret",
        "token",
        "passw",
        "cookie",
        "credential",
        "api_key",
        "api-key",
        "apikey",
        "private_key",
        "private-key",
        ".env",
        "netrc",
        ".pem",
        ".key",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// `_scan_timestamp`: a retained scan's stable age key, or `None` for other databases.
fn scan_timestamp(path: &Path) -> Option<(String, String)> {
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let con =
        rusqlite::Connection::open_with_flags(resolved, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()?;
    let row: Option<(String, String)> = con
        .query_row(
            "SELECT scan_uuid,COALESCE(NULLIF(finished_at,''),created_at) \
             FROM scan WHERE singleton=1",
            [],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                ))
            },
        )
        .optional()
        .ok()
        .flatten();
    row
}

/// `_validate_project_state`: run one retained state file through its bounded
/// read-only project validator.
fn validate_project_state(root: &Path, rel: &str) -> ProjectsResult<()> {
    match rel {
        "events.jsonl" => {
            crate::event_log::page(root, 0, 1, None, "", None, false, 0)?;
        }
        "sources.json" => {
            crate::bindings::list_bindings(root, None)?;
        }
        "finding-views.json" => {
            crate::finding_views::list_views(root)?;
        }
        "project-settings.json" => {
            crate::project_settings::read_settings(root)?;
        }
        "settings-proposals.json" => {
            crate::project_settings::list_proposals(root, 0, 1)?;
        }
        "actions.jsonl" => {
            let path = crate::action_log::path(root)?;
            if path.is_symlink() {
                return Err(value("actions.jsonl must be a regular project-local file"));
            }
            if path.exists() {
                let size = fs::metadata(&path)?.len();
                let reader = std::io::BufReader::new(File::open(&path)?);
                crate::action_log::parse(reader, size)?;
            }
        }
        "schedule.json" | "schedule-state.json" => {
            let (_schedule_root, project, settings, state) = crate::schedule::open(root)?;
            crate::schedule::validate(&settings)?;
            let state_keys_ok = state
                .as_object()
                .map(|o| {
                    o.keys()
                        .map(String::as_str)
                        .collect::<std::collections::BTreeSet<_>>()
                        == [
                            "format",
                            "project_uuid",
                            "revision",
                            "last_window",
                            "last_run_at",
                            "last_result",
                            "resume_artifact",
                        ]
                        .into_iter()
                        .collect()
                })
                .unwrap_or(false);
            let state_ok = state_keys_ok
                && state["format"].as_str() == Some(crate::schedule::STATE_FORMAT)
                && state["project_uuid"] == project["project_uuid"]
                && state["revision"].is_i64()
                && state["revision"].as_i64().is_some_and(|r| r >= 0)
                && (state["last_window"].is_null() || state["last_window"].is_string())
                && (state["last_run_at"].is_null() || state["last_run_at"].is_string())
                && (state["last_result"].is_null()
                    || (state["last_result"].is_array()
                        && state["last_result"]
                            .as_array()
                            .is_some_and(|l| l.iter().all(|i| i.is_object()))))
                && (state["resume_artifact"].is_null()
                    || (state["resume_artifact"]
                        .as_str()
                        .is_some_and(|s| !s.is_empty() && s.chars().count() <= 8192)))
                && (state["last_result"].is_null()
                    || state["last_result"].as_array().map_or(0, Vec::len) <= 100);
            if !state_ok {
                return Err(value("schedule-state.json has an unsupported shape"));
            }
            for field in ["last_window", "last_run_at"] {
                let value_ref = &state[field];
                if value_ref.is_null() {
                    continue;
                }
                let text = value_ref.as_str().unwrap_or_default();
                if field == "last_window" {
                    if text.parse::<jiff::civil::Date>().is_err() {
                        return Err(value("schedule-state.json last_window date is invalid"));
                    }
                    continue;
                }
                let stamp = parse_utc_stamp(text).map_err(|_| {
                    value(format!("schedule-state.json {field} timestamp is invalid"))
                })?;
                if !stamp {
                    return Err(value(format!(
                        "schedule-state.json {field} timestamp must be UTC"
                    )));
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// `_validate_project_stores`: consume each present local store through its
/// authoritative read validator.
fn validate_project_stores(root: &Path) -> ProjectsResult<()> {
    let (_root, project) = workspace_load(root)?;
    if root.join("project-settings.json").exists() {
        crate::project_settings::read_settings(root)?;
    }
    if root.join("settings-proposals.json").exists() {
        crate::project_settings::list_proposals(root, 0, 1)?;
    }
    if root.join("events.jsonl").exists() {
        crate::event_log::page(root, 0, 1, None, "", None, false, 0)?;
    }
    if root.join("actions.jsonl").exists() {
        if root.join("actions.jsonl").is_symlink() {
            return Err(value("actions.jsonl must be a regular project-local file"));
        }
        validate_project_state(root, "actions.jsonl")?;
    }
    if root.join("coverage.json").exists() {
        crate::coverage::store::read(root, &project)?;
    }
    if root.join("inbox.json").exists() {
        crate::inbox::list_entries(root, "archive-validation", 0, 1, true)?;
    }
    if root.join("exports.json").exists() {
        crate::exports::exports_page(root, 0, 1)?;
    }
    Ok(())
}

fn walk(base: &Path, mut out: Vec<PathBuf>) -> Vec<PathBuf> {
    if let Ok(entries) = fs::read_dir(base) {
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir() && !p.is_symlink())
            .filter(|p| {
                !p.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .starts_with('.')
            })
            .collect();
        dirs.sort();
        let mut files: Vec<PathBuf> = fs::read_dir(base)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| {
                        !p.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .starts_with('.')
                            && !p.is_dir()
                    })
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        for file in files {
            out.push(file);
        }
        for dir in dirs {
            out = walk(&dir, out);
        }
    }
    out
}

/// `take`: admit one path or record the skip reason.
fn take(included: &mut Vec<(String, PathBuf)>, skipped: &mut Vec<Value>, rel: &str, path: &Path) {
    if credential_name(rel) {
        skipped.push(json!({"path": rel, "reason": "credential-like name"}));
    } else if path.is_symlink() || !path.is_file() {
        skipped.push(json!({"path": rel, "reason": "not a regular file"}));
    } else if rel.ends_with("-wal") || rel.ends_with("-shm") {
        skipped.push(json!({
            "path": rel,
            "reason": "SQLite companion; snapshot is taken instead",
        }));
    } else {
        included.push((rel.to_owned(), path.to_path_buf()));
    }
}

/// `(relative path, absolute path)` archive plan entries.
type PlanEntries = Vec<(String, PathBuf)>;

/// `_plan`: `(archived entries, skipped entries)` relative to the project root.
fn plan(root: &Path, keep_newest_scans: Option<i64>) -> ProjectsResult<(PlanEntries, Vec<Value>)> {
    let mut included: PlanEntries = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    let mut entries: Vec<PathBuf> = fs::read_dir(root)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort_by_key(|p| {
        p.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    });
    for entry in entries {
        let name = entry
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        if FILES.contains(&name.as_str())
            || (SQLITE.iter().any(|s| name.ends_with(s)) && entry.is_file())
        {
            take(&mut included, &mut skipped, &name, &entry);
        } else if !DIRECTORIES.contains(&name.as_str()) && !name.starts_with('.') {
            skipped.push(json!({"path": name, "reason": "outside the archive set"}));
        }
    }
    for name in DIRECTORIES {
        let base = root.join(name);
        for path in walk(&base, Vec::new()) {
            let rel = path
                .strip_prefix(root)
                .map(|r| r.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            take(&mut included, &mut skipped, &rel, &path);
        }
    }
    for (rel, _) in &included {
        if PROJECT_STATE.contains(&rel.as_str()) {
            validate_project_state(root, rel)?;
        }
    }
    if let Some(keep) = keep_newest_scans {
        let mut scans: Vec<(String, PathBuf, (String, String))> = included
            .iter()
            .filter(|(rel, _)| {
                rel.starts_with("scans/") && SQLITE.iter().any(|s| rel.to_lowercase().ends_with(s))
            })
            .filter_map(|(rel, path)| {
                scan_timestamp(path).map(|stamp| (rel.clone(), path.clone(), stamp))
            })
            .collect();
        scans.sort_by(|a, b| a.2.cmp(&b.2).then(a.0.cmp(&b.0)));
        let newest: std::collections::BTreeSet<String> = scans
            .iter()
            .skip(scans.len().saturating_sub(keep.max(0) as usize))
            .map(|(rel, _, _)| rel.clone())
            .collect();
        let omitted: std::collections::BTreeSet<String> = scans
            .iter()
            .map(|(rel, _, _)| rel.clone())
            .filter(|rel| !newest.contains(rel))
            .collect();
        included.retain(|(rel, _)| !omitted.contains(rel));
        for rel in &omitted {
            skipped.push(json!({
                "path": rel,
                "reason": format!("outside keep_newest_scans={keep}"),
            }));
        }
    }
    Ok((included, skipped))
}

fn hex64(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_file(path: &Path) -> ProjectsResult<(String, u64)> {
    let mut digest = Sha256::new();
    let mut size = 0u64;
    let mut stream = File::open(path)?;
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
        size += read as u64;
    }
    Ok((hex64(&digest.finalize()), size))
}

/// `_snapshot`: copy a live SQLite store through the backup API so WAL contents
/// are included.
fn snapshot(source: &Path, destination: &Path) -> ProjectsResult<()> {
    let resolved = source
        .canonicalize()
        .unwrap_or_else(|_| source.to_path_buf());
    let src =
        rusqlite::Connection::open_with_flags(resolved, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| value(e.to_string()))?;
    let mut dst = rusqlite::Connection::open(destination).map_err(|e| value(e.to_string()))?;
    let backup = rusqlite::backup::Backup::new(&src, &mut dst).map_err(|e| value(e.to_string()))?;
    backup
        .run_to_completion(5, std::time::Duration::from_millis(10), None)
        .map_err(|e| value(e.to_string()))?;
    Ok(())
}

/// `_exclude_html_bodies`: strip stored page documents from an archive snapshot
/// while preserving evidence tables.
fn exclude_html_bodies(path: &Path, older_than_days: Option<i64>) -> ProjectsResult<i64> {
    let con = rusqlite::Connection::open(path).map_err(|e| value(e.to_string()))?;
    let tables: std::collections::BTreeSet<String> = {
        let mut stmt = con
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .map_err(|e| value(e.to_string()))?;
        let mapped = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| value(e.to_string()))?;
        mapped.filter_map(|r| r.ok()).collect()
    };
    if !["scan", "documents", "responses", "bodies"]
        .iter()
        .all(|t| tables.contains(*t))
    {
        return Ok(0);
    }
    let header: Option<String> = con
        .query_row(
            "SELECT format_version FROM scan WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| value(e.to_string()))?;
    if header
        .as_deref()
        .is_none_or(|v| !spiderhead_core::artifacts::SCAN_FORMATS.contains(&v))
    {
        return Ok(0);
    }
    let before: i64 = con
        .query_row("SELECT COUNT(*) FROM bodies", [], |row| row.get(0))
        .map_err(|e| value(e.to_string()))?;
    let (condition, cutoff): (String, Option<String>) = if let Some(days) = older_than_days {
        let cutoff = crate::pytime::time_seconds() - (days as f64) * 86400.0;
        (
            "body_sha256 IS NOT NULL AND captured_at<? AND (fidelity='serialized_dom' \
             OR EXISTS (SELECT 1 FROM responses r WHERE \
             r.response_id=documents.source_response_id AND lower(r.content_type) LIKE '%html%'))"
                .into(),
            Some(crate::pytime::epoch_z(cutoff)),
        )
    } else {
        (
            "body_sha256 IS NOT NULL AND (fidelity='serialized_dom' OR EXISTS \
             (SELECT 1 FROM responses r WHERE r.response_id=documents.source_response_id \
             AND lower(r.content_type) LIKE '%html%'))"
                .into(),
            None,
        )
    };
    if let Some(cutoff_at) = &cutoff {
        con.execute_batch("CREATE TEMP TABLE archive_omitted_body_hashes(sha256 TEXT PRIMARY KEY)")
            .map_err(|e| value(e.to_string()))?;
        con.execute(
            &format!(
                "INSERT OR IGNORE INTO archive_omitted_body_hashes \
                 SELECT body_sha256 FROM documents WHERE {condition}"
            ),
            [cutoff_at.as_str()],
        )
        .map_err(|e| value(e.to_string()))?;
    }
    match &cutoff {
        Some(cutoff_at) => {
            con.execute(
                &format!(
                    "UPDATE documents SET body_sha256=NULL,body_state='unavailable',\
                     body_reason='not_in_corpus' WHERE {condition}"
                ),
                [cutoff_at.as_str()],
            )
            .map_err(|e| value(e.to_string()))?;
        }
        None => {
            con.execute(
                &format!(
                    "UPDATE documents SET body_sha256=NULL,body_state='unavailable',\
                     body_reason='not_in_corpus' WHERE {condition}"
                ),
                [],
            )
            .map_err(|e| value(e.to_string()))?;
        }
    }
    if let Some(cutoff_at) = &cutoff {
        con.execute(
            "UPDATE responses SET body_sha256=NULL,body_state='unavailable',\
             body_reason='not_in_corpus' WHERE body_sha256 IS NOT NULL AND purpose='page' \
             AND lower(content_type) LIKE '%html%' AND (received_at<? OR body_sha256 IN \
             (SELECT sha256 FROM archive_omitted_body_hashes)) AND NOT EXISTS \
             (SELECT 1 FROM documents d WHERE d.source_response_id=responses.response_id \
             AND d.body_sha256 IS NOT NULL)",
            [cutoff_at.as_str()],
        )
        .map_err(|e| value(e.to_string()))?;
    } else {
        con.execute(
            "UPDATE responses SET body_sha256=NULL,body_state='unavailable',\
             body_reason='not_in_corpus' WHERE body_sha256 IS NOT NULL AND purpose='page' \
             AND lower(content_type) LIKE '%html%'",
            [],
        )
        .map_err(|e| value(e.to_string()))?;
    }
    con.execute(
        "DELETE FROM bodies WHERE NOT EXISTS \
         (SELECT 1 FROM documents WHERE documents.body_sha256=bodies.sha256) \
         AND NOT EXISTS (SELECT 1 FROM responses WHERE responses.body_sha256=bodies.sha256)",
        [],
    )
    .map_err(|e| value(e.to_string()))?;
    let after: i64 = con
        .query_row("SELECT COUNT(*) FROM bodies", [], |row| row.get(0))
        .map_err(|e| value(e.to_string()))?;
    let removed = before - after;
    if removed > 0 {
        // VACUUM only the staged archive snapshot; DELETE alone leaves old BLOB bytes
        // in free SQLite pages, where they would still be recoverable from the ZIP.
        con.execute_batch("VACUUM")
            .map_err(|e| value(e.to_string()))?;
        let checkpoint: Option<i64> = con
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .optional()
            .map_err(|e| value(e.to_string()))?;
        if checkpoint.is_some_and(|c| c != 0) {
            return Err(value("archived scan snapshot could not be checkpointed"));
        }
    }
    Ok(removed)
}

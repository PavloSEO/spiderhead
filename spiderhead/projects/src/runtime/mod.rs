//! Project crawl policy, admission, bounded preparation and document publishing
//! Playbook helpers live in [`crate::playbooks`].
//!
//! Shared project JSON documents carry a project UUID, a format identifier and a monotonic
//! revision. Publishing is atomic, keeps one retained previous revision under
//! `.project-backups/`, and is serialized by a per-project OS write lock.

use spiderhead_core::filesystem::PrivateMode;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::sync::LazyLock;
use uuid::Uuid;

use crate::error::{io, ProjectsError, ProjectsResult};
use crate::workspace::{load as workspace_load, target as workspace_target};
use crate::{locks, pyjson, pytime};
use spiderhead_core::filesystem::atomic_write_bytes;

mod admission;
mod aggregate_coverage;
mod reusable_scan;
pub use admission::*;
pub use aggregate_coverage::*;
pub use reusable_scan::*;

/// Crawl-policy document format.
pub const POLICY_FORMAT: &str = "spiderhead.project-crawl-policy.v1";
/// Preparation document format.
pub const PREPARATION_FORMAT: &str = "spiderhead.project-preparation.v1";

const MAX_DOC_BYTES: u64 = 16 * 1024 * 1024;

fn operation_id_re() -> &'static Regex {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[0-9a-f]{32}\z").unwrap());
    &RE
}

fn profile_name_re() -> &'static Regex {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[a-z0-9][a-z0-9_-]{0,63}\z").unwrap());
    &RE
}

/// Built-in default crawl policy.
pub fn default_policy() -> Value {
    json!({
        "evidence_hash": {"max_bytes": 1073741824, "max_seconds": 5},
        "approval_thresholds": {"pages": 1000, "requests": 3000, "seconds": 600},
        "quick_crawl": {"pages": 50, "requests": 150, "seconds": 60},
        "crawl_profile": null,
        "crawl_overrides": {},
        "competitor_limit": 5,
    })
}

/// Read one bounded JSON object under the project root, or `None` when missing.
pub fn read_document(root: &Path, name: &str) -> ProjectsResult<Option<Value>> {
    let path = root.join(name);
    if !path.exists() && !path.is_symlink() {
        return Ok(None);
    }
    let meta = io(path.symlink_metadata())?;
    if meta.file_type().is_symlink() || !meta.is_file() || meta.len() > MAX_DOC_BYTES {
        return Err(crate::error::value(format!("unsafe or oversized {name}")));
    }
    let text = io(fs::read_to_string(&path))?;
    let data: Value = serde_json::from_str(&text).map_err(ProjectsError::Json)?;
    if !data.is_object() {
        return Err(crate::error::value(format!(
            "{name} must contain an object"
        )));
    }
    Ok(Some(data))
}

thread_local! {
    static HELD_LOCKS: RefCell<std::collections::HashMap<String, usize>> =
        RefCell::new(std::collections::HashMap::new());
}

/// RAII guard for the project write lock; reentrant within one thread.
pub struct ProjectWriteLock {
    key: String,
    file: Option<std::fs::File>,
}

impl Drop for ProjectWriteLock {
    fn drop(&mut self) {
        let _ = self.file.take().map(|f| locks::unlock(&f));
        HELD_LOCKS.with(|held| {
            let mut held = held.borrow_mut();
            match held.get(&self.key).copied() {
                Some(1) | None => {
                    held.remove(&self.key);
                }
                Some(n) => {
                    held.insert(self.key.clone(), n - 1);
                }
            }
        });
    }
}

/// Serialize project mutations with a bounded, portable OS lock.
///
/// Reentrant within one thread; other processes acquire within `timeout_seconds` or get a
/// clear busy error.
pub fn project_write_lock(root: &Path, timeout_seconds: f64) -> ProjectsResult<ProjectWriteLock> {
    let key = io(root.canonicalize())?.to_string_lossy().to_string();
    let already = HELD_LOCKS.with(|held| held.borrow().contains_key(&key));
    if already {
        HELD_LOCKS.with(|held| {
            *held.borrow_mut().entry(key.clone()).or_insert(0) += 1;
        });
        return Ok(ProjectWriteLock { key, file: None });
    }
    locks::require_locking().map_err(ProjectsError::Io)?;
    let file = io(locks::open_lock(&root.join(".project-write.lock"), true))?;
    let deadline = pytime::monotonic() + timeout_seconds;
    loop {
        match locks::lock_exclusive(&file) {
            Ok(()) => break,
            Err(e) => {
                if pytime::monotonic() >= deadline {
                    return Err(crate::error::value(
                        "project workspace is busy; another writer owns it",
                    ));
                }
                let rest = (deadline - pytime::monotonic()).clamp(0.0, 0.01);
                std::thread::sleep(std::time::Duration::from_secs_f64(rest));
                let _ = e;
            }
        }
    }
    HELD_LOCKS.with(|held| {
        held.borrow_mut().insert(key.clone(), 1);
    });
    Ok(ProjectWriteLock {
        key,
        file: Some(file),
    })
}

/// Publish one bounded document, keeping a retained previous revision.
pub fn write_document(
    root: &Path,
    name: &str,
    data: &Value,
    expected_revision: Option<i64>,
) -> ProjectsResult<()> {
    let _guard = project_write_lock(root, 5.0)?;
    write_document_locked(root, name, data, expected_revision)
}

/// Publish while the caller holds `project_write_lock`.
pub fn write_document_locked(
    root: &Path,
    name: &str,
    data: &Value,
    expected_revision: Option<i64>,
) -> ProjectsResult<()> {
    let content = pyjson::dumps_indent2(data) + "\n";
    if content.len() as u64 > MAX_DOC_BYTES {
        return Err(crate::error::value(
            "project document exceeds its byte budget",
        ));
    }
    let lock = root.join(format!(".{name}.lock"));
    let created = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .private_mode(0o600)
        .open(&lock);
    match created {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(crate::error::value(format!("another writer owns {name}")));
        }
        Err(e) => return Err(ProjectsError::Io(e)),
    }
    let result = (|| -> ProjectsResult<()> {
        let previous = read_document(root, name)?;
        let revision = previous
            .as_ref()
            .and_then(|p| p.get("revision"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if let Some(expected) = expected_revision {
            if revision != expected {
                return Err(crate::error::value("project document revision conflict"));
            }
        }
        if previous.is_some() {
            let backups = root.join(".project-backups");
            if backups.is_symlink() {
                return Err(crate::error::value("unsafe project backup directory"));
            }
            io(fs::create_dir_all(&backups))?;
            let backup = backups.join(format!("{name}.{}.json", Uuid::new_v4().simple()));
            let mut stream = io(std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .private_mode(0o600)
                .open(&backup))?;
            io(stream.write_all(&io(fs::read(root.join(name)))?))?;
            io(stream.sync_all())?;
        }
        atomic_write_bytes(&root.join(name), content.as_bytes(), None, None, true)
            .map_err(ProjectsError::Io)
    })();
    let _ = fs::remove_file(&lock);
    result
}

fn positive(value: &Value, label: &str) -> ProjectsResult<i64> {
    let v = value.as_i64().filter(|v| (1..=1_000_000_000).contains(v));
    match v {
        Some(v) if !value.is_boolean() => Ok(v),
        _ => Err(crate::error::value(format!(
            "{label} must be a positive bounded integer"
        ))),
    }
}

/// Check a policy object against the collector's settings validation.
pub fn validate_policy(policy: &Value) -> ProjectsResult<Value> {
    let default = default_policy();
    let object = policy.as_object();
    let keys: BTreeSet<&str> = object
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();
    let full: BTreeSet<&str> = default
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let legacy: BTreeSet<&str> = full
        .iter()
        .copied()
        .filter(|k| *k != "crawl_profile")
        .collect();
    let no_hash_full: BTreeSet<&str> = full
        .iter()
        .copied()
        .filter(|k| *k != "evidence_hash")
        .collect();
    let no_hash_legacy: BTreeSet<&str> = legacy
        .iter()
        .copied()
        .filter(|k| *k != "evidence_hash")
        .collect();
    if object.is_none()
        || !(keys == full || keys == legacy || keys == no_hash_full || keys == no_hash_legacy)
    {
        return Err(crate::error::value("crawl policy has unsupported fields"));
    }
    let mut result = default.clone();
    for (k, v) in object.iter().flat_map(|o| o.iter()) {
        result[k] = v.clone();
    }
    if !keys.contains("evidence_hash") {
        result["evidence_hash"] = default["evidence_hash"].clone();
    }
    let hashing = result["evidence_hash"].as_object().filter(|h| {
        h.keys().map(String::as_str).collect::<BTreeSet<_>>()
            == ["max_bytes", "max_seconds"].into_iter().collect()
    });
    let Some(hashing) = hashing else {
        return Err(crate::error::value(
            "evidence_hash requires max_bytes and max_seconds",
        ));
    };
    for (name, ceiling) in [
        ("max_bytes", 64i64 * 1024 * 1024 * 1024),
        ("max_seconds", 300),
    ] {
        match hashing[name].as_i64() {
            Some(v) if !hashing[name].is_boolean() && 1 <= v && v <= ceiling => {}
            _ => {
                return Err(crate::error::value(format!(
                    "evidence_hash.{name} must be an integer from 1 to {ceiling}"
                )))
            }
        }
    }
    for section in ["approval_thresholds", "quick_crawl"] {
        let object = result[section].as_object().filter(|s| {
            s.keys().map(String::as_str).collect::<BTreeSet<_>>()
                == ["pages", "requests", "seconds"].into_iter().collect()
        });
        let Some(object) = object else {
            return Err(crate::error::value(format!(
                "{section} requires pages, requests and seconds"
            )));
        };
        for (key, v) in object {
            positive(v, &format!("{section}.{key}"))?;
        }
    }
    if !result["crawl_overrides"].is_object() {
        return Err(crate::error::value(
            "crawl_overrides must be a configuration object",
        ));
    }
    let profile = &result["crawl_profile"];
    if !profile.is_null() {
        let ok = profile
            .as_str()
            .is_some_and(|p| !p.is_empty() && profile_name_re().is_match(p));
        if !ok {
            return Err(crate::error::value(
                if profile.as_str().is_none_or(str::is_empty) {
                    "crawl_profile must be a saved profile name or null"
                } else {
                    "crawl_profile is not a valid saved profile name"
                },
            ));
        }
    }
    // Validate with the collector's actual settings; policy never executes its text.
    let overrides: Map<String, Value> = result["crawl_overrides"]
        .as_object()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .collect();
    let input = spiderhead_core::crawl_settings::LoadInput {
        file: None,
        overrides,
        base_overrides: Map::new(),
    };
    let host = spiderhead_core::host::SystemHost;
    spiderhead_core::crawl_settings::load(&input, &host)
        .map_err(|e| crate::error::value(e.to_string()))?;
    if positive(&result["competitor_limit"], "competitor_limit")? > 20 {
        return Err(crate::error::value("competitor_limit cannot exceed 20"));
    }
    Ok(result)
}

fn read_project_policy(root: &Path, project_uuid: &str) -> ProjectsResult<Option<Value>> {
    let current = read_document(root, "crawl-policy.json")?;
    if let Some(current) = &current {
        let keys: BTreeSet<&str> = current
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        let base: BTreeSet<&str> = ["format", "project_uuid", "revision", "policy"]
            .into_iter()
            .collect();
        let mut with_op = base.clone();
        with_op.insert("operation_id");
        let op_ok = current.get("operation_id").is_none_or(|v| {
            v.is_null() || v.as_str().is_some_and(|s| operation_id_re().is_match(s))
        });
        if !(keys == base || keys == with_op)
            || current["format"].as_str() != Some(POLICY_FORMAT)
            || current["project_uuid"].as_str() != Some(project_uuid)
            || current["revision"].as_i64().is_none()
            || current["revision"].is_boolean()
            || !op_ok
        {
            return Err(crate::error::value(
                "unsupported or mismatched project crawl policy",
            ));
        }
    }
    Ok(current.map(|mut c| {
        if c.get("operation_id").is_none() {
            c["operation_id"] = Value::Null;
        }
        c
    }))
}

/// Read the effective policy, or apply a validated replacement.
pub fn project_policy(
    directory: &Path,
    policy: Option<&Value>,
    apply: bool,
    expected_revision: Option<i64>,
    operation_id: Option<&str>,
) -> ProjectsResult<Value> {
    if let Some(op) = operation_id {
        if !operation_id_re().is_match(op) {
            return Err(crate::error::value(
                "operation_id must be a UUID hex value or null",
            ));
        }
    }
    let (root, project) = workspace_load(directory)?;
    let project_uuid = project["project_uuid"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    if !apply {
        let current = read_project_policy(&root, &project_uuid)?;
        let fallback = default_policy();
        let selected = validate_policy(match policy {
            Some(p) => p,
            None => match &current {
                Some(c) => &c["policy"],
                None => &fallback,
            },
        })?;
        return Ok(json!({
            "ok": true,
            "applied": false,
            "revision": current.as_ref().map_or(0, |c| c["revision"].as_i64().unwrap_or(0)),
            "policy": selected,
            "operation_id": current.as_ref().map_or(Value::Null, |c| c["operation_id"].clone()),
        }));
    }
    let Some(expected) = expected_revision else {
        return Err(crate::error::value(
            "expected_revision is required to change project policy",
        ));
    };
    let _guard = project_write_lock(&root, 5.0)?;
    let (current_root, current_project) = workspace_load(directory)?;
    if current_root != root
        || current_project["project_uuid"].as_str() != Some(project_uuid.as_str())
    {
        return Err(crate::error::value("project changed before policy update"));
    }
    let current = read_project_policy(&root, &project_uuid)?;
    let fallback = default_policy();
    let selected = validate_policy(match policy {
        Some(p) => p,
        None => match &current {
            Some(c) => &c["policy"],
            None => &fallback,
        },
    })?;
    let revision = current
        .as_ref()
        .map_or(0, |c| c["revision"].as_i64().unwrap_or(0));
    let document = json!({
        "format": POLICY_FORMAT,
        "project_uuid": project_uuid,
        "revision": revision + 1,
        "policy": selected,
        "operation_id": operation_id,
    });
    write_document(&root, "crawl-policy.json", &document, Some(expected))?;
    Ok(json!({
        "ok": true,
        "applied": true,
        "revision": revision + 1,
        "policy": selected,
        "operation_id": operation_id,
    }))
}

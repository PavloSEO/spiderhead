//! Explicit bounded one-shot scans for a task's structured competitor set
//!
//! A run is a durable manifest under `reports/.competitor-scan-runs/` plus a project-wide
//! `.competitor-scan.lock` holding the active runner's identity. Sites crawl through the
//! caller-injected `crawl_site` handler so no network work hides inside the project layer.

use spiderhead_core::filesystem::PrivateMode;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::sync::{LazyLock, Mutex};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::bindings::py_str;
use crate::error::{py_exception_name, py_named, value, ProjectsError, ProjectsResult};
use crate::locks;
use crate::pyjson;
use crate::pytime::{monotonic, zulu};
use crate::schedule::falsy;
use crate::urls::urlsplit;

mod crawl_plan;
mod new_manifest;
mod truncate_1024;
pub use crawl_plan::*;
pub use new_manifest::*;
pub use truncate_1024::*;

const MAX_COMPETITORS: usize = 20;
const MAX_CONCURRENCY: i64 = 3;
const MAX_SITE_URLS: i64 = 10_000;
const MAX_TOTAL_URLS: i64 = 50_000;
const MAX_SITE_SECONDS: i64 = 3600;
const MAX_TOTAL_SECONDS: i64 = 8 * 3600;
const MAX_SITE_REQUESTS: i64 = 30_000;
const MAX_TOTAL_REQUESTS: i64 = 60_000;
const MAX_RUN_MANIFESTS: usize = 100;
const MAX_MANIFEST_BYTES: u64 = 2 * 1024 * 1024;

/// Run manifest format.
pub const RUN_FORMAT: &str = "spiderhead.competitor-scan-run.v1";
/// Directory of durable run manifests under the project root.
pub const RUN_DIRECTORY: &str = "reports/.competitor-scan-runs";
/// Durable run lock name under the project root.
pub const RUN_LOCK: &str = ".competitor-scan.lock";

const PLAN_FIELDS: [&str; 12] = [
    "url",
    "project_uuid",
    "directory",
    "max_urls",
    "max_requests",
    "max_crawl_seconds",
    "mode",
    "user_agent",
    "min_delay_seconds",
    "sitemap_url",
    "urls",
    "schedule_revision",
];
const SITE_STATES: [&str; 7] = [
    "pending",
    "running",
    "interrupted",
    "completed",
    "partial",
    "failed",
    "cancelled",
];

/// `CompetitorScanError`: domain errors of the competitor-run surface.
fn competitor_error(message: &str) -> ProjectsError {
    py_named("CompetitorScanError", message)
}

fn key_error(key: &str) -> ProjectsError {
    py_named("KeyError", format!("'{key}'"))
}

/// Type name of one JSON value, spelled as recorded in error messages.
fn py_typename(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// `obj[key]` with `KeyError` parity.
fn obj_get<'a>(obj: &'a Value, key: &str) -> ProjectsResult<&'a Value> {
    if !obj.is_object() {
        return Err(py_named(
            "TypeError",
            format!("'{}' object is not subscriptable", py_typename(obj)),
        ));
    }
    obj.get(key).ok_or_else(|| key_error(key))
}

fn is_hex(text: &str, len: usize) -> bool {
    text.len() == len
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_int(v: &Value) -> bool {
    v.is_i64() || v.is_u64()
}

/// `_run_directory`: the manifests directory under `reports/`.
fn run_directory(root: &Path, create: bool) -> ProjectsResult<PathBuf> {
    let reports = root.join("reports");
    if reports.is_symlink() || !reports.is_dir() {
        return Err(competitor_error(
            "project reports directory is unavailable or unsafe",
        ));
    }
    let directory = root.join(RUN_DIRECTORY);
    if directory.is_symlink() {
        return Err(competitor_error("competitor scan run directory is unsafe"));
    }
    if create {
        std::fs::DirBuilder::new()
            .recursive(true)
            .private_mode(0o700)
            .create(&directory)
            .map_err(ProjectsError::Io)?;
    }
    if directory.exists() && !directory.is_dir() {
        return Err(competitor_error(
            "competitor scan run path is not a directory",
        ));
    }
    Ok(directory)
}

/// `_manifest_path`: validated `<run_id>.json` under the run directory.
fn manifest_path(root: &Path, run_id: &str, create: bool) -> ProjectsResult<PathBuf> {
    let normalized = uuid::Uuid::parse_str(run_id)
        .map(|u| u.to_string())
        .map_err(|_| competitor_error("run_id must be a UUID"))?;
    Ok(run_directory(root, create)?.join(format!("{normalized}.json")))
}

/// `_read_manifest`: parse and fully validate one durable run record.
pub fn read_manifest(root: &Path, run_id: &str, project_uuid: &str) -> ProjectsResult<Value> {
    let path = manifest_path(root, run_id, false)?;
    if path.is_symlink()
        || !path.is_file()
        || path.metadata().map(|m| m.len()).unwrap_or(u64::MAX) > MAX_MANIFEST_BYTES
    {
        return Err(competitor_error("competitor scan run is unavailable"));
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|_| competitor_error("competitor scan run is invalid"))?;
    let document: Value = serde_json::from_str(&text)
        .map_err(|_| competitor_error("competitor scan run is invalid"))?;
    let normalized = uuid::Uuid::parse_str(run_id)
        .map(|u| u.to_string())
        .map_err(|_| competitor_error("run_id must be a UUID"))?;
    let budgets_ok = document.get("budgets").is_some_and(|b| {
        b.is_object()
            && b.as_object().is_some_and(|m| {
                m.len() == 6
                    && [
                        "urls",
                        "requests",
                        "seconds",
                        "max_urls",
                        "max_requests",
                        "max_seconds",
                    ]
                    .iter()
                    .all(|k| m.contains_key(*k))
            })
    });
    let runner_ok = match document.get("runner_token") {
        None | Some(Value::Null) => true,
        Some(v) => v.as_str().is_some_and(|t| is_hex(t, 32)),
    };
    if !document.is_object()
        || document["format"].as_str() != Some(RUN_FORMAT)
        || document["run_id"].as_str() != Some(normalized.as_str())
        || document["project_uuid"].as_str() != Some(project_uuid)
        || !document["sites"].is_array()
        || document["sites"]
            .as_array()
            .is_some_and(|s| s.len() > MAX_COMPETITORS)
        || !document["task_id"].is_string()
        || !is_int(&document["coverage_revision"])
        || document["coverage_revision"]
            .as_i64()
            .is_some_and(|r| r < 0)
        || !document["fingerprint"]
            .as_str()
            .is_some_and(|f| is_hex(f, 64))
        || !matches!(
            document["state"].as_str(),
            Some("running" | "interrupted" | "completed" | "partial" | "failed" | "cancelled")
        )
        || !runner_ok
        || !is_int(&document["concurrency"])
        || !document["concurrency"]
            .as_i64()
            .is_some_and(|c| (1..=MAX_CONCURRENCY).contains(&c))
        || !document["approve_large_crawl"].is_boolean()
        || !budgets_ok
        || document["budgets"].as_object().is_some_and(|b| {
            b.values()
                .any(|v| !is_int(v) || v.as_i64().is_some_and(|n| n < 0))
        })
        || document["budgets"]["max_urls"].as_i64() != Some(MAX_TOTAL_URLS)
        || document["budgets"]["max_requests"].as_i64() != Some(MAX_TOTAL_REQUESTS)
        || document["budgets"]["max_seconds"].as_i64() != Some(MAX_TOTAL_SECONDS)
    {
        return Err(competitor_error(
            "competitor scan run has an unsupported shape",
        ));
    }
    let required: Vec<&str> = PLAN_FIELDS
        .iter()
        .filter(|f| **f != "urls")
        .chain(std::iter::once(&"input_hash"))
        .copied()
        .collect();
    let mut totals: HashMap<&str, i64> = [("urls", 0), ("requests", 0), ("seconds", 0)].into();
    for site in document["sites"].as_array().cloned().unwrap_or_default() {
        let missing = site
            .as_object()
            .map(|m| required.iter().any(|k| !m.contains_key(*k)))
            .unwrap_or(true);
        if missing {
            return Err(competitor_error(
                "competitor scan run has an invalid site plan",
            ));
        }
        let bad = !site["url"].is_string()
            || !site["project_uuid"].is_string()
            || !site["directory"].is_string()
            || site["directory"].as_str().is_some_and(|d| {
                Path::new(d).is_absolute()
                    || Path::new(d)
                        .components()
                        .any(|c| matches!(c, Component::ParentDir))
                    || !d.starts_with("competitors/")
            })
            || !is_int(&site["max_urls"])
            || !site["max_urls"]
                .as_i64()
                .is_some_and(|n| (1..=MAX_SITE_URLS).contains(&n))
            || !is_int(&site["max_requests"])
            || !site["max_requests"]
                .as_i64()
                .is_some_and(|n| (1..=MAX_SITE_REQUESTS).contains(&n))
            || !is_int(&site["max_crawl_seconds"])
            || !site["max_crawl_seconds"]
                .as_i64()
                .is_some_and(|n| (1..=MAX_SITE_SECONDS).contains(&n))
            || !matches!(site["mode"].as_str(), Some("spider" | "sitemap" | "list"))
            || !site["user_agent"].is_string()
            || !site["input_hash"].as_str().is_some_and(|h| is_hex(h, 64))
            || !is_int(&site["schedule_revision"])
            || site["schedule_revision"].as_i64().is_some_and(|n| n < 0)
            || !site
                .get("state")
                .and_then(|s| s.as_str())
                .is_some_and(|s| SITE_STATES.contains(&s))
            || !site.get("artifact").is_some_and(|a| a.is_string())
            || site["artifact"].as_str().is_some_and(|a| {
                Path::new(a).is_absolute()
                    || Path::new(a)
                        .components()
                        .any(|c| matches!(c, Component::ParentDir))
                    || !a.starts_with("scans/")
            });
        if bad {
            return Err(competitor_error(
                "competitor scan run has an invalid site record",
            ));
        }
        *totals.get_mut("urls").unwrap_or(&mut 0) += site["max_urls"].as_i64().unwrap_or(0);
        *totals.get_mut("requests").unwrap_or(&mut 0) += site["max_requests"].as_i64().unwrap_or(0);
        *totals.get_mut("seconds").unwrap_or(&mut 0) +=
            site["max_crawl_seconds"].as_i64().unwrap_or(0);
    }
    let budgets = &document["budgets"];
    let totals_mismatch = ["urls", "requests", "seconds"]
        .iter()
        .any(|k| budgets[*k].as_i64() != totals.get(*k).copied());
    let totals_over = ["urls", "requests", "seconds"].iter().any(|k| {
        totals.get(*k).copied().unwrap_or(0)
            > budgets[format!("max_{k}").as_str()].as_i64().unwrap_or(0)
    });
    if totals_mismatch || totals_over {
        return Err(competitor_error(
            "competitor scan run budgets disagree with its per-site plans",
        ));
    }
    Ok(document)
}

/// `_write_manifest`: atomically publish the manifest under the project write lock.
pub fn write_manifest(root: &Path, manifest: &Value) -> ProjectsResult<()> {
    let path = manifest_path(root, manifest["run_id"].as_str().unwrap_or_default(), true)?;
    let payload = pyjson::dumps_indent2(manifest) + "\n";
    if payload.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(competitor_error(
            "competitor scan run exceeds its byte limit",
        ));
    }
    let _guard = crate::runtime::project_write_lock(root, 5.0)?;
    spiderhead_core::filesystem::atomic_write_bytes(
        &path,
        payload.as_bytes(),
        None,
        Some(0o600),
        true,
    )
    .map_err(ProjectsError::Io)
}

/// `_publish_start`: clear any stale cancel marker, then publish the running manifest.
fn publish_start(root: &Path, manifest: &Value, cancel_path: &Path) -> ProjectsResult<()> {
    let _guard = crate::runtime::project_write_lock(root, 5.0)?;
    if cancel_path.is_symlink() {
        return Err(competitor_error(
            "competitor scan cancellation marker is unsafe",
        ));
    }
    match std::fs::remove_file(cancel_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(ProjectsError::Io(e)),
    }
    write_manifest(root, manifest)
}

/// `_publish_final`: publish the final manifest, then clear the cancel marker.
fn publish_final(root: &Path, manifest: &Value, cancel_path: &Path) -> ProjectsResult<()> {
    let _guard = crate::runtime::project_write_lock(root, 5.0)?;
    write_manifest(root, manifest)?;
    if cancel_path.is_symlink() {
        return Err(competitor_error(
            "competitor scan cancellation marker is unsafe",
        ));
    }
    match std::fs::remove_file(cancel_path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(ProjectsError::Io(e)),
    }
}

/// `_cancel_path`: the `.cancel` marker next to the manifest.
pub fn cancel_path(root: &Path, run_id: &str, create: bool) -> ProjectsResult<PathBuf> {
    Ok(manifest_path(root, run_id, create)?.with_extension("cancel"))
}

/// `_cancelled`: marker file or caller callback requests cancellation.
fn cancelled(
    root: &Path,
    run_id: &str,
    callback: Option<&(dyn Fn() -> bool + Sync)>,
) -> ProjectsResult<bool> {
    let marker = cancel_path(root, run_id, false)?;
    if marker.is_symlink() {
        return Err(competitor_error(
            "competitor scan cancellation marker is unsafe",
        ));
    }
    if marker.is_file() {
        return Ok(true);
    }
    Ok(callback.is_some_and(|cb| cb()))
}

/// `_scan_reference`: validate a crawl result into project-relative + absolute paths.
fn scan_reference(root: &Path, target: &Path, result: &Value) -> ProjectsResult<(String, PathBuf)> {
    let scan = if result.is_object() {
        result.get("path").cloned().unwrap_or(Value::Null)
    } else {
        result.clone()
    };
    let scan = scan.as_str().unwrap_or_default();
    if scan.is_empty() {
        return Err(value("crawl returned no retained scan path"));
    }
    let source_path = PathBuf::from(scan);
    if source_path.is_symlink() {
        return Err(value("crawl result is not a regular retained scan"));
    }
    let scan_path = resolve_lenient(&source_path);
    let scans_root = resolve_lenient(&target.join("scans"));
    let root_resolved = root
        .canonicalize()
        .map_err(|_| value("crawl result is not a retained child-project scan"))?;
    let relative = scan_path
        .strip_prefix(&root_resolved)
        .map_err(|_| value("crawl result is not a retained child-project scan"))?;
    if scan_path.strip_prefix(&scans_root).is_err() {
        return Err(value("crawl result is not a retained child-project scan"));
    }
    if scan_path.is_symlink() || !scan_path.is_file() {
        return Err(value("crawl result is not a regular retained scan"));
    }
    Ok((relative.to_string_lossy().replace('\\', "/"), scan_path))
}

/// `_targets`: resolve the task's requested competitors against registered workspaces.
fn targets(root: &Path, project: &Value, task: &Value) -> ProjectsResult<Vec<Value>> {
    let prep = crate::runtime::read_document(root, "preparation.json")?.ok_or_else(|| {
        value("register competitor workspaces before scanning a task's competitors")
    })?;
    let candidates = prep.get("competitors");
    if !candidates.is_some_and(|c| c.is_array()) {
        return Err(value("project competitor list is invalid"));
    }
    let definition = obj_get(task, "definition")?;
    if !definition.is_object() {
        return Err(py_named(
            "AttributeError",
            format!(
                "'{}' object has no attribute 'get'",
                py_typename(definition)
            ),
        ));
    }
    let requested = definition
        .get("competitors")
        .filter(|c| !falsy(c))
        .cloned()
        .unwrap_or_else(|| json!([]));
    if !requested.is_array()
        || requested
            .as_array()
            .is_some_and(|r| r.is_empty() || r.len() > MAX_COMPETITORS)
    {
        return Err(value("the task has no bounded competitor list"));
    }
    let root_resolved = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut found: HashMap<String, Value> = HashMap::new();
    for row in candidates.and_then(|c| c.as_array()).into_iter().flatten() {
        let probe = (|| -> ProjectsResult<Option<(String, Value)>> {
            if !row.is_object() {
                return Ok(None);
            }
            let identity =
                crate::runtime::competitor_identity(row["url"].as_str().unwrap_or_default())?;
            let relative = row["directory"].as_str().unwrap_or_default();
            let rel_path = Path::new(relative);
            if rel_path.is_absolute()
                || rel_path
                    .components()
                    .any(|c| matches!(c, Component::ParentDir))
                || !relative.starts_with("competitors/")
            {
                return Ok(None);
            }
            let child = root.join(rel_path);
            if child.is_symlink() {
                return Ok(None);
            }
            let child_resolved = child.canonicalize().map_err(ProjectsError::Io)?;
            if !child_resolved.starts_with(&root_resolved) {
                return Ok(None);
            }
            let (child_root, child_project) = crate::workspace::load(&child)?;
            if child_root != child_resolved
                || child_project["project_uuid"] != row["project_uuid"]
                || child_project["site"]["target"].as_str().unwrap_or_default() != identity
            {
                return Ok(None);
            }
            let mut entry = row.clone();
            entry["root"] = json!(child_root.to_string_lossy());
            entry["project"] = child_project;
            Ok(Some((identity, entry)))
        })();
        let entry = match probe {
            Ok(Some(pair)) => pair,
            _ => continue,
        };
        if found.contains_key(&entry.0) {
            return Err(value("project contains duplicate competitor workspaces"));
        }
        found.insert(entry.0, entry.1);
    }
    let mut output = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut requested_urls = 0i64;
    for item in requested.as_array().cloned().unwrap_or_default() {
        let keys_ok = item
            .as_object()
            .is_some_and(|m| m.len() == 2 && m.contains_key("url") && m.contains_key("max_urls"));
        if !keys_ok {
            return Err(value("each task competitor requires url and max_urls"));
        }
        if !is_int(&item["max_urls"])
            || !item["max_urls"]
                .as_i64()
                .is_some_and(|n| (1..=MAX_SITE_URLS).contains(&n))
        {
            return Err(value(format!(
                "each task competitor max_urls must be 1..{MAX_SITE_URLS}"
            )));
        }
        requested_urls += item["max_urls"].as_i64().unwrap_or(0);
        if requested_urls > MAX_TOTAL_URLS {
            return Err(value(format!(
                "aggregate competitor URL cap is {MAX_TOTAL_URLS}"
            )));
        }
        let identity =
            crate::runtime::competitor_identity(item["url"].as_str().unwrap_or_default())?;
        if !seen.insert(identity.clone()) {
            return Err(value("task contains duplicate competitor sites"));
        }
        let Some(candidate) = found.get(&identity) else {
            return Err(value(format!(
                "competitor workspace is unavailable for {}",
                py_str(&item["url"])
            )));
        };
        if identity
            == crate::runtime::competitor_identity(
                project["site"]["target"].as_str().unwrap_or_default(),
            )?
        {
            return Err(value("primary site cannot be scanned as a competitor"));
        }
        let mut entry = candidate.clone();
        entry["max_urls"] = item["max_urls"].clone();
        output.push(entry);
    }
    Ok(output)
}

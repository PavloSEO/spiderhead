//! Durable, registered workflow checkpoints for interruption-safe local work

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::coverage::coverage_status;
use crate::error::{value, ProjectsError, ProjectsResult};
use crate::evidence::{artifact_path, digest};
use crate::io::{check_owner, load_document, save_checked};
use crate::pytime;
use crate::schedule::falsy;

mod resume;
mod review;
pub use resume::*;
pub use review::*;

/// `execution.json` document format token.
pub const FORMAT: &str = "spiderhead.workflow-execution.v1";
/// `execution.json` document name.
pub const NAME: &str = "execution.json";
const EVIDENCE_HASH_SECONDS: f64 = 5.0;
const EVIDENCE_HASH_BYTES: u64 = 1024 * 1024 * 1024;
/// Default workflow prompt reference.
pub const DEFAULT_PROMPT: &str = "skill:workflow/full-audit-v1";

const STEP_STATES: [&str; 5] = [
    "succeeded",
    "failed",
    "unavailable",
    "skipped",
    "interrupted",
];

/// Digest cache keyed by file identity, shared across one recovery pass.
pub type Digests = HashMap<(String, u64, u64, u64, i64, i64), String>;

fn text(item: &Value, name: &str, maximum: usize) -> ProjectsResult<String> {
    let Some(raw) = item.as_str() else {
        return Err(value(format!(
            "{name} must be nonempty text of at most {maximum} characters"
        )));
    };
    if raw.trim().is_empty() || raw.chars().count() > maximum {
        return Err(value(format!(
            "{name} must be nonempty text of at most {maximum} characters"
        )));
    }
    Ok(raw.trim().to_string())
}

fn text_bound(item: &Value, name: &str, maximum: usize) -> ProjectsResult<String> {
    text(item, name, maximum)
}

fn key_error(key: &str) -> ProjectsError {
    value(format!("'{key}'"))
}

fn get<'a>(item: &'a Value, key: &str) -> ProjectsResult<&'a Value> {
    item.get(key).ok_or_else(|| key_error(key))
}

fn get_str<'a>(item: &'a Value, key: &str) -> ProjectsResult<&'a str> {
    item.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| key_error(key))
}

fn default_document(project_uuid: &str) -> Value {
    json!({"format": FORMAT, "revision": 0, "project_uuid": project_uuid, "runs": []})
}

fn load(directory: &Path) -> ProjectsResult<(PathBuf, Value, Value)> {
    let (root, project, document) = load_document(directory, NAME, &default_document)?;
    check_owner(
        &document,
        project["project_uuid"].as_str().unwrap_or_default(),
        FORMAT,
        "execution checkpoint",
    )?;
    let runs = document.get("runs");
    if !runs.is_some_and(Value::is_array)
        || runs.and_then(Value::as_array).map_or(0, Vec::len) > 10_000
    {
        return Err(value("execution checkpoint has invalid run history"));
    }
    Ok((root, project, document))
}

fn save(root: &Path, document: &mut Value, expected_revision: i64) -> ProjectsResult<()> {
    save_checked(root, NAME, document, expected_revision, "execution")
}

fn catalogue_hash(item_id: &str) -> ProjectsResult<Option<Value>> {
    Ok(crate::catalogue::catalogue()?
        .get(item_id)
        .and_then(|entry| entry.get("definition_hash").cloned()))
}

fn accepted_goal(directory: &Path, item: &Value) -> ProjectsResult<Value> {
    let goal_id = text_bound(item, "goal_id", 256)?;
    let mut offset = 0i64;
    loop {
        let page = crate::inbox::list_entries(directory, "workflow-handoff", offset, 100, true)?;
        for entry in page["entries"].as_array().cloned().unwrap_or_default() {
            if entry.get("id").and_then(Value::as_str) != Some(goal_id.as_str()) {
                continue;
            }
            if entry.get("kind").and_then(Value::as_str) != Some("proposed_goal")
                || entry.get("goal_state").and_then(Value::as_str) != Some("accepted")
            {
                return Err(value(format!(
                    "workflow goal_id must identify an accepted proposed goal; accept it with `seohead project inbox-goal --directory DIR --entry-id {goal_id} --state accepted`"
                )));
            }
            return Ok(json!({
                "id": entry.get("id").cloned().unwrap_or(Value::Null),
                "references": entry.get("references").cloned().ok_or_else(|| key_error("references"))?,
            }));
        }
        match page["pagination"]["next_offset"].as_i64() {
            Some(next) => offset = next,
            None => break,
        }
    }
    Err(value(
        "workflow goal_id is not present in this project inbox; list entries with `seohead project inbox-list --directory DIR --consumer ID`",
    ))
}

fn prompt_reference(item: &Value) -> ProjectsResult<Value> {
    let prompt_id = text_bound(item, "prompt_reference", 512)?;
    let entry = crate::catalogue::catalogue()?.get(&prompt_id).cloned();
    match entry {
        Some(entry) if entry.get("kind").and_then(Value::as_str) == Some("skill") => Ok(json!({
            "id": prompt_id,
            "definition_hash": entry.get("definition_hash").cloned().ok_or_else(|| key_error("definition_hash"))?,
        })),
        _ => Err(value("prompt_reference must identify a registered skill")),
    }
}

fn task_ids(item: &Value, rows: &BTreeMap<String, Value>) -> ProjectsResult<Vec<String>> {
    let Some(list) = item.as_array() else {
        return Err(value(
            "workflow task_ids must be unique current custom checklist tasks",
        ));
    };
    let unique: std::collections::HashSet<String> =
        list.iter().map(crate::pyjson::dumps_compact).collect();
    if list.is_empty()
        || list.len() > 20
        || unique.len() != list.len()
        || list
            .iter()
            .any(|entry| entry.as_str().is_none_or(|id| !id.starts_with("custom:")))
    {
        return Err(value(
            "workflow task_ids must be unique current custom checklist tasks",
        ));
    }
    let ids: Vec<String> = list
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    if ids.iter().any(|id| {
        rows.get(id).is_none_or(|row| {
            !falsy(row.get("stale").unwrap_or(&Value::Null))
                || !falsy(row.get("complete").unwrap_or(&Value::Null))
        })
    }) {
        return Err(value(
            "workflow task_ids must be current incomplete custom checklist tasks",
        ));
    }
    Ok(ids)
}

fn resolve_context(
    item: &Value,
    directory: &Path,
    project: &Value,
    rows: &BTreeMap<String, Value>,
) -> ProjectsResult<Value> {
    let empty = Value::Object(Map::new());
    let item = if item.is_null() { &empty } else { item };
    let Some(object) = item.as_object() else {
        return Err(value(
            "workflow context has unsupported fields; allowed: goal_id, prompt_reference, task_ids, competitors, phase",
        ));
    };
    const ALLOWED: [&str; 5] = [
        "goal_id",
        "prompt_reference",
        "task_ids",
        "competitors",
        "phase",
    ];
    if object.keys().any(|key| !ALLOWED.contains(&key.as_str())) {
        return Err(value(
            "workflow context has unsupported fields; allowed: goal_id, prompt_reference, task_ids, competitors, phase",
        ));
    }
    let goal_id = object.get("goal_id").ok_or_else(|| {
        value(
            "workflow context requires goal_id (an accepted owner goal); accept one with `seohead project inbox-goal --directory DIR --entry-id ENTRY --state accepted`",
        )
    })?;
    let competitors = object
        .get("competitors")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let competitor_list = competitors.as_array();
    let unique: BTreeSet<String> = competitor_list
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if competitor_list.is_none_or(|list| {
        list.len() > 20
            || unique.len() != list.len()
            || list.iter().any(|url| {
                url.as_str()
                    .is_none_or(|u| !u.starts_with("http://") && !u.starts_with("https://"))
            })
    }) {
        return Err(value(
            "workflow competitors must be a unique bounded HTTP(S) list",
        ));
    }
    let goal = accepted_goal(directory, goal_id)?;
    let task_ids = if let Some(explicit) = object.get("task_ids") {
        task_ids(explicit, rows)?
    } else {
        let task_id = crate::inbox::goal_task_id(goal["id"].as_str().unwrap_or_default());
        match rows.get(&task_id) {
            Some(row)
                if falsy(row.get("stale").unwrap_or(&Value::Null))
                    && falsy(row.get("complete").unwrap_or(&Value::Null)) => {}
            _ => {
                return Err(value(format!(
                    "accepted goal's checklist task {task_id} is missing, stale or complete; pass task_ids explicitly with current custom tasks"
                )))
            }
        }
        vec![task_id]
    };
    let mut result = json!({
        "own_site": get(get(project, "site")?, "target")?,
        "competitors": competitors,
        "goal": goal,
        "prompt": prompt_reference(object.get("prompt_reference").unwrap_or(&json!(DEFAULT_PROMPT)))?,
        "task_ids": task_ids,
    });
    if let Some(phase) = object.get("phase") {
        result["phase"] = json!(text_bound(phase, "phase", 128)?);
    }
    Ok(result)
}

fn hash_limits(root: &Path) -> ProjectsResult<(f64, u64)> {
    let policy = crate::runtime::project_policy(root, None, false, None, None)?;
    let limits = &policy["policy"]["evidence_hash"];
    Ok((
        limits["max_seconds"]
            .as_f64()
            .unwrap_or(EVIDENCE_HASH_SECONDS),
        limits["max_bytes"].as_u64().unwrap_or(EVIDENCE_HASH_BYTES),
    ))
}

type Identity = (String, u64, u64, u64, i64, i64);

fn artifact_identity(root: &Path, reference: &str) -> ProjectsResult<(PathBuf, Identity)> {
    let path = artifact_path(root, &Value::String(reference.to_string()))?;
    let info = std::fs::metadata(&path).map_err(ProjectsError::Io)?;
    let identity = {
        use spiderhead_core::filesystem::{path_identity, StatCompat};
        let id = path_identity(&path).map_err(ProjectsError::Io)?;
        (
            id.0,
            id.1,
            info.size(),
            info.mtime_nsec(),
            info.ctime_nsec(),
        )
    };
    Ok((
        path.clone(),
        (
            path.to_string_lossy().into_owned(),
            identity.0,
            identity.1,
            identity.2,
            identity.3,
            identity.4,
        ),
    ))
}

fn verification_receipt(
    root: &Path,
    records: &[Value],
    digests: &Digests,
) -> ProjectsResult<Value> {
    let mut artifacts = Vec::new();
    for record in records {
        let reference = get_str(record, "reference")?;
        let (path, identity) = artifact_identity(root, reference)?;
        if digests.get(&identity).map(String::as_str) != get(record, "sha256")?.as_str() {
            return Err(value(
                "evidence artifact changed before recording its verification receipt",
            ));
        }
        let _ = path;
        let mut artifact = record.as_object().cloned().unwrap_or_default();
        let (_path_key, dev, ino, size, mtime, ctime) = &identity;
        artifact.insert(
            "identity".into(),
            Value::Array(vec![
                json!(dev),
                json!(ino),
                json!(size),
                json!(mtime),
                json!(ctime),
            ]),
        );
        artifacts.push(Value::Object(artifact));
    }
    Ok(json!({
        "verified_at": pytime::iso_now(),
        "artifacts": artifacts,
    }))
}

fn verify_receipt_metadata(root: &Path, step: &Value) -> ProjectsResult<()> {
    let records = evidence(
        step.get("evidence").unwrap_or(&Value::Null),
        None,
        &mut HashMap::new(),
        None,
        None,
    )?;
    let receipt = step.get("evidence_verification").unwrap_or(&Value::Null);
    let receipt_object = receipt.as_object();
    if records.is_empty()
        || receipt_object.is_none()
        || !receipt["verified_at"].is_string()
        || !receipt["artifacts"].is_array()
    {
        return Err(value("unverified: no retained byte-verification receipt"));
    }
    let stamp_text = receipt["verified_at"].as_str().unwrap_or_default();
    let stamp = pytime::fromisoformat(&stamp_text.replace('Z', "+00:00"))
        .map_err(|_| value("unverified: verification receipt timestamp is invalid"))?;
    if !stamp.is_aware() {
        return Err(value(
            "unverified: verification receipt timestamp has no timezone",
        ));
    }
    let mut saved: BTreeMap<String, Value> = BTreeMap::new();
    for item in receipt["artifacts"].as_array().cloned().unwrap_or_default() {
        let valid = item.as_object().is_some_and(|object| {
            object.len() == 3
                && object.contains_key("reference")
                && object.contains_key("sha256")
                && object.contains_key("identity")
        }) && item["reference"].is_string()
            && item["identity"].as_array().is_some_and(|identity| {
                identity.len() == 5
                    && identity
                        .iter()
                        .all(|part| part.as_i64().is_some() && !part.is_boolean())
            });
        if !valid {
            return Err(value(
                "unverified: verification receipt identity is invalid",
            ));
        }
        saved.insert(
            item["reference"].as_str().unwrap_or_default().to_string(),
            item,
        );
    }
    if saved.len() != records.len() {
        return Err(value(
            "unverified: verification receipt does not cover the evidence",
        ));
    }
    for record in &records {
        let reference = record["reference"].as_str().unwrap_or_default();
        let prior = saved.get(reference).cloned().unwrap_or(Value::Null);
        let (_, identity) = artifact_identity(root, reference)?;
        let (_k, dev, ino, size, mtime, ctime) = &identity;
        let expected: Vec<Value> = vec![
            json!(dev),
            json!(ino),
            json!(size),
            json!(mtime),
            json!(ctime),
        ];
        if prior.get("sha256") != record.get("sha256")
            || prior
                .get("identity")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                != expected
        {
            return Err(value(
                "evidence artifact metadata changed; explicit byte verification required",
            ));
        }
    }
    Ok(())
}

/// Bounded evidence-record validation.
pub fn evidence(
    records: &Value,
    root: Option<&Path>,
    digests: &mut Digests,
    deadline: Option<f64>,
    max_bytes: Option<u64>,
) -> ProjectsResult<Vec<Value>> {
    if falsy(records) {
        return Ok(Vec::new());
    }
    let Some(list) = records.as_array() else {
        return Err(value("evidence must be a list of at most 20 records"));
    };
    if list.len() > 20 {
        return Err(value("evidence must be a list of at most 20 records"));
    }
    let limits = match root {
        Some(root) if deadline.is_none() || max_bytes.is_none() => Some(hash_limits(root)?),
        _ => None,
    };
    let max_seconds = limits.map(|(s, _)| s).unwrap_or(EVIDENCE_HASH_SECONDS);
    let max_bytes = max_bytes
        .or_else(|| limits.map(|(_, b)| b))
        .unwrap_or(EVIDENCE_HASH_BYTES);
    let deadline = deadline.unwrap_or_else(|| pytime::monotonic() + max_seconds);
    let mut result = Vec::new();
    for record in list {
        let keys: BTreeSet<&str> = record
            .as_object()
            .map(|object| object.keys().map(String::as_str).collect())
            .unwrap_or_default();
        if record.as_object().is_none_or(|o| o.len() != 2)
            || !keys.contains("reference")
            || !keys.contains("sha256")
        {
            return Err(value("evidence records require reference and sha256"));
        }
        let reference = text_bound(&record["reference"], "evidence reference", 512)?;
        let digest_value = &record["sha256"];
        let valid = digest_value.as_str().is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        });
        if !valid {
            return Err(value(
                "evidence sha256 must be a lowercase 64-character digest",
            ));
        }
        if let Some(root) = root {
            let (path, identity) = artifact_identity(root, &reference)?;
            if !digests.contains_key(&identity) {
                let measured = digest(&path, Some(deadline), Some(max_bytes))?;
                let (_, after) = artifact_identity(root, &reference)?;
                if identity.1 != after.1
                    || identity.2 != after.2
                    || identity.3 != after.3
                    || identity.4 != after.4
                    || identity.5 != after.5
                {
                    return Err(value("evidence artifact changed while hashing"));
                }
                digests.insert(identity.clone(), measured);
            }
            if digests.get(&identity).map(String::as_str) != digest_value.as_str() {
                return Err(value(
                    "evidence artifact sha256 does not match retained bytes",
                ));
            }
        }
        result.push(json!({"reference": reference, "sha256": digest_value}));
    }
    let unique: BTreeSet<&str> = result
        .iter()
        .filter_map(|record| record["reference"].as_str())
        .collect();
    if unique.len() != result.len() {
        return Err(value(
            "evidence references must be unique within a checkpoint",
        ));
    }
    Ok(result)
}

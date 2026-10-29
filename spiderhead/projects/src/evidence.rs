//! Validate saved completion evidence without running checks or changing artifacts

use std::collections::{BTreeSet, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::coverage::schema::{text, URL_ENUMERATION_LIMIT};
use crate::error::{io, ProjectsError, ProjectsResult};
use crate::pytime;

/// `artifact_path`: accept portable regular files inside the project, excluding control files.
pub fn artifact_path(root: &Path, value: &Value) -> ProjectsResult<PathBuf> {
    let reference = text(value, "artifact", 1024)?;
    let relative = Path::new(&reference);
    if relative.is_absolute()
        || relative.components().any(|c| c.as_os_str() == "..")
        || relative.components().count() == 0
    {
        return Err(crate::error::value(
            "artifact must be a portable project-relative path",
        ));
    }
    let first = relative.components().next().unwrap().as_os_str();
    if first != "scans" && first != "reports" {
        return Err(crate::error::value(
            "evidence belongs under scans/ or reports/",
        ));
    }
    let mut current = root.to_path_buf();
    for part in relative.components() {
        current = current.join(part.as_os_str());
        if current.is_symlink() {
            return Err(crate::error::value("evidence must not traverse symlinks"));
        }
    }
    let resolved = current.canonicalize();
    match resolved {
        // `root` is compared as given (not canonicalized), so a symlinked prefix never matches.
        Ok(path) if path.is_file() && path.starts_with(root) => Ok(path),
        _ => Err(crate::error::value(
            "evidence artifact is missing or unsafe",
        )),
    }
}

/// `_digest`: sha256 in fixed-size blocks, optionally enforcing a read budget.
pub fn digest(
    path: &Path,
    deadline: Option<f64>,
    max_bytes: Option<u64>,
) -> ProjectsResult<String> {
    if let Some(cap) = max_bytes {
        if io(path.metadata())?.len() > cap {
            return Err(crate::error::value(
                "evidence artifact exceeds the hashing byte budget",
            ));
        }
    }
    let mut digest = Sha256::new();
    let mut used = 0u64;
    let mut stream = io(std::fs::File::open(path))?;
    let mut block = vec![0u8; 1024 * 1024];
    loop {
        if let Some(deadline) = deadline {
            if pytime::monotonic() > deadline {
                return Err(crate::error::value("evidence hashing time budget exceeded"));
            }
        }
        let read = io(stream.read(&mut block))?;
        if read == 0 {
            break;
        }
        used += read as u64;
        if let Some(cap) = max_bytes {
            if used > cap {
                return Err(crate::error::value(
                    "evidence artifact exceeds the hashing byte budget",
                ));
            }
        }
        digest.update(&block[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

/// `artifact_identity`: cheap replacement/change identity; not a content digest.
pub fn artifact_identity(path: &Path) -> ProjectsResult<[i64; 5]> {
    use spiderhead_core::filesystem::{path_identity, StatCompat};
    let info = io(path.metadata())?;
    let id = io(path_identity(path))?;
    Ok([
        id.0 as i64,
        id.1 as i64,
        info.size() as i64,
        info.mtime_nsec(),
        info.ctime_nsec(),
    ])
}

fn artifact_receipt(root: &Path, reference: &str) -> ProjectsResult<Value> {
    let path = artifact_path(root, &json!(reference))?;
    let before = artifact_identity(&path)?;
    let digest = digest(&path, None, None)?;
    let after_path = artifact_path(root, &json!(reference))?;
    if artifact_identity(&after_path)? != before {
        return Err(crate::error::value(
            "evidence changed while it was being recorded",
        ));
    }
    Ok(json!({
        "artifact": reference,
        "sha256": digest,
        "artifact_verification": {
            "sha256": digest,
            "identity": before.to_vec(),
            "verified_at": pytime::iso_now(),
        },
    }))
}

/// `audit_facts`: run, check coverage and fired check IDs of a retained scan's audit.
///
/// A streamed scan keeps its audit in the audit.v2 companion; the companion header and
/// its issue collection are read instead of the absent legacy row.
pub fn audit_facts(
    path: &Path,
    con: &rusqlite::Connection,
) -> ProjectsResult<(Value, Value, BTreeSet<String>)> {
    let document: Option<String> = con
        .query_row(
            "SELECT document_json FROM audit WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .ok();
    if let Some(text) = document {
        let audit: Value = serde_json::from_str(&text).map_err(ProjectsError::Json)?;
        let fired = audit["issues"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .filter_map(|i| i.get("check").and_then(Value::as_str).map(str::to_owned))
            .collect();
        return Ok((
            audit.get("run").cloned().unwrap_or(json!({})),
            audit["summary"]["check_coverage"].clone(),
            fired,
        ));
    }
    let reader = spiderhead_store::audit_v2::AuditV2Reader::new(path, true, false, None)
        .map_err(|e| crate::error::value(e.to_string()))?;
    let header = reader.header.clone();
    let issues = reader
        .iter_collection("/issues")
        .map_err(|e| crate::error::value(e.to_string()))?;
    let fired = issues
        .iter()
        .filter_map(|i| i.get("check").and_then(Value::as_str).map(str::to_owned))
        .collect();
    Ok((
        header.get("run").cloned().unwrap_or(json!({})),
        header["summary"]["check_coverage"].clone(),
        fired,
    ))
}

fn saved_check(root: &Path, definition: &Value, value: &Value) -> ProjectsResult<Value> {
    let path = artifact_path(root, value)?;
    let before_identity = artifact_identity(&path)?;
    let before = digest(&path, None, None)?;
    let result = (|| -> ProjectsResult<Value> {
        let con = spiderhead_store::scan_artifact::open_scan(&path, false, None)
            .map_err(|e| crate::error::value(e.to_string()))?;
        let scan = con
            .query_row("SELECT * FROM scan WHERE singleton=1", [], |row| {
                let mut object = Map::new();
                for (index, name) in row.as_ref().column_names().iter().enumerate() {
                    let cell = row
                        .get::<_, rusqlite::types::Value>(index)
                        .unwrap_or(rusqlite::types::Value::Null);
                    let cell = match cell {
                        rusqlite::types::Value::Null => Value::Null,
                        rusqlite::types::Value::Integer(v) => json!(v),
                        rusqlite::types::Value::Real(v) => json!(v),
                        rusqlite::types::Value::Text(v) => json!(v),
                        rusqlite::types::Value::Blob(v) => {
                            json!(format!(
                                "{:x}",
                                v.iter().fold(0u64, |a, b| a.wrapping_add(*b as u64))
                            ))
                        }
                    };
                    object.insert((*name).to_owned(), cell);
                }
                Ok(Value::Object(object))
            })
            .map_err(|e| crate::error::value(e.to_string()))?;
        let operation = definition["operation"].as_str().unwrap_or_default();
        let check_id = operation.strip_prefix("check:").unwrap_or_default();
        let (run, coverage, fired) = audit_facts(&path, &con)?;
        let silent: BTreeSet<String> = coverage["checks_silent_ids"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .filter_map(|i| i.as_str().map(str::to_owned))
            .collect();
        let mut unavailable: BTreeSet<String> = BTreeSet::new();
        for key in ["checks_skipped", "checks_disabled"] {
            for item in run[key].as_array().cloned().unwrap_or_default() {
                if let Some(id) = item.get("id").and_then(Value::as_str) {
                    unavailable.insert(id.to_owned());
                }
            }
        }
        for id in coverage["checks_disabled_ids"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            if let Some(id) = id.as_str() {
                unavailable.insert(id.to_owned());
            }
        }
        if unavailable.contains(check_id)
            || !(fired.contains(check_id) || silent.contains(check_id))
        {
            return Err(crate::error::value(
                "saved artifact does not prove this check ran successfully",
            ));
        }
        if run["crawl_valid"].as_bool() == Some(false) {
            return Err(crate::error::value("saved crawl is invalid"));
        }
        let start = scan["start_url"]
            .as_str()
            .or_else(|| run.get("start_url").and_then(Value::as_str))
            .or_else(|| run.get("source").and_then(Value::as_str));
        let site = definition["scope"]["site"].as_str().unwrap_or_default();
        let same = start
            .and_then(|s| crate::urls::urlsplit(s).ok())
            .zip(crate::urls::urlsplit(site).ok())
            .is_some_and(|(a, b)| a.netloc == b.netloc);
        if !same {
            return Err(crate::error::value(
                "evidence site does not match item scope",
            ));
        }
        let mut pages: BTreeSet<String> = BTreeSet::new();
        let mut statement = con
            .prepare("SELECT url FROM pages JOIN urls USING(url_id)")
            .map_err(|e| crate::error::value(e.to_string()))?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| crate::error::value(e.to_string()))?;
        for row in rows {
            pages.insert(row.map_err(|e| crate::error::value(e.to_string()))?);
        }
        let requested: BTreeSet<String> = definition["scope"]["urls"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|u| u.as_str().map(str::to_owned))
            .collect();
        if !definition["scope"]["template"].is_null() && requested.is_empty() {
            return Err(crate::error::value(
                "template measurements require explicit sample URLs",
            ));
        }
        if !requested.is_empty() && requested != pages {
            return Err(crate::error::value(
                "evidence population must match the requested scope URLs",
            ));
        }
        if pages.is_empty() {
            return Err(crate::error::value(
                "empty scan cannot complete a measurement",
            ));
        }
        let limited = scan["crawl_partial"].as_bool().unwrap_or(false)
            || scan["corpus_partial"].as_bool().unwrap_or(false)
            || !definition["scope"]["template"].is_null()
            || !requested.is_empty();
        let source: Map<String, Value> = [
            "scan_uuid",
            "source_kind",
            "writer_version",
            "writer_revision",
            "config_fingerprint",
            "config_json",
            "finish_reason",
            "lifecycle",
            "evidence_revision",
        ]
        .iter()
        .filter_map(|key| {
            scan.as_object()
                .and_then(|o| o.get(*key).cloned())
                .map(|v| ((*key).to_owned(), v))
        })
        .collect();
        let urls: Vec<String> = pages.iter().take(URL_ENUMERATION_LIMIT).cloned().collect();
        Ok(json!({
            "artifact": value,
            "sha256": before,
            "site": definition["scope"]["site"],
            "scope": definition["scope"],
            "operation": operation,
            "operation_hash": crate::catalogue::catalogue()?[operation]["definition_hash"],
            "source": Value::Object(source),
            "observed_at": run.get("generated_at").cloned().or_else(|| scan.get("created_at").cloned()),
            "measurement": {
                "state": if limited { "limited" } else { "measured" },
                "reason": if limited {
                    "source is partial or scope is an explicit sample"
                } else {
                    "measured population in the saved scan; not a census of the whole site"
                },
                "population": pages.len(),
                "requested_urls": requested.len(),
                "urls": urls,
                "urls_enumerated": pages.len() <= URL_ENUMERATION_LIMIT,
                "crawl_partial": scan["crawl_partial"].as_bool().unwrap_or(false),
                "corpus_partial": scan["corpus_partial"].as_bool().unwrap_or(false),
            },
        }))
    })();
    let result = result?;
    if digest(&path, None, None)? != before
        || artifact_identity(&artifact_path(root, value)?)? != before_identity
    {
        return Err(crate::error::value(
            "evidence changed while it was being recorded",
        ));
    }
    let mut result = result;
    result["artifact_verification"] = json!({
        "sha256": before,
        "identity": before_identity.to_vec(),
        "verified_at": pytime::iso_now(),
    });
    Ok(result)
}

/// `validate_record`: check one caller-supplied execution record into its stored shape.
pub fn validate_record(root: &Path, definition: &Value, record: &Value) -> ProjectsResult<Value> {
    let object = record.as_object().filter(|o| {
        o.keys().all(|k| {
            matches!(
                k.as_str(),
                "status"
                    | "reason"
                    | "artifact"
                    | "evidence"
                    | "reviewer"
                    | "actor"
                    | "review"
                    | "signoff"
            )
        })
    });
    let Some(object) = object else {
        return Err(crate::error::value("record has unsupported fields"));
    };
    let status = object
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !matches!(
        status,
        "running" | "failed" | "unavailable" | "succeeded" | "not_applicable"
    ) {
        return Err(crate::error::value("invalid execution status"));
    }
    let reason = text(
        object.get("reason").unwrap_or(&Value::Null),
        "record reason",
        2048,
    )?;
    let mut result = json!({"status": status, "reason": reason, "measurement": null});
    if let Some(actor) = object.get("actor") {
        result["actor"] = json!(text(actor, "record actor", 128)?);
    }
    if matches!(status, "running" | "failed" | "unavailable") {
        if object
            .keys()
            .any(|k| !matches!(k.as_str(), "status" | "reason" | "actor"))
        {
            return Err(crate::error::value(
                "unfinished attempts record status, reason and optional actor only",
            ));
        }
        return Ok(result);
    }
    if status == "not_applicable" {
        if object.keys().any(|k| {
            !matches!(
                k.as_str(),
                "status" | "reason" | "reviewer" | "artifact" | "evidence" | "actor"
            )
        }) {
            return Err(crate::error::value(
                "applicability review records reason, reviewer and evidence basis",
            ));
        }
        result["reviewer"] = json!(text(
            object.get("reviewer").unwrap_or(&Value::Null),
            "reviewer",
            128,
        )?);
        if object.contains_key("artifact") {
            let reference = object["artifact"].as_str().unwrap_or_default();
            let path = artifact_path(root, &object["artifact"])?;
            if io(path.metadata())?.len() == 0 {
                return Err(crate::error::value(
                    "empty artifact cannot support an applicability decision",
                ));
            }
            let receipt = artifact_receipt(root, reference)?;
            for (k, v) in receipt.as_object().unwrap().clone() {
                result[k] = v;
            }
        }
        if object.contains_key("evidence") {
            result["evidence"] = json!(text(&object["evidence"], "exclusion evidence", 512)?);
        }
        if !result.as_object().unwrap().contains_key("artifact")
            && !result.as_object().unwrap().contains_key("evidence")
        {
            return Err(crate::error::value(
                "applicability review requires an inspectable evidence basis",
            ));
        }
        return Ok(result);
    }
    let kind = definition
        .get("execution_kind")
        .and_then(Value::as_str)
        .ok_or_else(|| crate::error::py_named("KeyError", "'execution_kind'"))?;
    if kind == "automatic" {
        if object
            .keys()
            .any(|k| !matches!(k.as_str(), "status" | "reason" | "artifact" | "actor"))
        {
            return Err(crate::error::value(
                "automatic evidence cannot be replaced by manual signoff",
            ));
        }
        let saved = saved_check(
            root,
            definition,
            object.get("artifact").unwrap_or(&Value::Null),
        )?;
        for (k, v) in saved.as_object().unwrap().clone() {
            result[k] = v;
        }
    } else {
        result["reviewer"] = json!(text(
            object.get("reviewer").unwrap_or(&Value::Null),
            "reviewer",
            128,
        )?);
        if object.contains_key("signoff") && !object["signoff"].is_boolean() {
            return Err(crate::error::value("signoff must be a boolean"));
        }
        if kind == "manual" && object.get("signoff").and_then(Value::as_bool) == Some(true) {
            if object.contains_key("artifact") || object.contains_key("review") {
                return Err(crate::error::value(
                    "use either explicit signoff or reviewed evidence",
                ));
            }
            result["signoff"] = json!(true);
        } else {
            if object.get("review").and_then(Value::as_str) != Some("approved") {
                return Err(crate::error::value(
                    "completion requires approved artifact review",
                ));
            }
            let path = artifact_path(root, object.get("artifact").unwrap_or(&Value::Null))?;
            if io(path.metadata())?.len() == 0 {
                return Err(crate::error::value(
                    "empty artifact cannot complete a review",
                ));
            }
            let receipt = artifact_receipt(root, object["artifact"].as_str().unwrap_or_default())?;
            for (k, v) in receipt.as_object().unwrap().clone() {
                result[k] = v;
            }
            result["review"] = json!("approved");
        }
        result["scope"] = definition
            .get("scope")
            .cloned()
            .ok_or_else(|| crate::error::py_named("KeyError", "'scope'"))?;
        result["measurement"] = json!({
            "state": "not_measured",
            "reason": "specialist review or signoff; no automatic measurement is implied",
        });
    }
    Ok(result)
}

/// `evidence_stale`: the reason a record's evidence is stale, or empty.
pub fn evidence_stale(
    root: &Path,
    record: &Value,
    catalogue: &Map<String, Value>,
    digests: &mut HashMap<(String, [i64; 5]), String>,
    verify_bytes: bool,
) -> String {
    if record.get("artifact").is_some() {
        let stale = (|| -> Result<(), String> {
            let artifact = record["artifact"].as_str().unwrap_or_default();
            let path = artifact_path(root, &record["artifact"])
                .map_err(|_| "evidence artifact is missing or unsafe".to_owned())?;
            let identity = artifact_identity(&path)
                .map_err(|_| "evidence artifact is missing or unsafe".to_owned())?;
            if verify_bytes {
                let key = (artifact.to_owned(), identity);
                if !digests.contains_key(&key) {
                    let computed = digest(&path, None, None)
                        .map_err(|_| "evidence artifact is missing or unsafe".to_owned())?;
                    digests.insert(key.clone(), computed);
                }
                let second = artifact_identity(
                    &artifact_path(root, &record["artifact"])
                        .map_err(|_| "evidence artifact is missing or unsafe".to_owned())?,
                )
                .map_err(|_| "evidence artifact is missing or unsafe".to_owned())?;
                if digests[&key] != record["sha256"].as_str().unwrap_or_default()
                    || second != identity
                {
                    return Err("evidence artifact changed".to_owned());
                }
            } else {
                let receipt = record.get("artifact_verification");
                if !receipt.is_some_and(Value::is_object) {
                    return Err(
                        "evidence unverified: no retained byte-verification metadata".into(),
                    );
                }
                let receipt = receipt.unwrap();
                if receipt["sha256"].as_str() != record["sha256"].as_str()
                    || receipt["identity"].as_array()
                        != Some(&identity.into_iter().map(Value::from).collect::<Vec<_>>())
                {
                    return Err(
                        "evidence artifact metadata changed; explicit byte verification required"
                            .into(),
                    );
                }
            }
            Ok(())
        })();
        if let Err(reason) = stale {
            return reason;
        }
    }
    if record.get("operation").is_some() {
        let operation = record["operation"].as_str().unwrap_or_default();
        let current = catalogue.get(operation);
        if current.is_none()
            || current.unwrap()["definition_hash"].as_str() != record["operation_hash"].as_str()
        {
            return "evidence operation definition changed".into();
        }
    }
    String::new()
}

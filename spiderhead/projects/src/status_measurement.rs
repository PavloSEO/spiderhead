//! Persisted bounded scan-directory size observation for cheap project status reads

use std::path::Path;

use serde_json::{json, Value};

use crate::error::ProjectsResult;
use crate::pytime;
use crate::runtime::{read_document, write_document};
use crate::workspace::load as workspace_load;

/// Measurement document name.
pub const FILE: &str = "project-measurement.json";
/// Document format.
pub const FORMAT: &str = "spiderhead.project-measurement.v1";
/// Scan-entry ceiling for one measurement pass.
pub const MAX_ENTRIES: usize = 10_000;

fn missing() -> Value {
    json!({
        "state": "unavailable",
        "relative_path": "scans",
        "total_bytes": null,
        "scan_count": null,
        "measured_at": null,
        "reason": "measurement has not been recorded",
    })
}

fn document(directory: &Path) -> ProjectsResult<(std::path::PathBuf, Value, Option<Value>)> {
    let (root, project) = workspace_load(directory)?;
    let current = read_document(&root, FILE)?;
    if let Some(current) = &current {
        let ok = current
            .as_object()
            .map(|o| {
                o.keys()
                    .map(String::as_str)
                    .collect::<std::collections::BTreeSet<_>>()
                    == ["format", "version", "project_uuid", "revision", "storage"]
                        .into_iter()
                        .collect()
            })
            .unwrap_or(false)
            && current["format"].as_str() == Some(FORMAT)
            && current["version"].as_i64() == Some(1)
            && !current["version"].is_boolean()
            && current["project_uuid"].as_str() == project["project_uuid"].as_str()
            && current["revision"].as_i64().is_some_and(|r| r >= 1)
            && !current["revision"].is_boolean();
        if !ok {
            return Err(crate::error::value(
                "project measurement has an unsupported shape",
            ));
        }
        let storage = current["storage"].as_object().filter(|s| {
            s.keys()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>()
                == [
                    "state",
                    "relative_path",
                    "total_bytes",
                    "scan_count",
                    "measured_at",
                    "reason",
                ]
                .into_iter()
                .collect()
        });
        let Some(storage) = storage else {
            return Err(crate::error::value(
                "project storage measurement has an unsupported shape",
            ));
        };
        if !["available", "unavailable"].contains(&storage["state"].as_str().unwrap_or_default())
            || storage["relative_path"].as_str() != Some("scans")
        {
            return Err(crate::error::value(
                "project storage measurement state is invalid",
            ));
        }
        if !storage["measured_at"].is_null() && !storage["measured_at"].is_string() {
            return Err(crate::error::value(
                "project storage measurement timestamp is invalid",
            ));
        }
        if !storage["reason"].is_null() && !storage["reason"].is_string() {
            return Err(crate::error::value(
                "project storage measurement reason is invalid",
            ));
        }
        for name in ["total_bytes", "scan_count"] {
            let value = &storage[name];
            if !value.is_null() && (value.as_i64().is_none_or(|v| v < 0) || value.is_boolean()) {
                return Err(crate::error::value(format!(
                    "project storage {name} must be a nonnegative integer or null"
                )));
            }
        }
        if storage["state"].as_str() == Some("available")
            && (storage["total_bytes"].is_null()
                || storage["scan_count"].is_null()
                || !storage["reason"].is_null())
        {
            return Err(crate::error::value(
                "available project storage measurement is incomplete",
            ));
        }
        if storage["state"].as_str() == Some("unavailable")
            && (!storage["total_bytes"].is_null()
                || !storage["scan_count"].is_null()
                || storage["reason"].as_str().unwrap_or_default().is_empty())
        {
            return Err(crate::error::value(
                "unavailable project storage measurement requires a reason",
            ));
        }
    }
    Ok((root, project, current))
}

/// Read the persisted observation without walking project files.
pub fn read_storage_measurement(directory: &Path) -> ProjectsResult<Value> {
    let (_root, _project, current) = document(directory)?;
    Ok(match current {
        Some(c) => c["storage"].clone(),
        None => missing(),
    })
}

/// Measure direct regular files in `scans/` once and persist the result.
pub fn refresh_storage_measurement(directory: &Path) -> ProjectsResult<Value> {
    let (root, project, current) = document(directory)?;
    let measured_at = pytime::zulu();
    let mut storage = missing();
    storage["measured_at"] = json!(measured_at);
    storage["reason"] = Value::Null;
    let scans = root.join("scans");
    let measured = (|| -> Result<(i64, i64), String> {
        let entries = std::fs::read_dir(&scans)
            .map_err(|e| format!("scan directory could not be measured: {}", kind(&e)))?;
        let mut total_bytes = 0i64;
        let mut scan_count = 0i64;
        for (number, entry) in entries.enumerate() {
            if number + 1 > MAX_ENTRIES {
                return Err(format!("scans directory exceeds {MAX_ENTRIES} entries"));
            }
            let entry =
                entry.map_err(|e| format!("scan directory could not be measured: {}", kind(&e)))?;
            let info = entry
                .metadata()
                .map_err(|e| format!("scan directory could not be measured: {}", kind(&e)))?;
            if info.file_type().is_symlink() {
                return Err("scans directory contains a symbolic link".to_owned());
            }
            if info.is_dir() {
                return Err(
                    "nested scan directories are not included in this measurement".to_owned(),
                );
            }
            if !info.is_file() {
                return Err("scans directory contains a non-regular entry".to_owned());
            }
            total_bytes += info.len() as i64;
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if name.ends_with(".sqlite") || name.ends_with(".sqlite3") || name.ends_with(".db") {
                scan_count += 1;
            }
        }
        Ok((total_bytes, scan_count))
    })();
    match measured {
        Ok((total_bytes, scan_count)) => {
            storage["state"] = json!("available");
            storage["total_bytes"] = json!(total_bytes);
            storage["scan_count"] = json!(scan_count);
        }
        Err(reason) => {
            storage["reason"] = json!(reason);
        }
    }
    let document = json!({
        "format": FORMAT,
        "version": 1,
        "project_uuid": project["project_uuid"],
        "revision": current.as_ref().and_then(|c| c["revision"].as_i64()).unwrap_or(0) + 1,
        "storage": storage,
    });
    write_document(
        &root,
        FILE,
        &document,
        current.and_then(|c| c["revision"].as_i64()),
    )?;
    Ok(storage)
}

/// Error-kind name recorded for an I/O failure, using the stable document spelling.
fn kind(e: &std::io::Error) -> &'static str {
    use std::io::ErrorKind::*;
    match e.kind() {
        NotFound => "FileNotFoundError",
        PermissionDenied => "PermissionError",
        AlreadyExists => "FileExistsError",
        _ => "OSError",
    }
}

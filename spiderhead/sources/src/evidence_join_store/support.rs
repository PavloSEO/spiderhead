//! Split out of `evidence_join_store`.

use super::*;

pub(super) fn store_error(message: impl Into<String>) -> CfError {
    CfError::Python("EvidenceJoinStoreError", message.into())
}

/// `storage.open_scan` — `(scan_uuid, (page_ordinal, url) rows)` for one scan.
pub type ScanRows<'a> = &'a dyn Fn(&str) -> Result<(String, Vec<(i64, String)>), CfError>;

pub(super) fn update(digest: &mut sha2::Sha256, values: &[String]) {
    for value in values {
        digest.update(value.as_bytes());
        digest.update(b"\0");
    }
}

pub(super) fn url_policy(value: Option<&Value>) -> Result<Map<String, Value>, CfError> {
    let mut policy = Map::from_iter(
        ["ignore_query", "ignore_scheme", "casefold_path"]
            .iter()
            .map(|k| (k.to_string(), json!(false))),
    );
    let supplied = value.filter(|v| pystr::truthy(Some(v)));
    if let Some(spec) = supplied {
        let entries = spec
            .as_object()
            .ok_or_else(|| store_error("unsupported evidence-join URL policy"))?;
        if entries
            .keys()
            .any(|k| !["ignore_query", "ignore_scheme", "casefold_path"].contains(&k.as_str()))
        {
            return Err(store_error("unsupported evidence-join URL policy"));
        }
        for (key, enabled) in entries {
            if !enabled.is_boolean() {
                return Err(store_error(format!("URL policy {key} must be boolean")));
            }
            policy.insert(key.clone(), enabled.clone());
        }
    }
    Ok(policy)
}

pub(super) fn new_target(value: &str) -> Result<PathBuf, CfError> {
    let path = std::fs::canonicalize(Path::new(value).parent().unwrap_or(Path::new(".")))
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(Path::new(value).file_name().unwrap_or_default());
    let invalid = path.exists()
        || path
            .symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        || path
            .parent()
            .and_then(|p| p.symlink_metadata().ok())
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        || !path.parent().map(|p| p.is_dir()).unwrap_or(false);
    if invalid {
        return Err(store_error(
            "evidence join store must be a new file in an existing non-symlink directory",
        ));
    }
    Ok(path)
}

pub(super) fn open_read(path: &Path) -> Result<Connection, CfError> {
    if path
        .symlink_metadata()
        .map(|m| m.file_type().is_symlink() || !m.is_file())
        .unwrap_or(true)
    {
        return Err(store_error(
            "evidence join store must be a regular non-symlink file",
        ));
    }
    let con = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| store_error(format!("cannot open evidence join store: {e}")))?;
    con.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA query_only=ON")
        .map_err(|e| store_error(format!("cannot open evidence join store: {e}")))?;
    Ok(con)
}

pub(super) fn validate_metadata(metadata: &Value) -> Result<(), CfError> {
    if !metadata.is_object() || metadata.get("format") != Some(&json!(FORMAT)) {
        return Err(store_error(format!("expected {FORMAT} metadata")));
    }
    for name in ["crawl", "evidence", "summary"] {
        if !metadata.get(name).is_some_and(Value::is_object) {
            return Err(store_error("evidence join store metadata is incomplete"));
        }
    }
    let summary = &metadata["summary"];
    for name in [
        "pages",
        "rows",
        "matched_rows",
        "matched_pages",
        "crawl_only",
        "external_only",
        "unkeyable_rows",
        "unkeyable_pages",
        "candidate_pairs",
    ] {
        let value = summary.get(name);
        if !pystr::is_py_int(value.unwrap_or(&Value::Null))
            || value.and_then(Value::as_i64).unwrap_or(-1) < 0
        {
            return Err(store_error(
                "evidence join store has invalid population counts",
            ));
        }
    }
    let get = |n: &str| summary[n].as_i64().unwrap_or_default();
    if get("matched_rows") + get("external_only") + get("unkeyable_rows") != get("rows") {
        return Err(store_error(
            "evidence join store does not conserve evidence rows",
        ));
    }
    if get("matched_pages") + get("crawl_only") + get("unkeyable_pages") != get("pages") {
        return Err(store_error(
            "evidence join store does not conserve crawl pages",
        ));
    }
    Ok(())
}

pub(super) fn metadata_document(path: &Path) -> Result<Value, CfError> {
    let con = open_read(path)?;
    let row: Option<String> = con
        .query_row("SELECT value FROM meta WHERE key='document'", [], |r| {
            r.get(0)
        })
        .ok();
    let Some(text) = row else {
        return Err(store_error("evidence join store has no metadata"));
    };
    let metadata: Value = serde_json::from_str(&text)
        .map_err(|_| store_error("evidence join store metadata is invalid JSON"))?;
    validate_metadata(&metadata)?;
    Ok(metadata)
}

/// `is_store(value)` — True when the file is a well-formed join store.
pub fn is_store(value: &str) -> bool {
    metadata_document(Path::new(value))
        .map(|metadata| metadata.get("format") == Some(&json!(FORMAT)))
        .unwrap_or(false)
}

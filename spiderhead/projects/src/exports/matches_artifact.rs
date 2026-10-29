use super::*;

/// `_matches_artifact`: verify a prepared artifact against its recorded checksum.
pub(super) fn matches_artifact(path: &Path, result: &Value) -> bool {
    let bytes = result.get("bytes");
    let valid_record = result.is_object()
        && result["path"].is_string()
        && bytes.is_some_and(|b| b.is_i64() || b.is_u64())
        && bytes
            .and_then(|b| b.as_i64())
            .is_some_and(|b| (0..=MAX_EXPORT_BYTES as i64).contains(&b))
        && result["sha256"].as_str().is_some_and(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        });
    if !valid_record {
        return false;
    }
    let (exists, symlink) = path_flags(path);
    if symlink || !exists || !path.is_file() {
        return false;
    }
    let expected = result["bytes"].as_i64().unwrap_or(0) as u64;
    let actual_size = match path.metadata() {
        Ok(meta) => meta.len(),
        Err(_) => return false,
    };
    if actual_size != expected {
        return false;
    }
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut reader = std::io::BufReader::new(file);
    let mut digest = Sha256::new();
    let mut block = [0u8; 1024 * 1024];
    loop {
        match reader.read(&mut block) {
            Ok(0) => break,
            Ok(n) => digest.update(&block[..n]),
            Err(_) => return false,
        }
    }
    format!("{:x}", digest.finalize()) == result["sha256"].as_str().unwrap_or_default()
}

/// `_known_scan_uuids`: retained scan ids across this project and linked competitors.
pub(super) fn known_scan_uuids(root: &Path) -> ProjectsResult<HashSet<String>> {
    let mut directories = vec![root.join("scans")];
    let prep = crate::runtime::preparation_status(root)?;
    let competitors = prep
        .get("competitors")
        .filter(|v| !crate::schedule::falsy(v))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let root_resolved = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    for row in competitors {
        let Some(relative) = row.get("directory").and_then(|v| v.as_str()) else {
            continue;
        };
        let rel_path = Path::new(relative);
        let has_parent = rel_path
            .components()
            .any(|c| matches!(c, Component::ParentDir));
        if rel_path.is_absolute() || has_parent || !relative.starts_with("competitors/") {
            continue;
        }
        let child = root.join(rel_path);
        if child.is_symlink() {
            continue;
        }
        let child_resolved = match child.canonicalize() {
            Ok(p) if p.starts_with(&root_resolved) => p,
            _ => continue,
        };
        let loaded = crate::workspace::load(&child);
        let (child_root, child_project) = match loaded {
            Ok(pair) => pair,
            Err(
                ProjectsError::Value(_)
                | ProjectsError::Py { .. }
                | ProjectsError::Json(_)
                | ProjectsError::Io(_),
            ) => continue,
            Err(e) => return Err(e),
        };
        if child_root != child_resolved
            || child_project.get("project_uuid") != row.get("project_uuid")
        {
            continue;
        }
        directories.push(child_root.join("scans"));
    }
    let mut known = HashSet::new();
    for directory in directories {
        if !directory.is_dir() {
            continue;
        }
        let mut offset = 0i64;
        loop {
            let page = spiderhead_store::history::list_scans(&directory, offset, 100)?;
            if !crate::schedule::falsy(&page["errors"]) {
                return Err(export_error(
                    "one or more retained scan records are unavailable",
                ));
            }
            let items = page["items"].as_array().cloned().unwrap_or_default();
            for row in &items {
                if !crate::schedule::falsy(&row["uuid"]) {
                    if let Some(uuid) = row["uuid"].as_str() {
                        known.insert(uuid.to_string());
                    }
                }
            }
            offset += items.len() as i64;
            if crate::schedule::falsy(&page["has_more"]) {
                break;
            }
        }
    }
    Ok(known)
}

/// `_compare_source`: validate a compare.v2 manifest and bound its dataset files.
pub(super) fn compare_source(
    root: &Path,
    manifest_path: &str,
) -> ProjectsResult<(PathBuf, Value, Vec<String>)> {
    let path = PathBuf::from(manifest_path);
    if path.is_symlink()
        || !path.is_file()
        || path.metadata().map(|m| m.len()).unwrap_or(u64::MAX) > MANIFEST_BYTES
    {
        return Err(export_error(
            "compare manifest must be a bounded regular local file",
        ));
    }
    let text = std::fs::read_to_string(&path).map_err(|e| {
        let (_, s) = py_exception_name(&ProjectsError::Io(e));
        export_error(&format!("cannot read compare manifest: {s}"))
    })?;
    let manifest: Value = serde_json::from_str(&text).map_err(|e| {
        let (_, s) = py_exception_name(&ProjectsError::Json(e));
        export_error(&format!("cannot read compare manifest: {s}"))
    })?;
    if !manifest.is_object()
        || manifest["schema_version"].as_str() != Some(spiderhead_core::artifacts::COMPARE_V2)
    {
        return Err(export_error(
            "compare dataset requires a compare.v2 manifest",
        ));
    }
    let sources = [manifest.get("before"), manifest.get("after")];
    if sources.iter().any(|s| {
        !s.is_some_and(|v| v.is_object()) || crate::schedule::falsy(&s.unwrap()["scan_uuid"])
    }) {
        return Err(export_error(
            "compare manifest has no exact retained scan identities",
        ));
    }
    let known = known_scan_uuids(root)?;
    let mut unknown = Vec::new();
    for source in sources.iter().flatten() {
        let uuid = &source["scan_uuid"];
        let known_hit = uuid.as_str().is_some_and(|u| known.contains(u));
        if !known_hit {
            unknown.push(uuid.clone());
        }
    }
    if !unknown.is_empty() {
        return Err(export_error(
            "both compare scans must belong to this project or a competitor workspace",
        ));
    }
    let files = manifest
        .get("files")
        .filter(|f| f.is_object())
        .ok_or_else(|| export_error("compare manifest has no bounded dataset files"))?;
    let files = files.as_object().cloned().unwrap_or_default();
    let mut names: Vec<String> = files
        .iter()
        .filter(|(_, item)| item.is_object())
        .map(|(name, _)| name.clone())
        .collect();
    names.sort();
    if names.is_empty() {
        return Err(export_error("compare manifest contains no datasets"));
    }
    let mut rows: i64 = 0;
    for name in &names {
        rows += py_int(
            &files[name.as_str()]
                .get("rows")
                .cloned()
                .unwrap_or(json!(0)),
        )?;
    }
    if rows > MAX_EXPORT_ROWS {
        return Err(export_error(&format!(
            "comparison exceeds the {MAX_EXPORT_ROWS}-row export bound"
        )));
    }
    Ok((path, manifest, names))
}

/// `_task_rows`: flatten the coverage document into the task export row set.
pub(super) fn task_rows(directory: &Path) -> ProjectsResult<(Vec<Value>, i64)> {
    let (root, project) = crate::workspace::load(directory)?;
    let document = crate::coverage::store::read(&root, &project)?
        .ok_or_else(|| export_error("initialize the project checklist before exporting tasks"))?;
    let catalogue = crate::catalogue::load_catalogue()?;
    let status =
        crate::coverage::status(&root, document.clone(), &catalogue, Some(&project), true)?;
    if status["state"].as_str() != Some("initialized") {
        return Err(export_error(
            "initialize the project checklist before exporting tasks",
        ));
    }
    let mut status_by_id: Map<String, Value> = Map::new();
    for item in status
        .get("items")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        // The map is keyed by the raw `item["id"]` value; a non-string id can
        // never match a string task id, so only string keys are stored.
        let id = obj_get(item, "id")?;
        if let Some(id) = id.as_str() {
            status_by_id.insert(id.to_string(), item.clone());
        }
    }
    let mut rows = Vec::new();
    let items = document_items(&document)?;
    let mut item_ids: Vec<&String> = items.keys().collect();
    item_ids.sort();
    for item_id in item_ids {
        let stored = &items[item_id.as_str()];
        let Some(item) = status_by_id.get(item_id) else {
            continue;
        };
        let definition = obj_get(stored, "definition")?;
        let task = item.get("task").filter(|t| !crate::schedule::falsy(t));
        if task.is_some_and(|t| !t.is_object()) {
            return Err(py_named(
                "AttributeError",
                format!(
                    "'{}' object has no attribute 'get'",
                    py_typename(task.unwrap_or(&Value::Null))
                ),
            ));
        }
        let task_obj = task.and_then(|t| t.as_object());
        let affected = item
            .get("affected_urls")
            .filter(|a| !crate::schedule::falsy(a));
        if affected.is_some_and(|a| !a.is_object()) {
            return Err(py_named(
                "AttributeError",
                format!(
                    "'{}' object has no attribute 'get'",
                    py_typename(affected.unwrap_or(&Value::Null))
                ),
            ));
        }
        let affected_obj = affected.and_then(|a| a.as_object());
        let get = |v: Option<&Map<String, Value>>, key: &str| -> Value {
            v.and_then(|m| m.get(key).cloned()).unwrap_or(Value::Null)
        };
        let competitors = get(task_obj, "competitors");
        let competitors = if crate::schedule::falsy(&competitors) {
            json!([])
        } else {
            competitors
        };
        rows.push(json!({
            "task_id": item_id,
            "title": item.get("title").cloned().unwrap_or(Value::Null),
            "kind": definition.get("kind").cloned().unwrap_or(Value::Null),
            "source_hash": definition.get("source_hash").cloned().unwrap_or(Value::Null),
            "operation": definition.get("operation").cloned().unwrap_or(Value::Null),
            "state": crate::progress::item_state(item),
            "attempt_status": item.get("attempt_status").cloned().unwrap_or(Value::Null),
            "assignee": get(task_obj, "assignee"),
            "description": get(task_obj, "description"),
            "acceptance_criterion": get(task_obj, "acceptance_criterion"),
            "finding_ref": get(task_obj, "finding_ref"),
            "method_ref": get(task_obj, "method_ref"),
            "competitors": competitors,
            "affected_url_count": get(affected_obj, "count"),
            "affected_url_state": match affected_obj {
                Some(m) => m.get("state").cloned().unwrap_or_else(|| json!("not_measured")),
                None => json!("not_measured"),
            },
            "affected_url_reason": get(affected_obj, "reason"),
            "actor": item.get("actor").cloned().unwrap_or(Value::Null),
            "complete": !crate::schedule::falsy(&item["complete"]),
            "reason": item.get("reason").cloned().unwrap_or(Value::Null),
        }));
    }
    let revision = obj_get(&status, "revision")?.as_i64().unwrap_or(0);
    Ok((rows, revision))
}

/// `_task_related_rows`: stream comments or definition/attempt history.
pub(super) fn task_related_rows(
    directory: &Path,
    dataset: &str,
) -> ProjectsResult<(Vec<Value>, i64, Value)> {
    let (root, project) = crate::workspace::load(directory)?;
    let document = crate::coverage::store::read(&root, &project)?.ok_or_else(|| {
        export_error("initialize the project checklist before exporting task history")
    })?;
    let items = document_items(&document)?;
    if dataset == "task_comments" {
        let mut total = 0i64;
        for item in items.values() {
            total += item
                .get("comments")
                .and_then(|c| c.as_array())
                .map(|c| c.len() as i64)
                .unwrap_or(0);
        }
        let mut rows = Vec::new();
        let mut task_ids: Vec<&String> = items.keys().collect();
        task_ids.sort();
        for task_id in task_ids {
            let item = &items[task_id.as_str()];
            for (index, comment) in item
                .get("comments")
                .and_then(|c| c.as_array())
                .cloned()
                .unwrap_or_default()
                .iter()
                .enumerate()
            {
                rows.push(json!({
                    "task_id": task_id,
                    "comment_ordinal": index as i64 + 1,
                    "created_at": obj_get(comment, "created_at")?,
                    "actor": obj_get(comment, "actor")?,
                    "text": obj_get(comment, "text")?,
                    "reference": comment.get("reference").cloned().unwrap_or(Value::Null),
                }));
            }
        }
        return Ok((
            rows,
            total,
            json!({"checklist_revision": document["revision"].clone()}),
        ));
    }
    if dataset != "task_history" {
        return Err(export_error(
            "dataset must be tasks, task_comments, task_history, or compare",
        ));
    }
    let mut total = 0i64;
    for item in items.values() {
        total += obj_get(item, "definitions")?
            .as_array()
            .map(|d| d.len() as i64)
            .unwrap_or(0);
        total += obj_get(item, "records")?
            .as_array()
            .map(|r| r.len() as i64)
            .unwrap_or(0);
    }
    let mut rows = Vec::new();
    let mut task_ids: Vec<&String> = items.keys().collect();
    task_ids.sort();
    for task_id in task_ids {
        let item = &items[task_id.as_str()];
        let mut events: Vec<(Value, i64, &str, Value)> = Vec::new();
        for version in obj_get(item, "definitions")?
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            events.push((
                version.get("observed_at").cloned().unwrap_or(Value::Null),
                0,
                "definition",
                version,
            ));
        }
        for record in obj_get(item, "records")?
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            events.push((
                record.get("recorded_at").cloned().unwrap_or(Value::Null),
                1,
                "attempt",
                record,
            ));
        }
        events.sort_by(|a, b| {
            pyjson::dumps_compact(&a.0)
                .cmp(&pyjson::dumps_compact(&b.0))
                .then(a.1.cmp(&b.1))
        });
        for (ordinal, (occurred_at, _tie, event_type, entry)) in events.into_iter().enumerate() {
            let is_attempt = event_type == "attempt";
            rows.push(json!({
                "task_id": task_id,
                "event_type": event_type,
                "event_ordinal": ordinal as i64 + 1,
                "occurred_at": occurred_at,
                "revision": if is_attempt { entry.get("revision").cloned().unwrap_or(Value::Null) } else { Value::Null },
                "actor": if is_attempt {
                    entry.get("actor").filter(|a| !crate::schedule::falsy(a))
                        .cloned()
                        .or_else(|| entry.get("reviewer").cloned())
                        .unwrap_or(Value::Null)
                } else { Value::Null },
                "status": if is_attempt { entry.get("status").cloned().unwrap_or(Value::Null) } else { Value::Null },
                "entry_json": entry,
            }));
        }
    }
    Ok((
        rows,
        total,
        json!({"checklist_revision": document["revision"].clone()}),
    ))
}

/// Validate and read one compare dataset file.
pub(super) fn compare_rows(manifest_path: &Path, name: &str) -> ProjectsResult<Vec<Value>> {
    match compare_rows_inner(manifest_path, name) {
        Err(
            e @ ProjectsError::Py {
                name: "CompareError",
                ..
            },
        ) => Err(e),
        Err(e) => {
            let (_, text) = py_exception_name(&e);
            Err(compare_error(&format!(
                "invalid comparison file '{name}': {text}"
            )))
        }
        Ok(rows) => Ok(rows),
    }
}

pub(super) fn compare_rows_inner(manifest_path: &Path, name: &str) -> ProjectsResult<Vec<Value>> {
    let text = std::fs::read_to_string(manifest_path).map_err(ProjectsError::Io)?;
    let manifest: Value = serde_json::from_str(&text).map_err(ProjectsError::Json)?;
    if manifest["schema_version"].as_str() != Some(spiderhead_core::artifacts::COMPARE_V2) {
        return Err(compare_error("unsupported comparison manifest schema"));
    }
    let entry = &manifest["files"][name];
    if entry.is_null() {
        return Err(key_error(name));
    }
    let relative = obj_get(entry, "path")?;
    if !relative.is_string()
        || relative
            .as_str()
            .is_some_and(|r| py_path_name(r) != r || r == "." || r == "..")
    {
        return Err(compare_error(
            "comparison file path must be a local filename",
        ));
    }
    let relative = relative.as_str().unwrap_or_default();
    let path = manifest_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(relative);
    if path.is_symlink() {
        return Err(compare_error("comparison file must not be a symbolic link"));
    }
    let format_ = obj_get(entry, "format")?
        .as_str()
        .unwrap_or_default()
        .to_string();
    if format_ != "ndjson" && format_ != "ndjson.gz" {
        return Err(compare_error("unsupported comparison file format"));
    }
    let compressed = format_ == "ndjson.gz";
    if compressed && entry.get("compression").and_then(|v| v.as_str()) != Some("gzip") {
        return Err(compare_error("comparison compression metadata mismatch"));
    }
    let expected_size = if compressed {
        obj_get(entry, "uncompressed_bytes")?
    } else {
        obj_get(entry, "bytes")?
    };
    for v in [
        obj_get(entry, "bytes")?,
        expected_size,
        obj_get(entry, "rows")?,
    ] {
        if !v.is_i64() && !v.is_u64() || v.as_i64().is_some_and(|n| n < 0) {
            return Err(compare_error("invalid comparison file count metadata"));
        }
    }
    let expected_size = expected_size.as_i64().unwrap_or(0);
    let expected_rows = obj_get(entry, "rows")?.as_i64().unwrap_or(0);
    let raw = std::fs::read(&path).map_err(ProjectsError::Io)?;
    let digest = format!("{:x}", Sha256::digest(&raw));
    if raw.len() as i64 != obj_get(entry, "bytes")?.as_i64().unwrap_or(-1)
        || digest != entry["sha256"].as_str().unwrap_or_default()
    {
        return Err(compare_error(
            "comparison file checksum or byte count mismatch",
        ));
    }
    let mut rows = Vec::new();
    let mut size = 0i64;
    let mut count = 0i64;
    let mut stream: Box<dyn Read> = if compressed {
        Box::new(flate2::read::GzDecoder::new(&raw[..]))
    } else {
        Box::new(&raw[..])
    };
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let n = stream.read(&mut chunk).map_err(ProjectsError::Io)?;
        if n == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..n]);
        while let Some(pos) = buffer.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buffer.drain(..=pos).collect();
            size += line.len() as i64;
            count += 1;
            if size > expected_size || count > expected_rows || !line.ends_with(b"\n") {
                return Err(compare_error(
                    "comparison file row framing or count mismatch",
                ));
            }
            let row: Value = serde_json::from_slice(&line).map_err(ProjectsError::Json)?;
            if !row.is_object() {
                return Err(compare_error("comparison file contains a non-object row"));
            }
            rows.push(row);
        }
    }
    if !buffer.is_empty() {
        size += buffer.len() as i64;
        count += 1;
        if size > expected_size || count > expected_rows {
            return Err(compare_error(
                "comparison file row framing or count mismatch",
            ));
        }
        // A trailing line without "\n" violates the `line.endswith(b"\n")` check.
        return Err(compare_error(
            "comparison file row framing or count mismatch",
        ));
    }
    if count != expected_rows || size != expected_size {
        return Err(compare_error(
            "comparison file row or decoded byte count mismatch",
        ));
    }
    Ok(rows)
}

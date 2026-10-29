use super::*;

/// `record.get(outer, {}).get(key)` — `AttributeError` when `outer` is present but not a dict.
pub(super) fn nested_field(record: &Value, outer: &str, key: &str) -> ProjectsResult<Value> {
    match record.get(outer) {
        None => Ok(Value::Null),
        Some(v) if v.is_object() => Ok(v.get(key).cloned().unwrap_or(Value::Null)),
        Some(v) => Err(py_named(
            "AttributeError",
            format!("'{}' object has no attribute 'get'", py_typename(v)),
        )),
    }
}

/// `_rows`: resolve the dataset into rows, expected total and source metadata.
pub(super) fn rows_for(
    directory: &Path,
    dataset: &str,
    compare_manifest: Option<&str>,
) -> ProjectsResult<(Vec<Value>, i64, Value)> {
    if dataset == "tasks" {
        let (rows, revision) = task_rows(directory)?;
        let total = rows.len() as i64;
        return Ok((rows, total, json!({"checklist_revision": revision})));
    }
    if dataset == "task_comments" || dataset == "task_history" {
        return task_related_rows(directory, dataset);
    }
    if dataset != "compare" || compare_manifest.is_none() {
        return Err(export_error(
            "dataset must be tasks, task_comments, task_history, or compare with a compare_manifest",
        ));
    }
    let (root, _) = crate::workspace::load(directory)?;
    let (path, manifest, names) = compare_source(&root, compare_manifest.unwrap_or_default())?;
    let mut total = 0i64;
    for name in &names {
        total += py_int(&manifest["files"][name.as_str()]["rows"])?;
    }
    let mut rows = Vec::new();
    for name in &names {
        for record in compare_rows(&path, name)? {
            let check = match record.get("check") {
                Some(c) if !crate::schedule::falsy(c) => c.clone(),
                _ => nested_field(&record, "issue", "check")?,
            };
            let severity = match record.get("severity") {
                Some(s) if !crate::schedule::falsy(s) => s.clone(),
                _ => nested_field(&record, "issue", "severity")?,
            };
            let target_url = match record.get("target_url") {
                Some(t) if !crate::schedule::falsy(t) => t.clone(),
                _ => nested_field(&record, "issue", "target_url")?,
            };
            rows.push(json!({
                "category": name,
                "check": check,
                "severity": severity,
                "target_url": target_url,
                "row_json": record,
            }));
        }
    }
    let manifest_bytes = std::fs::read(&path).map_err(ProjectsError::Io)?;
    let source = json!({
        "before": manifest["before"].clone(),
        "after": manifest["after"].clone(),
        "compare_manifest_sha256": format!("{:x}", Sha256::digest(&manifest_bytes)),
    });
    Ok((rows, total, source))
}

/// `checkpoint` inside `_write_rows`: heartbeat the job file, honor cancel and the
/// 10 GiB free-space reserve.
pub(super) fn write_checkpoint(
    root: &Path,
    out_dir: &Path,
    job: &mut Value,
    processed: i64,
) -> ProjectsResult<()> {
    job["processed_rows"] = json!(processed);
    job["updated_at"] = json!(zulu());
    write_job(root, job)?;
    check_cancel(root, job)?;
    if free_bytes(out_dir).map_err(ProjectsError::Io)? < FREE_SPACE_RESERVE {
        return Err(export_error(
            "project export stopped to preserve the 10 GiB free-space reserve",
        ));
    }
    Ok(())
}

pub(super) struct RowSink {
    file: Option<std::fs::File>,
    written: u64,
    digest: Sha256,
    workbook: Option<rust_xlsxwriter::Workbook>,
    sheet_index: usize,
    rows_written: i64,
}

/// `_write_rows`: stream the dataset into a temporary artifact and publish it.
pub(super) fn write_rows(
    root: &Path,
    job: &mut Value,
    rows: &[Value],
    total: i64,
    columns: &[&str],
) -> ProjectsResult<Value> {
    let out_dir = root.join("reports");
    if out_dir.is_symlink() || !out_dir.is_dir() {
        return Err(export_error(
            "project reports directory is unavailable or unsafe",
        ));
    }
    let (target, temporary) = artifact_paths(root, job)?;
    if target.parent() != Some(out_dir.as_path()) || target.is_symlink() || lexists(&target) {
        return Err(export_error(
            "export output must be a new file directly under reports/",
        ));
    }
    let format_ = job["format"].as_str().unwrap_or_default().to_string();
    let suffix = match format_.as_str() {
        "json" => ".json",
        "csv" => ".csv",
        _ => ".xlsx",
    };
    let target_suffix = target
        .file_name()
        .map(|n| casefold(&py_suffix(&n.to_string_lossy())))
        .unwrap_or_default();
    if target_suffix != suffix {
        return Err(export_error(&format!("output extension must be {suffix}")));
    }
    if temporary.is_symlink() || lexists(&temporary) {
        return Err(export_error("export temporary path already exists"));
    }
    let digest = Sha256::new();
    let mut sink = RowSink {
        file: None,
        written: 0,
        digest,
        workbook: None,
        sheet_index: 0,
        rows_written: 0,
    };
    let result = write_rows_inner(
        root, &out_dir, &temporary, &mut sink, job, rows, total, columns, &format_,
    );
    if result.is_err() {
        drop(sink.file.take());
        drop(sink.workbook.take());
        let _ = std::fs::remove_file(&temporary);
        return Err(result
            .err()
            .unwrap_or_else(|| export_error("export failed")));
    }
    let (size, processed) = result.unwrap_or((0, 0));
    publish_result(root, job, target, temporary, size, processed, sink.digest)
}

/// Inner body of `_write_rows` so cleanup happens exactly once on any failure.
#[allow(clippy::too_many_arguments)]
pub(super) fn write_rows_inner(
    root: &Path,
    out_dir: &Path,
    temporary: &Path,
    sink: &mut RowSink,
    job: &mut Value,
    rows: &[Value],
    total: i64,
    columns: &[&str],
    format_: &str,
) -> ProjectsResult<(u64, i64)> {
    job["state"] = json!("running");
    job["phase"] = json!("writing");
    job["temporary"] = json!(temporary
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default());
    job["updated_at"] = json!(zulu());
    write_job(root, job)?;
    if format_ == "json" {
        let mut file = open_temporary(temporary)?;
        let prefix = build_json_prefix(job);
        file.write_all(prefix.as_bytes())
            .map_err(ProjectsError::Io)?;
        sink.digest.update(prefix.as_bytes());
        sink.written += prefix.len() as u64;
        sink.file = Some(file);
    } else if format_ == "csv" {
        let file = open_temporary(temporary)?;
        let mut file = file;
        let header = csv_line(&columns.iter().map(|c| c.to_string()).collect::<Vec<_>>());
        file.write_all(&header).map_err(ProjectsError::Io)?;
        sink.written += header.len() as u64;
        sink.file = Some(file);
    } else {
        if total > XLSX_ROW_LIMIT {
            return Err(export_error(
                "XLSX has one header row and a 1,048,575 data-row limit; use complete CSV or JSON",
            ));
        }
        let mut workbook = rust_xlsxwriter::Workbook::new();
        let sheet_name = match job["dataset"].as_str().unwrap_or_default() {
            "tasks" => "Tasks",
            "task_comments" => "Comments",
            "task_history" => "Task history",
            _ => "Compare",
        };
        let sheet = workbook
            .add_worksheet_with_constant_memory()
            .map_err(xlsx_error)?;
        sheet.set_name(sheet_name).map_err(xlsx_error)?;
        for (col, name) in columns.iter().enumerate() {
            sheet
                .write_string(0, col as u16, *name)
                .map_err(xlsx_error)?;
        }
        sink.rows_written = 1;
        let descriptor = open_temporary(temporary)?;
        drop(descriptor);
        sink.workbook = Some(workbook);
        sink.sheet_index = 1;
    }
    let mut processed = 0i64;
    for row in rows {
        processed += 1;
        if processed > MAX_EXPORT_ROWS {
            return Err(export_error(&format!(
                "export exceeds the {MAX_EXPORT_ROWS}-row bound"
            )));
        }
        let values: Vec<String> = columns.iter().map(|c| safe_cell(row.get(*c))).collect();
        if format_ == "xlsx" && values.iter().any(|v| v.chars().count() > XLSX_CELL_LIMIT) {
            return Err(export_error(
                "XLSX cell exceeds 32,767 characters; use complete CSV or JSON",
            ));
        }
        if format_ == "json" {
            let encoded = pyjson::dumps_compact(row);
            let chunk = format!("{}{}", if processed > 1 { "," } else { "" }, encoded);
            let file = sink
                .file
                .as_mut()
                .ok_or_else(|| export_error("export stream is unavailable"))?;
            file.write_all(chunk.as_bytes())
                .map_err(ProjectsError::Io)?;
            sink.digest.update(chunk.as_bytes());
            sink.written += chunk.len() as u64;
        } else if format_ == "csv" {
            let line = csv_line(&values);
            let file = sink
                .file
                .as_mut()
                .ok_or_else(|| export_error("export stream is unavailable"))?;
            file.write_all(&line).map_err(ProjectsError::Io)?;
            sink.written += line.len() as u64;
        } else {
            let workbook = sink
                .workbook
                .as_mut()
                .ok_or_else(|| export_error("export workbook is unavailable"))?;
            let sheet = workbook
                .worksheet_from_index(sink.sheet_index)
                .map_err(xlsx_error)?;
            let row_index = sink.rows_written as u32;
            for (col, cell) in values.iter().enumerate() {
                sheet
                    .write_string(row_index, col as u16, cell)
                    .map_err(xlsx_error)?;
            }
            sink.rows_written += 1;
        }
        if processed % 256 == 0 {
            write_checkpoint(root, out_dir, job, processed)?;
        }
        if sink.file.is_some() && sink.written > MAX_EXPORT_BYTES {
            return Err(export_error("export exceeds its 4 GiB byte bound"));
        }
    }
    if processed != total {
        return Err(export_error("export row count changed during generation"));
    }
    let size: u64;
    if format_ == "json" {
        let file = sink
            .file
            .as_mut()
            .ok_or_else(|| export_error("export stream is unavailable"))?;
        file.write_all(b"]}\n").map_err(ProjectsError::Io)?;
        sink.digest.update(b"]}\n");
        sink.written += 3;
        file.flush().map_err(ProjectsError::Io)?;
        file.sync_all().map_err(ProjectsError::Io)?;
        size = sink.written;
        sink.file = None;
    } else if format_ == "csv" {
        let file = sink
            .file
            .as_mut()
            .ok_or_else(|| export_error("export stream is unavailable"))?;
        file.flush().map_err(ProjectsError::Io)?;
        file.sync_all().map_err(ProjectsError::Io)?;
        size = sink.written;
        sink.file = None;
        let data = std::fs::read(temporary).map_err(ProjectsError::Io)?;
        sink.digest.update(&data);
    } else {
        let mut workbook = sink
            .workbook
            .take()
            .ok_or_else(|| export_error("export workbook is unavailable"))?;
        workbook.save(temporary).map_err(xlsx_error)?;
        size = temporary.metadata().map_err(ProjectsError::Io)?.len();
        let data = std::fs::read(temporary).map_err(ProjectsError::Io)?;
        sink.digest.update(&data);
    }
    if size > MAX_EXPORT_BYTES {
        return Err(export_error("export exceeds its 4 GiB byte bound"));
    }
    Ok((size, processed))
}

pub(super) fn build_json_prefix(job: &Value) -> String {
    let body = pyjson::dumps_compact(&json!({
        "format": "spiderhead.project-export.v1",
        "dataset": job["dataset"],
        "source": job["source"],
    }));
    format!("{},\"rows\":[", &body[..body.len().saturating_sub(1)])
}

pub(super) fn xlsx_error(error: rust_xlsxwriter::XlsxError) -> ProjectsError {
    py_named("XlsxError", error.to_string())
}

/// Tail of `_write_rows`: publish the prepared artifact by hard-link.
pub(super) fn publish_result(
    root: &Path,
    job: &mut Value,
    target: PathBuf,
    temporary: PathBuf,
    size: u64,
    processed: i64,
    digest: Sha256,
) -> ProjectsResult<Value> {
    let relative = target
        .strip_prefix(root)
        .unwrap_or(&target)
        .to_string_lossy()
        .replace('\\', "/");
    let result = json!({
        "path": relative,
        "rows": processed,
        "bytes": size as i64,
        "sha256": format!("{:x}", digest.finalize()),
    });
    job["state"] = json!("running");
    job["phase"] = json!("publishing");
    job["result"] = result.clone();
    job["processed_rows"] = json!(processed);
    job["updated_at"] = json!(zulu());
    write_job(root, job)?;
    check_cancel(root, job)?;
    std::fs::hard_link(&temporary, &target).map_err(ProjectsError::Io)?;
    std::fs::remove_file(&temporary).map_err(ProjectsError::Io)?;
    job["phase"] = json!("history");
    job["updated_at"] = json!(zulu());
    write_job(root, job)?;
    Ok(result)
}

/// `_record_published_export`: verify/publish the artifact and record its receipt.
pub(super) fn record_published_export(
    root: &Path,
    directory: &Path,
    job: &mut Value,
) -> ProjectsResult<Value> {
    let (target, temporary) = artifact_paths(root, job)?;
    let relative = target
        .strip_prefix(root)
        .unwrap_or(&target)
        .to_string_lossy()
        .replace('\\', "/");
    let result = job.get("result").cloned().unwrap_or(Value::Null);
    if !result.is_object() || result["path"].as_str() != Some(relative.as_str()) {
        return Err(export_error(
            "published export has no matching prepared result",
        ));
    }
    if target.is_symlink() {
        return Err(export_error("published export path is a symbolic link"));
    }
    if target.exists() {
        if !matches_artifact(&target, &result) {
            return Err(export_error(
                "published export does not match its prepared checksum",
            ));
        }
    } else {
        if !matches_artifact(&temporary, &result) {
            return Err(export_error(
                "prepared export is unavailable or does not match its checksum",
            ));
        }
        std::fs::hard_link(&temporary, &target).map_err(ProjectsError::Io)?;
    }
    let (temp_exists, temp_symlink) = path_flags(&temporary);
    if temp_exists && !temp_symlink && matches_artifact(&temporary, &result) {
        std::fs::remove_file(&temporary).map_err(ProjectsError::Io)?;
    }
    job["state"] = json!("running");
    job["phase"] = json!("history");
    job["updated_at"] = json!(zulu());
    write_job(root, job)?;
    let receipt = record_export(
        directory,
        job["dataset"].as_str().unwrap_or_default(),
        job["format"].as_str().unwrap_or_default(),
        result["path"].as_str().unwrap_or_default(),
        result["rows"].as_i64().unwrap_or(0),
        result["bytes"].as_i64().unwrap_or(0),
        result["sha256"].as_str(),
        job.get("source").filter(|s| !crate::schedule::falsy(s)),
        job["job_id"].as_str(),
    );
    match receipt {
        Ok(receipt) => {
            job["state"] = json!("completed");
            job["phase"] = json!("complete");
            job["history_state"] = json!("recorded");
            job["history_reason"] = json!("");
            job["export"] = receipt;
            job["finished_at"] = json!(zulu());
        }
        Err(exc) => {
            let (name, _) = py_exception_name(&exc);
            job["state"] = json!("completed");
            job["phase"] = json!("complete");
            job["history_state"] = json!("failed");
            job["history_reason"] = json!(format!(
                "history receipt could not be recorded ({name}); retry this job_id"
            ));
            job["finished_at"] = json!(zulu());
        }
    }
    write_job(root, job)?;
    Ok(job.clone())
}

/// `record_export`: append one success receipt; a job ID makes retries idempotent.
#[allow(clippy::too_many_arguments)]
pub fn record_export(
    directory: &Path,
    dataset: &str,
    fmt: &str,
    path: &str,
    row_count: i64,
    byte_count: i64,
    sha256: Option<&str>,
    source: Option<&Value>,
    job_id: Option<&str>,
) -> ProjectsResult<Value> {
    let (root, project) = crate::workspace::load(directory)?;
    let default_document = || {
        json!({
            "format": HISTORY_FORMAT,
            "project_uuid": project["project_uuid"],
            "revision": 0,
            "items": [],
        })
    };
    let mut document =
        crate::runtime::read_document(&root, HISTORY_NAME)?.unwrap_or_else(default_document);
    let validate_document = |document: &Value| -> ProjectsResult<()> {
        if document["format"].as_str() != Some(HISTORY_FORMAT)
            || document.get("project_uuid") != Some(&project["project_uuid"])
        {
            return Err(export_error(
                "export history belongs to another project or format",
            ));
        }
        let items = document.get("items");
        if !items.is_some_and(|i| i.is_array())
            || !document
                .get("revision")
                .is_some_and(|r| r.is_i64() || r.is_u64())
            || document["revision"].as_i64().is_some_and(|r| r < 0)
        {
            return Err(export_error("export history items are malformed"));
        }
        for row in items.and_then(|i| i.as_array()).into_iter().flatten() {
            if !row.is_object() {
                return Err(export_error("export history items are malformed"));
            }
            validate_export(row)?;
        }
        Ok(())
    };
    validate_document(&document)?;
    let mut item = json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "recorded_at": zulu(),
        "dataset": dataset,
        "format": fmt,
        "path": path,
        "row_count": row_count,
        "byte_count": byte_count,
        "sha256": sha256.map(|s| json!(s)).unwrap_or(Value::Null),
        "source": source.cloned().unwrap_or_else(|| json!({})),
    });
    if let Some(job_id) = job_id {
        let normalized = uuid::Uuid::parse_str(job_id)
            .map(|u| u.to_string())
            .map_err(|_| export_error("invalid export history item"))?;
        item["job_id"] = json!(normalized);
    }
    validate_export(&item)?;
    for _attempt in 0..8 {
        let items = document["items"].as_array().cloned().unwrap_or_default();
        if let Some(job_id_value) = item.get("job_id") {
            let existing = items
                .iter()
                .find(|row| row.is_object() && row.get("job_id") == Some(job_id_value));
            if let Some(existing) = existing {
                let comparable = [
                    "dataset",
                    "format",
                    "path",
                    "row_count",
                    "byte_count",
                    "sha256",
                    "source",
                ];
                if comparable.iter().all(|key| {
                    existing.get(*key).unwrap_or(&Value::Null)
                        == item.get(*key).unwrap_or(&Value::Null)
                }) {
                    return Ok(existing.clone());
                }
                return Err(export_error(
                    "export job receipt conflicts with its published result",
                ));
            }
        }
        let mut new_items: Vec<Value> = items;
        new_items.push(item.clone());
        if new_items.len() > 10_000 {
            new_items = new_items[new_items.len() - 10_000..].to_vec();
        }
        let mut updated = document.clone();
        updated["revision"] = json!(document["revision"].as_i64().unwrap_or(0) + 1);
        updated["items"] = Value::Array(new_items);
        match crate::runtime::write_document(
            &root,
            HISTORY_NAME,
            &updated,
            document["revision"].as_i64(),
        ) {
            Ok(()) => break,
            Err(e) => {
                let is_conflict = e.to_string().contains("revision conflict");
                if !is_conflict {
                    return Err(e);
                }
                document = crate::runtime::read_document(&root, HISTORY_NAME)?
                    .unwrap_or_else(default_document);
                if document["format"].as_str() != Some(HISTORY_FORMAT)
                    || document.get("project_uuid") != Some(&project["project_uuid"])
                {
                    return Err(export_error(
                        "export history belongs to another project or format",
                    ));
                }
                let items = document.get("items");
                if !items.is_some_and(|i| i.is_array())
                    || !document
                        .get("revision")
                        .is_some_and(|r| r.is_i64() || r.is_u64())
                    || document["revision"].as_i64().is_some_and(|r| r < 0)
                {
                    return Err(export_error("export history is malformed"));
                }
                for row in items.and_then(|i| i.as_array()).into_iter().flatten() {
                    if !row.is_object() {
                        return Err(export_error("export history items are malformed"));
                    }
                    validate_export(row)?;
                }
            }
        }
        if _attempt == 7 {
            return Err(export_error(
                "export history remained busy after bounded retries",
            ));
        }
    }
    Ok(item)
}

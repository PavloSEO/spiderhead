//! `import_run`: legacy directory to scan.v1 artifact.

use super::*;

pub(crate) fn utc_now_iso() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64;
    let micros = now.subsec_micros();
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let fraction = if micros == 0 {
        String::new()
    } else {
        format!(".{micros:06}")
    };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}{fraction}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

pub(crate) fn sync_directory(directory: &Path) {
    if let Ok(handle) = File::open(directory) {
        let _ = handle.sync_all();
    }
}

/// Import legacy observations without changing their producer or audit bytes.
pub fn import_run(
    source: &Path,
    out: &Path,
    producer_build: &str,
    effective_config: Option<&Value>,
) -> Result<PathBuf, StoreError> {
    runtime()?;
    if producer_build.len() != 40
        || !producer_build
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(bad(
            "producer_build must identify the original crawl with a full lowercase Git commit SHA",
        ));
    }
    let out = std::path::absolute(out)?;
    let exists_message = format!(
        "output already exists: {}; choose a new --out path; imports never overwrite scans",
        out.display()
    );
    if fs::symlink_metadata(&out).is_ok() {
        return Err(bad(exists_message));
    }
    let parent = out
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = parent.join(format!(
        ".scan-import-{}-{stamp}.sqlite",
        std::process::id()
    ));
    let result = import_run_inner(
        source,
        &out,
        &temporary,
        producer_build,
        effective_config,
        &exists_message,
    );
    let _ = fs::remove_file(&temporary);
    result
}

fn import_run_inner(
    source: &Path,
    out: &Path,
    temporary: &Path,
    producer_build: &str,
    effective_config: Option<&Value>,
    exists_message: &str,
) -> Result<PathBuf, StoreError> {
    let audit_text = text(&source.join("audit.json"))?;
    let audit_doc = audit(&audit_text).map_err(import_failed)?;
    let recorded = audit_doc["run"]["crawl_config"].clone();
    if let Some(explicit) = effective_config {
        if !recorded.is_null() && dump(explicit) != dump(&recorded) {
            return Err(import_failed(bad(
                "explicit configuration differs from the original run.crawl_config",
            )));
        }
    }
    let config = config(effective_config.unwrap_or(&recorded)).map_err(import_failed)?;
    let producer_version = audit_doc["tool"]["version"]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| import_failed(bad("original audit.tool.version is required")))?
        .to_owned();
    let mut limitations =
        vec!["legacy import retains no response bodies or resumable crawl context".to_owned()];
    File::create_new(temporary).map_err(|e| import_failed(e.into()))?;
    let con =
        open_writer(temporary, Duration::from_secs(5), &[]).map_err(|e| import_failed(e.into()))?;
    con.execute_batch(schema())
        .map_err(|e| import_failed(e.into()))?;
    con.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA synchronous=FULL; BEGIN")
        .map_err(|e| import_failed(e.into()))?;
    let mut inputs: Vec<Map<String, Value>> = Vec::new();
    import_pages(&con, source, &mut limitations, &mut inputs).map_err(import_failed)?;
    import_links(&con, source, &mut limitations, &mut inputs).map_err(import_failed)?;
    let mut audit_input = Map::new();
    audit_input.insert("name".into(), Value::from("audit.json"));
    audit_input.insert("sha256".into(), Value::String(sha(&audit_text)));
    audit_input.insert("bytes".into(), Value::from(audit_text.len() as i64));
    inputs.push(audit_input);

    let partial =
        audit_doc["run"]["crawl_partial"].as_bool().unwrap_or(false) || recovered(&limitations);
    let missing_fields = legacy_fields_missing(&con).map_err(import_failed)?;
    if !missing_fields.is_empty() {
        limitations.push(format!(
            "legacy page fields unavailable: {}",
            missing_fields.join(", ")
        ));
    }
    let mut capabilities = Map::new();
    for key in CAPABILITIES {
        capabilities.insert(
            (*key).to_owned(),
            serde_json::json!({
                "state": "unavailable",
                "reason": "point A imports no retained bodies or resumable context",
            }),
        );
    }
    for key in ["pages", "links"] {
        capabilities.insert(
            key.to_owned(),
            serde_json::json!({
                "state": if partial { "partial" } else { "complete" },
                "reason": if partial {
                    "legacy source is partial"
                } else {
                    "imported observations; original crawl scope applies"
                },
            }),
        );
    }
    if !missing_fields.is_empty() {
        let fields_reason = format!(
            "legacy page fields unavailable: {}",
            missing_fields.join(", ")
        );
        let pages_reason = if partial {
            format!(
                "{}; {fields_reason}",
                capabilities["pages"]["reason"].as_str().unwrap_or_default()
            )
        } else {
            fields_reason
        };
        capabilities.insert(
            "pages".into(),
            serde_json::json!({ "state": "partial", "reason": pages_reason }),
        );
    }
    let now = utc_now_iso();
    let limitations_value = Value::Array(limitations.iter().cloned().map(Value::String).collect());
    let config_text = dump(&config);
    let mut runtime = Map::new();
    for key in ["python", "sqlite", "httpx", "lxml", "beautifulsoup4"] {
        runtime.insert(key.into(), Value::from("unknown"));
    }
    let mut scan_row = Map::new();
    scan_row.insert("singleton".into(), Value::from(1));
    scan_row.insert(
        "scan_uuid".into(),
        Value::String(uuid::Uuid::new_v4().to_string()),
    );
    scan_row.insert("format_version".into(), Value::from(FORMAT_VERSION));
    scan_row.insert("evidence_version".into(), Value::from("crawl.v1"));
    scan_row.insert(
        "writer_version".into(),
        Value::String(producer_version.clone()),
    );
    scan_row.insert("writer_revision".into(), Value::from(producer_build));
    scan_row.insert(
        "runtime_versions_json".into(),
        Value::String(dump(&Value::Object(runtime))),
    );
    scan_row.insert("created_at".into(), Value::String(now.clone()));
    scan_row.insert("finished_at".into(), Value::String(now.clone()));
    scan_row.insert("source_kind".into(), Value::from("legacy_import"));
    scan_row.insert("start_url".into(), audit_doc["run"]["source"].clone());
    scan_row.insert("config_json".into(), Value::String(config_text.clone()));
    scan_row.insert(
        "config_fingerprint".into(),
        Value::String(sha(&config_text)[..16].to_owned()),
    );
    scan_row.insert("lifecycle".into(), Value::from("finished"));
    scan_row.insert("finish_reason".into(), Value::from("legacy_import"));
    scan_row.insert("crawl_partial".into(), Value::from(i64::from(partial)));
    scan_row.insert("corpus_partial".into(), Value::from(1));
    scan_row.insert("evidence_revision".into(), Value::from(1));
    scan_row.insert(
        "limitations_json".into(),
        Value::String(dump(&limitations_value)),
    );
    scan_row.insert(
        "capabilities_json".into(),
        Value::String(dump(&Value::Object(capabilities))),
    );
    scan_row.insert(
        "retention_json".into(),
        Value::String(dump(&serde_json::json!({
            "policy_version": "scan_retention.v1",
            "body_mode": "off",
            "max_body_bytes": 0,
            "max_body_store_bytes": 0,
            "min_free_bytes": 0,
            "history_warning_bytes": 0,
            "automatic_delete": false,
        }))),
    );
    insert(&con, "scan", &scan_row).map_err(import_failed)?;

    let mut audit_row = Map::new();
    audit_row.insert("singleton".into(), Value::from(1));
    audit_row.insert("schema_version".into(), audit_doc["schema_version"].clone());
    audit_row.insert("evidence_revision".into(), Value::from(1));
    audit_row.insert(
        "analyzer_version".into(),
        Value::String(producer_version.clone()),
    );
    audit_row.insert("analyzer_revision".into(), Value::from(producer_build));
    audit_row.insert(
        "created_at".into(),
        audit_doc["run"]["generated_at"].clone(),
    );
    audit_row.insert("sha256".into(), Value::String(sha(&audit_text)));
    audit_row.insert("document_json".into(), Value::String(audit_text.clone()));
    insert(&con, "audit", &audit_row).map_err(import_failed)?;

    let mut provenance = Map::new();
    provenance.insert("source_format".into(), Value::from("legacy_directory.v1"));
    provenance.insert(
        "inputs".into(),
        Value::Array(inputs.into_iter().map(Value::Object).collect()),
    );
    provenance.insert(
        "recovered_truncated_final_line".into(),
        Value::Bool(recovered(&limitations)),
    );
    provenance.insert("resume_eligible".into(), Value::Bool(false));
    let mut context = Map::new();
    context.insert("kind".into(), Value::from("legacy_import_provenance"));
    context.insert("item_key".into(), Value::from("run"));
    context.insert("payload_version".into(), Value::from("scan_context.v1"));
    context.insert(
        "payload_json".into(),
        Value::String(dump(&Value::Object(provenance))),
    );
    context.insert(
        "completeness".into(),
        Value::from(if partial { "partial" } else { "complete" }),
    );
    context.insert("reason".into(), Value::from("resume unavailable"));
    insert(&con, "context_items", &context).map_err(import_failed)?;
    con.execute_batch("COMMIT")
        .map_err(|e| import_failed(e.into()))?;
    drop(con);

    open_scan(temporary, true, None)?;
    File::options()
        .write(true)
        .open(temporary)
        .and_then(|f| f.sync_all())
        .map_err(|e| import_failed(e.into()))?;
    match fs::hard_link(temporary, out) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(bad(exists_message));
        }
        Err(e) => return Err(import_failed(e.into())),
    }
    if let Some(parent) = out.parent() {
        sync_directory(parent);
    }
    Ok(out.to_path_buf())
}

/// Wrap a failure of `import_run` with the message prefix `cannot import run: `.
pub(super) fn import_failed(error: StoreError) -> StoreError {
    bad(format!("cannot import run: {error}"))
}

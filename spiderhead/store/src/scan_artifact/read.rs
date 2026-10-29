//! Read-only access: full validation, the light open and the audit reader.

use super::*;
use crate::audit_v2::AuditV2Reader;

/// Open and validate the audit.v2 companion, then close it.
fn check_companion(path: &Path) -> Result<(), StoreError> {
    let reader = AuditV2Reader::new(path, true, false, None)?;
    reader.close();
    Ok(())
}

/// Full validation of a scan.v1 (legacy import) or delegate native scans to the native validator.
pub fn validate(con: &Connection, require_audit: bool) -> Result<(), StoreError> {
    if objects(con)? != expected()?.objects {
        return Err(bad(
            "scan.v1 schema differs: missing/changed tables, indexes or unexpected schema objects",
        ));
    }
    let source: Option<String> = con
        .query_row("SELECT source_kind FROM scan WHERE singleton=1", [], |r| {
            r.get(0)
        })
        .optional()?;
    if matches!(source.as_deref(), Some("native") | Some("reanalysis")) {
        NativeScanSchemaMixin::validate_native(con)?;
        let has_audit = con
            .prepare("SELECT 1 FROM audit WHERE singleton=1")?
            .exists([])?;
        if require_audit && !has_audit {
            let database: String = con.query_row("PRAGMA database_list", [], |r| r.get(2))?;
            if database.is_empty() || !audit_v2_path(Path::new(&database)).exists() {
                return Err(bad(
                    "native scan has no current audit; collection evidence is available separately",
                ));
            }
            check_companion(Path::new(&database))?;
        }
        return Ok(());
    }
    for table in FUTURE_TABLES {
        if con
            .prepare(&format!("SELECT 1 FROM \"{table}\" LIMIT 1"))?
            .exists([])?
        {
            return Err(bad(format!(
                "{table}: this reader supports point-A legacy imports only; capability unavailable"
            )));
        }
    }
    validate_scalar_storage(con)?;
    let quick: String = con.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    if quick != "ok" {
        return Err(bad("scan database failed quick_check"));
    }
    if con.prepare("PRAGMA foreign_key_check")?.exists([])? {
        return Err(bad("scan database has inconsistent foreign-key references"));
    }
    let run_count: i64 = con.query_row("SELECT COUNT(*) FROM scan", [], |r| r.get(0))?;
    if run_count != 1 {
        return Err(bad("scan.v1 requires exactly one run header"));
    }
    let scan = query_maps(con, "SELECT * FROM scan WHERE singleton=1")?
        .into_iter()
        .next()
        .ok_or_else(|| bad("scan.v1 requires exactly one run header"))?;
    if field_str(&scan, "format_version")? != FORMAT_VERSION
        || field_str(&scan, "source_kind")? != "legacy_import"
    {
        return Err(bad(
            "unsupported scan format or source_kind (point-A legacy imports only)",
        ));
    }
    let revision = field_str(&scan, "writer_revision")?;
    if revision.len() != 40
        || !revision
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(bad(
            "scan producer build must be a full lowercase Git commit SHA",
        ));
    }
    let config = config(&loads(
        field_str(&scan, "config_json")?,
        "scan.config_json",
    )?)?;
    if field_str(&scan, "config_fingerprint")? != &sha(&dump(&config))[..16] {
        return Err(bad("effective configuration fingerprint does not match"));
    }
    let capabilities = loads(
        field_str(&scan, "capabilities_json")?,
        "scan.capabilities_json",
    )?;
    let cap_map = capabilities
        .as_object()
        .filter(|o| {
            o.len() == CAPABILITIES.len() && CAPABILITIES.iter().all(|c| o.contains_key(*c))
        })
        .ok_or_else(|| bad("invalid scan capability map"))?;
    for (name, state) in cap_map {
        let ok = state.as_object().is_some_and(|s| {
            s.len() == 2
                && s.get("reason").is_some_and(Value::is_string)
                && s.get("state")
                    .and_then(Value::as_str)
                    .is_some_and(|v| matches!(v, "complete" | "partial" | "unavailable"))
        });
        if !ok {
            return Err(bad(format!("invalid capability: {name}")));
        }
        let st = state
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if name != "pages" && name != "links" && st != "unavailable" {
            return Err(bad(format!(
                "unsupported capability must be unavailable: {name}"
            )));
        }
    }
    let audit_count: i64 = con.query_row("SELECT COUNT(*) FROM audit", [], |r| r.get(0))?;
    let audit_row = query_maps(con, "SELECT * FROM audit WHERE singleton=1")?
        .into_iter()
        .next();
    let Some(audit_row) = audit_row.filter(|_| audit_count == 1) else {
        return Err(bad("scan.v1 requires exactly one saved audit"));
    };
    let document_json = field_str(&audit_row, "document_json")?;
    if document_json.len() as i64 > MAX_JSON_BYTES {
        return Err(bad("saved audit exceeds the 64 MiB JSON limit"));
    }
    if field_str(&audit_row, "sha256")? != sha(document_json) {
        return Err(bad("saved audit hash does not match its document"));
    }
    let audit_doc = audit(document_json)?;
    if field_str(&audit_row, "schema_version")?
        != audit_doc["schema_version"].as_str().unwrap_or_default()
        || field_int(&audit_row, "evidence_revision")? != field_int(&scan, "evidence_revision")?
    {
        return Err(bad("saved audit version/revision does not match the scan"));
    }
    let tool_version = audit_doc["tool"]["version"].as_str().unwrap_or_default();
    if field_str(&audit_row, "analyzer_revision")? != revision
        || field_str(&audit_row, "analyzer_version")? != field_str(&scan, "writer_version")?
        || field_str(&scan, "writer_version")? != tool_version
    {
        return Err(bad("saved audit and scan producer identities disagree"));
    }
    let original_config = audit_doc["run"]["crawl_config"].clone();
    if !original_config.is_null() && dump(&original_config) != dump(&config) {
        return Err(bad(
            "saved audit and scan effective configurations disagree",
        ));
    }
    let audit_urls: Vec<&str> = audit_doc["pages"]
        .as_array()
        .map(|pages| pages.iter().filter_map(|p| p["url"].as_str()).collect())
        .unwrap_or_default();
    let unique: BTreeSet<&str> = audit_urls.iter().copied().collect();
    if unique.len() != audit_urls.len() {
        return Err(bad("saved audit contains duplicate page URLs"));
    }
    let mut stored_stmt = con.prepare("SELECT url FROM urls JOIN pages USING(url_id)")?;
    let stored: BTreeSet<String> = stored_stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    let audit_set: BTreeSet<String> = audit_urls.iter().map(|s| (*s).to_owned()).collect();
    if stored != audit_set {
        return Err(bad("pages.jsonl and audit page populations disagree"));
    }
    for page in query_maps(
        con,
        "SELECT p.*, u.url FROM pages p JOIN urls u USING(url_id)",
    )? {
        for key in PAGE_BOOLS {
            match page.get(*key) {
                None | Some(Value::Null) => {}
                Some(Value::Number(n)) if matches!(n.as_i64(), Some(0) | Some(1)) => {}
                _ => return Err(bad(format!("invalid page boolean: {key}"))),
            }
        }
        if let Some(text) = page.get("hreflang_json").and_then(Value::as_str) {
            hreflang(&loads(text, "hreflang_json")?)?;
        }
        let chain = loads(field_str(&page, "redirect_chain_json")?, "redirect_chain")?;
        if !chain.is_array() {
            return Err(bad("invalid redirect_chain"));
        }
    }
    for link in query_maps(
        con,
        "SELECT rel_json, source_document_id, evidence_representation FROM links",
    )? {
        let rel = loads(field_str(&link, "rel_json")?, "links.rel_json")?;
        if !rel
            .as_array()
            .is_some_and(|tokens| tokens.iter().all(Value::is_string))
        {
            return Err(bad("links.rel_json must contain string tokens"));
        }
        if !link.get("source_document_id").is_none_or(Value::is_null)
            || field_str(&link, "evidence_representation")? != "legacy_unknown"
        {
            return Err(bad(
                "imported link document provenance must remain unavailable",
            ));
        }
    }
    validate_import_metadata(con, &scan, &audit_doc)
}

/// Bound a scan's open-and-validate pass by artifact size, not a flat 30 s.
///
/// See READ_TIMEOUT_BYTES_PER_SECOND for the measured rate this derives from.
/// Callers that keep the connection for further reads re-budget per statement
/// (``scan_corpus``) rather than letting this one deadline span them.
pub fn read_deadline_seconds(path: &Path) -> Result<f64, StoreError> {
    let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    Ok((READ_TIMEOUT_SECONDS as f64).max(size as f64 / READ_TIMEOUT_BYTES_PER_SECOND as f64))
}

/// Maximum number of fully validated artifacts remembered per process.
pub const FULL_VALIDATED_LIMIT: i64 = 64;

/// Identity of a file on disk: (device, inode, size, mtime in ns).
pub type FileState = (i64, i64, i64, i64);

/// Key of a fully validated artifact: resolved path, file and WAL state, header digest.
pub type ValidatedKey = (String, Option<FileState>, Option<FileState>, String);

// Insertion-ordered LRU of fully validated artifacts, guarded by a mutex.
static FULL_VALIDATED: Mutex<Vec<ValidatedKey>> = Mutex::new(Vec::new());

/// Device, inode, size and mtime of a file; `None` when it cannot be stat'ed.
pub fn file_state(path: &Path) -> Option<FileState> {
    let info = fs::metadata(path).ok()?;
    let id = path_identity(path).ok()?;
    Some((
        id.0 as i64,
        id.1 as i64,
        info.size() as i64,
        info.mtime() * 1_000_000_000 + info.mtime_nsec(),
    ))
}

/// Identify validated bytes: file and WAL identity/size/mtime plus the scan header.
pub fn validated_key(path: &Path, con: &Connection) -> Result<ValidatedKey, StoreError> {
    let resolved = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let header = query_maps(con, "SELECT * FROM scan WHERE singleton=1")?
        .into_iter()
        .next()
        .map(|m| dump(&Value::Object(m)))
        .unwrap_or_default();
    let mut wal_name = resolved
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    wal_name.push("-wal");
    let wal = resolved.with_file_name(wal_name);
    Ok((
        resolved.to_string_lossy().into_owned(),
        file_state(&resolved),
        file_state(&wal),
        sha(&header),
    ))
}

/// Drop every remembered full validation: the cache is per-process by design,
/// and a test that emulates separate CLI invocations needs that boundary.
pub fn forget_validated() {
    if let Ok(mut cache) = FULL_VALIDATED.lock() {
        cache.clear();
    }
}

/// Remember a fully validated artifact, dropping the oldest beyond the limit.
pub fn remember_validated(key: &ValidatedKey) {
    if let Ok(mut cache) = FULL_VALIDATED.lock() {
        cache.retain(|k| k != key);
        cache.push(key.clone());
        let overflow = cache.len().saturating_sub(FULL_VALIDATED_LIMIT as usize);
        cache.drain(..overflow);
    }
}

/// Return a fully validated read-only connection; the caller must close it.
///
/// Optional query budgets activate only after full artifact validation. They
/// measure each SQLite execute/fetch operation, excluding caller processing
/// between streamed rows; validation keeps its existing artifact-size budget.
/// The budget is not enforced yet; the argument is accepted so callers can pass it already.
pub fn open_scan(
    path: &Path,
    require_audit: bool,
    query_timeout_seconds: Option<f64>,
) -> Result<Connection, StoreError> {
    open_scan_mode(path, require_audit, query_timeout_seconds, false).map(|(con, _)| con)
}

/// Open a scan and say how it was accepted: ``(connection, "light" | "full")``.
///
/// ``light=true`` (native scans, no audit required) checks identity, schema, version and
/// the header row only -- no O(file) or O(pages) pass. If the same bytes (path, size,
/// mtime, WAL state, header hash) were already fully validated in this process, that
/// result is reused and reported as ``"full"``. Anything else that cannot be accepted
/// lightly raises the same ``ScanError`` as before; it is never silently passed.
pub fn open_scan_mode(
    path: &Path,
    require_audit: bool,
    query_timeout_seconds: Option<f64>,
    light: bool,
) -> Result<(Connection, &'static str), StoreError> {
    let _ = query_timeout_seconds;
    runtime()?;
    open_scan_inner(path, require_audit, light)
        .map_err(|e| bad(format!("cannot read scan: {}", open_cause(&e))))
}

/// rusqlite appends `": <path>"` to `CannotOpen` failures where the C library
/// reports only `sqlite3_errmsg`; the diagnostics keep the errmsg text alone.
fn sqlite_open_error(error: rusqlite::Error, path: &Path) -> StoreError {
    match error {
        rusqlite::Error::SqliteFailure(inner, Some(message))
            if inner.code == rusqlite::ffi::ErrorCode::CannotOpen =>
        {
            let base = message
                .strip_suffix(&format!(": {}", path.to_string_lossy()))
                .unwrap_or(&message);
            StoreError::Sqlite(rusqlite::Error::SqliteFailure(inner, Some(base.to_owned())))
        }
        other => other.into(),
    }
}

/// The `str(exc)` wording a caller sees inside `cannot read scan: ...`: the
/// bare SQLite/driver message without this crate's error-variant prefixes.
fn open_cause(error: &StoreError) -> String {
    match error {
        StoreError::Sqlite(inner) => inner.to_string(),
        StoreError::Io(inner) => spiderhead_core::common::python_os_error(inner, None),
        StoreError::Json(inner) => inner.to_string(),
        StoreError::Invalid(message) => message.clone(),
        other => other.to_string(),
    }
}

fn open_scan_inner(
    path: &Path,
    require_audit: bool,
    light: bool,
) -> Result<(Connection, &'static str), StoreError> {
    let resolved = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let con = open_readonly(
        &resolved,
        Duration::from_secs(5),
        &[
            "PRAGMA trusted_schema=OFF",
            "PRAGMA query_only=ON",
            "PRAGMA foreign_keys=ON",
            "PRAGMA cache_size=-8192",
            "PRAGMA temp_store=FILE",
        ],
    )
    .map_err(|e| sqlite_open_error(e, &resolved))?;
    let before = file_state(&resolved);
    let deadline = Instant::now() + Duration::from_secs_f64(read_deadline_seconds(path)?);
    con.progress_handler(10_000, Some(move || Instant::now() > deadline));
    con.execute_batch("BEGIN")?;
    let version: i64 = con.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let app_id: i64 = con.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    if version != USER_VERSION && version != 2 {
        return Err(bad(format!(
            "unsupported scan user_version {version}; no automatic migration"
        )));
    }
    if app_id != APPLICATION_ID {
        return Err(bad(format!(
            "foreign application_id {app_id}; expected {APPLICATION_ID} (SEOH)"
        )));
    }
    let mut mode = "full";
    let source_kind: Option<String> = con
        .query_row("SELECT source_kind FROM scan WHERE singleton=1", [], |r| {
            r.get(0)
        })
        .optional()?;
    let native_kind = matches!(source_kind.as_deref(), Some("native") | Some("reanalysis"));
    if light && !require_audit && native_kind {
        let key = validated_key(&resolved, &con)?;
        let already = FULL_VALIDATED
            .lock()
            .map(|cache| cache.contains(&key))
            .unwrap_or(false);
        if !already {
            NativeScanSchemaMixin::validate_light(&con)?;
            mode = "light";
        }
    } else if version == 2 {
        NativeScanSchemaMixin::validate_native(&con)?;
        let has_audit = con
            .prepare("SELECT 1 FROM audit WHERE singleton=1")?
            .exists([])?;
        if require_audit && !has_audit {
            if !audit_v2_path(&resolved).exists() {
                return Err(bad("scan.v2 has no current audit"));
            }
            check_companion(&resolved)?;
        }
    } else {
        validate(&con, require_audit)?;
    }
    if mode == "full" && before == file_state(&resolved) {
        remember_validated(&validated_key(&resolved, &con)?);
    }
    Ok((con, mode))
}

/// Read a complete compatibility document under the 64 MiB legacy limit.
pub fn read_audit(path: &Path) -> Result<Value, StoreError> {
    if audit_v2_path(path).exists() {
        let reader = AuditV2Reader::new(path, true, false, None)?;
        let document = reader.materialize_legacy(MAX_JSON_BYTES);
        reader.close();
        return document;
    }
    // Learn cheaply that a native scan carries no audit, instead of validating the
    // whole artifact first only to refuse it with the same message.
    let (probe, mode) = open_scan_mode(path, false, None, true)?;
    if mode == "light"
        && !probe
            .prepare("SELECT 1 FROM audit WHERE singleton=1")?
            .exists([])?
    {
        let v2 = probe.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? == 2;
        return Err(bad(if v2 {
            "scan.v2 has no current audit"
        } else {
            "native scan has no current audit; collection evidence is available separately"
        }));
    }
    drop(probe);
    let con = open_scan(path, true, None)?;
    let text: String = con.query_row(
        "SELECT document_json FROM audit WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    loads(&text, "audit")
}

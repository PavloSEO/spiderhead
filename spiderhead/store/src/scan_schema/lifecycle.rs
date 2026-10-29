//! Creating and reopening native scan artifacts (`NativeScanSchemaMixin::create` / `open`).

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Map, Value};

use super::deps::deps;
use super::native_config::{crawl_fingerprint, native_config, resume_fingerprint};
use super::{
    bad, config_extension_context, config_extension_values, frozen_header_config,
    legacy_anonymous_session, read_effective_config, NativeScanSchemaMixin, RUNTIME_KEYS,
};
use crate::error::StoreError;
use crate::native_scan::NativeScan;
use crate::retention::{policy_for_config, validate_policy};
use crate::scan_artifact::{
    dump, insert, open_scan, runtime, schema, sync_directory, utc_now_iso, FORMAT_VERSION,
    MAX_RECORD_BYTES,
};
use spiderhead_core::filesystem::{file_identity, link_identity, PrivateMode};

/// Message of a store error, as shown to the user.
pub(crate) fn message_of(error: &StoreError) -> String {
    match error {
        StoreError::Invalid(m) => m.clone(),
        StoreError::Sqlite(e) => e.to_string(),
        StoreError::Io(e) => e.to_string(),
        StoreError::Json(e) => e.to_string(),
        other => other.to_string(),
    }
}

/// An exclusive advisory lock on `<scan>.writer.lock`, held for the writer's lifetime.
///
/// The lock file is a regular,
/// unaliased file (never a symlink) and SQLite keeps its own byte-range locks on the database.
pub struct WriterLock {
    file: File,
}

impl WriterLock {
    /// Open (creating) the lock file and take the lock without waiting.
    pub fn acquire(lock_path: &Path) -> Result<Self, StoreError> {
        let file = open_lock(lock_path)
            .map_err(|e| bad(format!("cannot acquire native scan writer lock: {e}")))?;
        let meta = file.metadata()?;
        let unaliased = file_identity(&file).map(|i| i.2 == 1).unwrap_or(false);
        if !meta.is_file() || !unaliased {
            return Err(bad(
                "native scan writer lock must be a regular unaliased file",
            ));
        }
        if file.try_lock().is_err() {
            return Err(bad("native scan already has an active writer"));
        }
        Ok(Self { file })
    }

    /// Hand the descriptor (and so the lock) to the `NativeScan` that now owns it.
    /// Hand the locked file to the scan that now owns the writer.
    pub fn into_file(self) -> File {
        self.file
    }
}

/// Open the lock file, refusing symlinks and aliased files.
fn open_lock(path: &Path) -> io::Result<File> {
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(io::Error::other("writer lock path must not be a symlink"));
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .private_mode(0o600);
    // Open the link itself instead of its target: O_NOFOLLOW on Unix,
    // FILE_FLAG_OPEN_REPARSE_POINT on Windows.
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    #[cfg(windows)]
    options.custom_flags(0x0020_0000);
    let file = options.open(path)?;
    let opened = file.metadata()?;
    let opened_id = file_identity(&file)?;
    let named_id = link_identity(path)?;
    if !opened.is_file()
        || opened_id.2 != 1
        || (opened_id.0, opened_id.1) != (named_id.0, named_id.1)
    {
        return Err(io::Error::other(
            "writer lock must be a regular unaliased file",
        ));
    }
    Ok(file)
}

fn fsync_file(path: &Path) -> Result<(), StoreError> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?
        .sync_all()?;
    Ok(())
}

/// Bounded items for the selected sitemap roots: item count and total JSON size.
fn bounded_sitemaps(
    items: &[(String, String)],
    label: &str,
    limit: usize,
) -> Result<Vec<(String, String)>, StoreError> {
    let mut size: i64 = 0;
    for (count, (url, source)) in items.iter().take(limit + 1).enumerate() {
        size += dump(&json!([url, source])).len() as i64;
        if count == limit || size > MAX_RECORD_BYTES {
            return Err(bad(format!(
                "{label} exceeds the bounded per-page limit ({limit} items / 8 MiB)"
            )));
        }
    }
    Ok(items.to_vec())
}

/// A fresh file name in `dir`, created exclusively with mode 0o600.
fn make_temporary(dir: &Path) -> io::Result<PathBuf> {
    for _ in 0..100 {
        let name = format!(".native-scan-{}.sqlite", uuid::Uuid::new_v4().simple());
        let candidate = dir.join(name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .private_mode(0o600)
            .open(&candidate)
        {
            Ok(_) => return Ok(candidate),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other("no usable temporary name"))
}

fn is_revision(text: &str) -> bool {
    text.len() == 40
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn object_at<'a>(config: &'a Map<String, Value>, path: &[&str]) -> Result<&'a Value, StoreError> {
    let mut node = config.get(path[0]);
    for key in &path[1..] {
        node = node.and_then(|n| n.get(*key));
    }
    node.ok_or_else(|| bad(format!("native config is missing {}", path.join("."))))
}

fn capabilities() -> Value {
    json!({
        "pages": {"state": "partial", "reason": "native writer has no collector adapter yet"},
        "links": {"state": "partial", "reason": "native writer has no collector adapter yet"},
        "responses": {"state": "unavailable", "reason": "no response observations yet"},
        "html_bodies": {"state": "unavailable", "reason": "no retained HTML bodies yet"},
        "rendered_bodies": {"state": "unavailable", "reason": "no retained rendered DOM yet"},
        "resource_refs": {"state": "unavailable", "reason": "resource capture is child H"},
        "resource_bodies": {"state": "unavailable", "reason": "resource capture is child H"},
        "resume": {
            "state": "partial",
            "reason": "storage core only; collector adapter is child D",
        },
        "offline_reanalysis": {
            "state": "unavailable",
            "reason": "no retained offline reanalysis inputs yet",
        },
    })
}

fn row(pairs: Vec<(&str, Value)>) -> Map<String, Value> {
    pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

fn context_item(kind: &str, payload: &Value) -> Map<String, Value> {
    row(vec![
        ("kind", json!(kind)),
        ("item_key", json!("run")),
        ("payload_version", json!("scan_context.v1")),
        ("payload_json", Value::String(dump(payload))),
        ("completeness", json!("complete")),
        ("reason", json!("")),
    ])
}

impl NativeScanSchemaMixin {
    /// Create a no-clobber running scan with no audit and no body lanes.
    ///
    /// Defaults: no fingerprint, limitations, retention or initial sitemaps; format `scan.v1`.
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        path: &Path,
        start_url: &str,
        config: &Map<String, Value>,
        config_fingerprint: Option<&str>,
        writer_version: &str,
        writer_revision: &str,
        runtime_versions: &Map<String, Value>,
        limitations: &[String],
        retention: Option<&Map<String, Value>>,
        initial_sitemaps: &[(String, String)],
        format_version: &str,
    ) -> Result<NativeScan, StoreError> {
        let target = Self::create_file(
            path,
            start_url,
            config,
            config_fingerprint,
            writer_version,
            writer_revision,
            runtime_versions,
            limitations,
            retention,
            initial_sitemaps,
            format_version,
        )?;
        Self::open(&target, Some(start_url), Some(config), None, false)
    }

    /// Everything `create` does before it reopens the published file for writing.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn create_file(
        path: &Path,
        start_url: &str,
        config: &Map<String, Value>,
        config_fingerprint: Option<&str>,
        writer_version: &str,
        writer_revision: &str,
        runtime_versions: &Map<String, Value>,
        limitations: &[String],
        retention: Option<&Map<String, Value>>,
        initial_sitemaps: &[(String, String)],
        format_version: &str,
    ) -> Result<PathBuf, StoreError> {
        runtime()?;
        if start_url.is_empty() {
            return Err(bad("native scan start_url is required"));
        }
        if !is_revision(writer_revision) {
            return Err(bad(
                "native scan writer_revision must be a lowercase 40-character Git SHA",
            ));
        }
        let wanted: BTreeSet<&str> = RUNTIME_KEYS.iter().copied().collect();
        let given: BTreeSet<&str> = runtime_versions.keys().map(String::as_str).collect();
        if given != wanted
            || runtime_versions
                .values()
                .any(|v| !v.as_str().is_some_and(|s| !s.is_empty()))
        {
            return Err(bad(
                "native scan requires complete runtime version provenance",
            ));
        }
        let config_value = Value::Object(config.clone());
        let effective = native_config(&config_value, false)?;
        if format_version != "scan.v1" && format_version != "scan.v2" {
            return Err(bad("native scan format_version must be scan.v1 or scan.v2"));
        }
        if object_at(&effective, &["storage", "format_version"])?.as_str() != Some(format_version) {
            return Err(bad(
                "native scan format selection differs from its effective configuration",
            ));
        }
        let sitemaps = bounded_sitemaps(initial_sitemaps, "selected sitemap roots", 5000)?;
        let derived = crawl_fingerprint(&effective)?;
        if config_fingerprint.is_some_and(|given| given != derived) {
            return Err(bad(
                "native scan configuration fingerprint disagrees with effective config",
            ));
        }
        let header_config = frozen_header_config(&effective);
        let header_fingerprint = crawl_fingerprint(&header_config)?;
        let target = std::path::absolute(path)?;
        if fs::symlink_metadata(&target).is_ok() {
            return Err(bad(format!(
                "native scan output already exists: {}",
                target.display()
            )));
        }
        let parent = target.parent().unwrap_or(Path::new("/")).to_path_buf();
        fs::DirBuilder::new()
            .recursive(true)
            .private_mode(0o700)
            .create(&parent)?;
        let temporary = make_temporary(&parent)?;
        let result = Self::create_in(
            &temporary,
            &target,
            start_url,
            config,
            &effective,
            &header_config,
            &header_fingerprint,
            writer_version,
            writer_revision,
            runtime_versions,
            limitations,
            retention,
            &sitemaps,
            format_version,
        );
        let _ = fs::remove_file(&temporary);
        let _ = fs::remove_file(PathBuf::from(format!("{}-wal", temporary.display())));
        let _ = fs::remove_file(PathBuf::from(format!("{}-shm", temporary.display())));
        result.map_err(|e| bad(format!("cannot create native scan: {}", message_of(&e))))?;
        Ok(target)
    }

    /// Everything `create` does between the temporary file and the published hard link.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn create_in(
        temporary: &Path,
        target: &Path,
        start_url: &str,
        config: &Map<String, Value>,
        effective: &Map<String, Value>,
        header_config: &Map<String, Value>,
        header_fingerprint: &str,
        writer_version: &str,
        writer_revision: &str,
        runtime_versions: &Map<String, Value>,
        limitations: &[String],
        retention: Option<&Map<String, Value>>,
        sitemaps: &[(String, String)],
        format_version: &str,
    ) -> Result<(), StoreError> {
        let con = Self::connect_writer(temporary)?;
        con.execute_batch(schema())?;
        let now = utc_now_iso();
        let scan_uuid = uuid::Uuid::new_v4().to_string();
        let effective_index: indexmap::IndexMap<String, Value> = effective
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let expected_policy = policy_for_config(&effective_index)?;
        let policy = match retention {
            Some(given) => validate_policy(&Value::Object(given.clone()))?,
            None => expected_policy.clone(),
        };
        if policy != expected_policy {
            return Err(bad(
                "body retention differs from the effective storage configuration",
            ));
        }
        let policy_value = Value::Object(policy.into_iter().collect());
        con.execute_batch("BEGIN IMMEDIATE")?;
        insert(
            &con,
            "scan",
            &row(vec![
                ("singleton", json!(1)),
                ("scan_uuid", json!(scan_uuid)),
                ("format_version", json!(FORMAT_VERSION)),
                ("evidence_version", json!("crawl.v1")),
                ("writer_version", json!(writer_version)),
                ("writer_revision", json!(writer_revision)),
                (
                    "runtime_versions_json",
                    Value::String(dump(&Value::Object(runtime_versions.clone()))),
                ),
                ("created_at", json!(now)),
                ("finished_at", Value::Null),
                ("source_kind", json!("native")),
                ("parent_scan_uuid", Value::Null),
                ("start_url", json!(start_url)),
                (
                    "config_json",
                    Value::String(dump(&Value::Object(header_config.clone()))),
                ),
                ("config_fingerprint", json!(header_fingerprint)),
                ("lifecycle", json!("running")),
                ("finish_reason", json!("running")),
                ("crawl_partial", json!(0)),
                ("corpus_partial", json!(1)),
                ("evidence_revision", json!(0)),
                ("limitations_json", Value::String(dump(&json!(limitations)))),
                ("capabilities_json", Value::String(dump(&capabilities()))),
                ("retention_json", Value::String(dump(&policy_value))),
                ("pinned", json!(0)),
            ]),
        )?;
        let speed = object_at(effective, &["speed"])?;
        let concurrency = if super::truthy(&speed["adaptive"]) {
            json!(1)
        } else {
            speed["concurrency"].clone()
        };
        insert(
            &con,
            "resume_state",
            &row(vec![
                ("singleton", json!(1)),
                ("state_version", json!("scan_resume.v1")),
                ("max_depth_reached", json!(0)),
                ("elapsed_seconds", json!(0.0)),
                ("circuit_timeout_streak", json!(0)),
                ("circuit_server_error_streak", json!(0)),
                ("crawl_delay_applied", Value::Null),
                (
                    "throttle_state_json",
                    Value::String(dump(&json!({
                        "schema_version": "scan_throttle.v2",
                        "delay_seconds": speed["min_delay_seconds"],
                        "concurrency": concurrency,
                        "consecutive_ok": 0,
                        "requests_used": 0,
                    }))),
                ),
            ]),
        )?;
        insert(
            &con,
            "context_items",
            &context_item(
                "analysis_asset_thresholds",
                &json!({
                    "schema_version": "analysis_asset_thresholds.v1",
                    "thresholds": object_at(effective, &["analysis", "asset_thresholds"])?,
                }),
            ),
        )?;
        insert(&con, "context_items", &config_extension_context(effective)?)?;
        if effective.contains_key("storage") {
            let live = Value::Object(config.clone());
            let implicit = super::truthy(object_at(config, &["http", "credential_headers"])?)
                || super::truthy(object_at(
                    config,
                    &["rendering", "browser", "persistent_profile"],
                )?);
            insert(
                &con,
                "context_items",
                &context_item(
                    "credential_context",
                    &json!({
                        "verifier": deps().credential_verifier(&live, &scan_uuid)?,
                        "implicit_state": implicit,
                    }),
                ),
            )?;
        }
        if format_version == "scan.v2" {
            deps().upgrade_to_v2(&con)?;
        }
        for (ordinal, (sitemap_url, source)) in sitemaps.iter().enumerate() {
            deps().declare_sitemap(&con, sitemap_url, source, ordinal as i64)?;
        }
        Self::validate_native(&con)?;
        con.execute_batch("COMMIT")?;
        // The temporary name entered WAL while schema/header rows were created. Publish only a
        // checkpointed DELETE-journal main file: linking the main file without its
        // temporary-name WAL would lose the committed header and keep an unusable journal mode.
        con.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        con.query_row("PRAGMA journal_mode=DELETE", [], |_| Ok(()))?;
        drop(con);
        fsync_file(temporary)?;
        match fs::hard_link(temporary, target) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                return Err(bad(format!(
                    "native scan output already exists: {}",
                    target.display()
                )))
            }
            Err(e) => return Err(e.into()),
        }
        sync_directory(target.parent().unwrap_or(Path::new("/")));
        Ok(())
    }

    /// Open an unfinished native scan for writing under the single-writer lock.
    ///
    /// Defaults: no expected start URL, config or writer revision; reanalysis is not allowed.
    pub fn open(
        path: &Path,
        expected_start_url: Option<&str>,
        expected_config: Option<&Map<String, Value>>,
        expected_writer_revision: Option<&str>,
        allow_reanalysis: bool,
    ) -> Result<NativeScan, StoreError> {
        let path = std::path::absolute(path)?;
        let meta = fs::symlink_metadata(&path);
        if !meta.as_ref().is_ok_and(|m| m.is_file()) {
            return Err(bad(
                "native scan writer requires an existing regular scan file",
            ));
        }
        if link_identity(&path).map(|i| i.2 != 1).unwrap_or(false) {
            return Err(bad(
                "native scan writer refuses hard-linked database aliases",
            ));
        }
        // SQLite owns byte-range locks on the database file. A separate advisory lock inode
        // prevents two application writers without contending with SQLite's own VFS locks.
        let mut lock_name = path.file_name().unwrap_or_default().to_os_string();
        lock_name.push(".writer.lock");
        let lock = WriterLock::acquire(&path.with_file_name(lock_name))?;
        // Before a rw connection: a missing file must not be created and a foreign file must
        // not be mutated before its application/schema identity is known.
        let inspected = Self::inspect(&path, None)?;
        let scan = inspected
            .get("scan")
            .and_then(Value::as_object)
            .ok_or_else(|| bad("native scan inspection returned no header"))?;
        let text = |key: &str| scan.get(key).and_then(Value::as_str).unwrap_or_default();
        if text("source_kind") == "reanalysis" && !allow_reanalysis {
            return Err(bad("derived reanalysis artifacts cannot resume collection"));
        }
        if matches!(text("lifecycle"), "finished" | "failed") {
            return Err(bad(format!(
                "native scan lifecycle '{}' cannot be opened for writing",
                text("lifecycle")
            )));
        }
        if expected_start_url.is_some_and(|expected| text("start_url") != expected) {
            return Err(bad("native scan start URL differs; refusing unsafe resume"));
        }
        if expected_writer_revision.is_some_and(|expected| text("writer_revision") != expected) {
            return Err(bad(
                "native scan producing build differs; refusing mixed-build resume",
            ));
        }
        // Credential references/values never enter the artifact. A local, per-scan verifier
        // detects a changed explicit context on resume.
        let mut legacy_anonymous = false;
        let mut recorded_credential = false;
        let mut observed = false;
        if let Some(expected) = expected_config {
            let reader = open_scan(&path, false, None)?;
            let effective = read_effective_config(&reader)?;
            // The opt-in redirect-hop probe (#1414) is an audit-time measurement the artifact
            // cannot restate; its recorded value is always the default, so a resume compares
            // with the toggle normalized to off rather than refusing.
            let mut comparable = expected.clone();
            let analysis = comparable
                .entry("analysis")
                .or_insert_with(|| Value::Object(Map::new()));
            if let Value::Object(map) = analysis {
                map.insert("redirect_hop_hsts_probe".into(), Value::Bool(false));
            }
            let comparable_value = Value::Object(comparable.clone());
            let effective_value = Value::Object(effective.clone());
            let thresholds = |c: &Map<String, Value>| {
                c.get("analysis")
                    .and_then(|a| a.get("asset_thresholds"))
                    .cloned()
                    .ok_or_else(|| bad("native config is missing analysis.asset_thresholds"))
            };
            if resume_fingerprint(&comparable_value, &effective_value)?
                != crawl_fingerprint(&effective)?
                || config_extension_values(&comparable)? != config_extension_values(&effective)?
                || thresholds(&comparable)? != thresholds(&effective)?
            {
                return Err(bad(
                    "native scan configuration differs; refusing unsafe resume",
                ));
            }
            let credential: Option<String> = reader
                .query_row(
                    "SELECT payload_json FROM context_items WHERE kind='credential_context' AND item_key='run'",
                    [],
                    |r| r.get(0),
                )
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })?;
            if let Some(payload) = credential {
                recorded_credential = true;
                let recorded: Value = serde_json::from_str(&payload)?;
                observed = reader
                    .prepare("SELECT 1 FROM responses LIMIT 1")?
                    .exists([])?;
                if super::truthy(&recorded["implicit_state"]) && observed {
                    let header: Value = serde_json::from_str(text("config_json"))?;
                    let header = header.as_object().cloned().unwrap_or_default();
                    legacy_anonymous = legacy_anonymous_session(&header, &reader)?;
                    if !legacy_anonymous {
                        return Err(bad(
                            "credentialed or persistent browser state cannot be restored; refusing unsafe resume",
                        ));
                    }
                }
                let verifier = deps()
                    .credential_verifier(&Value::Object(expected.clone()), text("scan_uuid"))?;
                if recorded["verifier"] != json!(verifier) {
                    return Err(bad("credential context differs; refusing unsafe resume"));
                }
            }
        }
        let con = Self::connect_writer(&path)?;
        let anonymous_recorded = expected_config.is_some() && recorded_credential && observed && {
            con.prepare(
                "SELECT 1 FROM context_items WHERE kind='anonymous_session' AND item_key='run'",
            )?
            .exists([])?
        };
        let opened = deps().open_native(&path, con, Some(lock.into_file()))?;
        if expected_config.is_some()
            && recorded_credential
            && observed
            && (anonymous_recorded || legacy_anonymous)
        {
            deps().record_anonymous_session_resume(&opened, legacy_anonymous)?;
        }
        Ok(opened)
    }

    /// Open the database read-write without creating it, with the writer pragmas.
    pub fn connect_writer(path: &Path) -> Result<Connection, StoreError> {
        let resolved = fs::canonicalize(path)?;
        let con = Connection::open_with_flags(
            &resolved,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        con.busy_timeout(Duration::from_secs(5))?;
        con.execute_batch("PRAGMA foreign_keys=ON; PRAGMA trusted_schema=OFF;")?;
        con.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))?;
        con.execute_batch(
            "PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000; PRAGMA cache_size=-8192; PRAGMA temp_store=FILE;",
        )?;
        Ok(con)
    }
}

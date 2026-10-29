//! The comparison job runner: one SQLite checkpoint per output directory,
//! phased indexing, correspondence binding, finding classification and
//! streamed exports, with a validated output cache and resumable cursors.

use std::collections::{BTreeMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::{json, Value};
use sha2::Digest as _;
use spiderhead_core::job_control::JobProgressUpdate;
use spiderhead_core::timeutil::{stamp, Format};
use spiderhead_store::evidence_contract::Obj;

use crate::compare::base::{
    header_obj, load_correspondence, run_of, AuditSource, CompareError, Declaration,
};
use crate::compare::facts::{measurement_gaps, preflight};
use crate::segments::{resolve_order, Segment};
use spiderhead_store::evidence_contract::{comparison_compatibility, comparison_warnings};

use super::checkpoint::{
    commit_txn, discard_completed_checkpoint, set_state, state_value, state_value_or,
};
use super::export::{export_one, iter_compare_rows, INDEX_BATCH};
use super::index::{bind, index_collection};
use super::publish::{publish, reservation_path};
use super::shared::{
    check_cancel, file_sha256_with_size, fsync_directory, notify, py_json, BUCKETS,
};
use super::source::{collection_count, facts_rows, job_id, row, segment_definitions};
use super::COMPARE_V2;

fn state_name(state: &spiderhead_core::job_control::JobState) -> &'static str {
    use spiderhead_core::job_control::JobState;
    match state {
        JobState::Running => "running",
        JobState::PauseRequested => "pause_requested",
        JobState::Paused => "paused",
        JobState::StopRequested => "stop_requested",
        JobState::Cancelled => "cancelled",
        JobState::Finished => "finished",
        JobState::Failed => "failed",
    }
}

/// The persisted job document plus its write path; every phase transition
/// lands here so a crash leaves a resumable checkpoint behind.
pub(crate) struct Recorder<'a> {
    job: Obj,
    job_path: PathBuf,
    job_id: String,
    progress: Option<&'a dyn Fn(&JobProgressUpdate)>,
    cancel_event: Option<&'a dyn Fn() -> bool>,
}

impl<'a> Recorder<'a> {
    pub(crate) fn new(
        job: Obj,
        job_path: &Path,
        job_id: &str,
        progress: Option<&'a dyn Fn(&JobProgressUpdate)>,
        cancel_event: Option<&'a dyn Fn() -> bool>,
    ) -> Self {
        Self {
            job,
            job_path: job_path.to_path_buf(),
            job_id: job_id.to_owned(),
            progress,
            cancel_event,
        }
    }

    /// The stage recorded at the moment of a failure, for the failure report.
    pub(crate) fn stage(&self) -> String {
        self.job
            .get("stage")
            .and_then(Value::as_str)
            .unwrap_or("compare")
            .to_owned()
    }

    /// A copy of one job field, for the failure report's progress figures.
    pub(crate) fn job_value(&self, key: &str) -> Option<Value> {
        self.job.get(key).cloned()
    }

    /// A bare notification without a job.json write.
    pub(crate) fn emit(
        &self,
        state: &str,
        stage: &str,
        completed: Option<i64>,
        total: Option<i64>,
    ) {
        notify(
            self.progress,
            &self.job_id,
            state,
            stage,
            completed,
            total,
            None,
        );
    }

    fn write_job(&self) -> Result<(), CompareError> {
        let mut data = py_json(&Value::Object(self.job.clone()));
        data.push('\n');
        spiderhead_core::filesystem::atomic_write_bytes(
            &self.job_path,
            data.as_bytes(),
            None,
            None,
            true,
        )
        .map_err(|e| CompareError(e.to_string()))
    }

    /// Cancel-aware checkpoint write: one job.json update, then the durable
    /// atomic replace.
    pub(crate) fn checkpoint(
        &mut self,
        state: &str,
        stage: &str,
        extra: &[(&str, Value)],
    ) -> Result<(), CompareError> {
        check_cancel(self.cancel_event)?;
        self.job.insert("state".into(), json!(state));
        self.job.insert("stage".into(), json!(stage));
        self.job
            .insert("updated_at".into(), json!(stamp(Format::Iso)));
        for (key, value) in extra {
            self.job.insert((*key).into(), value.clone());
        }
        self.write_job()
    }

    /// Checkpoint plus a progress notification; notification construction
    /// failures are swallowed, write failures propagate.
    pub(crate) fn report(
        &mut self,
        state: &str,
        stage: &str,
        completed: Option<i64>,
        total: Option<i64>,
        reason: Option<&str>,
    ) -> Result<(), CompareError> {
        self.checkpoint(
            state,
            stage,
            &[
                ("completed", json!(completed)),
                ("total", json!(total)),
                ("reason", json!(reason)),
            ],
        )?;
        notify(
            self.progress,
            &self.job_id,
            state,
            stage,
            completed,
            total,
            reason,
        );
        Ok(())
    }

    /// A forwarded running-index event: persist its fields into job.json and
    /// pass the update to the caller's transport.
    pub(crate) fn persist_progress(
        &mut self,
        update: &JobProgressUpdate,
    ) -> Result<(), CompareError> {
        self.job
            .insert("state".into(), json!(state_name(&update.state)));
        self.job.insert("stage".into(), json!(update.stage.clone()));
        self.job.insert("completed".into(), json!(update.completed));
        self.job.insert("total".into(), json!(update.total));
        self.job
            .insert("reason".into(), json!(update.reason.clone()));
        self.write_job()?;
        if let Some(progress) = self.progress {
            progress(update);
        }
        Ok(())
    }
}

fn truncate_chars(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

/// the reference `bool(...)` over a JSON value.
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(items)) => !items.is_empty(),
    }
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

/// Write or resume a disk-backed comparison with a validated output cache.
#[allow(clippy::too_many_arguments)]
pub fn compare_to_files(
    before: &AuditSource<'_>,
    after: &AuditSource<'_>,
    out_dir: &Path,
    force: bool,
    correspondence: Option<&Value>,
    compression: &str,
    resume: bool,
    progress: Option<&dyn Fn(&JobProgressUpdate)>,
    cancel_event: Option<&dyn Fn() -> bool>,
) -> Result<Obj, CompareError> {
    let destination = absolute(out_dir);
    let declaration: Option<Declaration> = match correspondence {
        Some(value) => Some(load_correspondence(value)?),
        None => None,
    };
    let identities: Obj = serde_json::from_value(json!({
        "before": crate::verification::source_identity(before).map_err(|e| CompareError(e.to_string()))?,
        "after": crate::verification::source_identity(after).map_err(|e| CompareError(e.to_string()))?,
    }))
    .map_err(|e| CompareError(e.to_string()))?;
    let identity = job_id(&identities, force, declaration.as_ref(), compression);
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CompareError(crate::compare::base::os_error_text(&e, parent)))?;
    }
    let work = destination.with_file_name(format!(
        ".{}.compare-work",
        destination
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default()
    ));
    if destination.exists() {
        return compare_to_files_cached(
            before,
            after,
            &destination,
            &work,
            &identities,
            &identity,
            force,
            compression,
            resume,
            progress,
        );
    }
    compare_to_files_fresh(
        before,
        after,
        &destination,
        &work,
        identities,
        declaration,
        identity,
        force,
        compression,
        resume,
        progress,
        cancel_event,
    )
}

/// The validated output cache: an existing destination is either the same
/// comparison (returned after draining every stream) or refused.
#[allow(clippy::too_many_arguments)]
fn compare_to_files_cached(
    _before: &AuditSource<'_>,
    _after: &AuditSource<'_>,
    destination: &Path,
    work: &Path,
    identities: &Obj,
    identity: &str,
    force: bool,
    compression: &str,
    resume: bool,
    progress: Option<&dyn Fn(&JobProgressUpdate)>,
) -> Result<Obj, CompareError> {
    if resume && !destination.join("compare.json").is_file() {
        // A crashed publication may have left the destination half-linked and
        // the staged tree intact: recover it when every member is ours.
        let reservation = reservation_path(destination, identity);
        if reservation.exists() && work.is_dir() && !work.is_symlink() {
            let _ = (|| -> Result<(), CompareError> {
                super::shared::safe_member(&reservation, false, false, 1)?;
                if std::fs::metadata(&reservation)
                    .map_err(|e| {
                        CompareError(crate::compare::base::os_error_text(&e, &reservation))
                    })?
                    .len()
                    != 0
                {
                    return Err(CompareError(
                        "comparison publish reservation is malformed".into(),
                    ));
                }
                super::shared::safe_member(&work.join("output"), true, false, 1)?;
                let staged = work.join("output");
                let mut members: Vec<PathBuf> = std::fs::read_dir(destination)
                    .map_err(|e| {
                        CompareError(crate::compare::base::os_error_text(&e, destination))
                    })?
                    .filter_map(|entry| entry.ok())
                    .map(|entry| entry.path())
                    .collect();
                members.sort();
                let same_inode = |member: &Path, staged: &Path| match (
                    std::fs::metadata(member),
                    std::fs::metadata(staged),
                ) {
                    (Ok(a), Ok(b)) => {
                        use std::os::unix::fs::MetadataExt;
                        a.dev() == b.dev() && a.ino() == b.ino()
                    }
                    _ => false,
                };
                let owned = !members.is_empty()
                    && members.iter().all(|member| {
                        !member.is_symlink()
                            && member.is_file()
                            && staged
                                .join(
                                    member
                                        .file_name()
                                        .map(|name| name.to_os_string())
                                        .unwrap_or_default(),
                                )
                                .is_file()
                            && same_inode(
                                member,
                                &staged.join(
                                    member
                                        .file_name()
                                        .map(|name| name.to_os_string())
                                        .unwrap_or_default(),
                                ),
                            )
                    });
                if owned {
                    for member in &members {
                        std::fs::remove_file(member).map_err(|e| {
                            CompareError(crate::compare::base::os_error_text(&e, member))
                        })?;
                    }
                    std::fs::remove_dir(destination).map_err(|e| {
                        CompareError(crate::compare::base::os_error_text(&e, destination))
                    })?;
                }
                Ok(())
            })();
        }
    }
    if destination.is_symlink() || !destination.join("compare.json").is_file() {
        return Err(CompareError(format!(
            "comparison output already exists: {}",
            destination.display()
        )));
    }
    let cached_manifest = destination.join("compare.json");
    if cached_manifest.is_symlink() {
        return Err(CompareError(
            "comparison manifest must not be a symbolic link".into(),
        ));
    }
    let cached_text = std::fs::read_to_string(&cached_manifest)
        .map_err(|e| CompareError(crate::compare::base::os_error_text(&e, &cached_manifest)))?;
    let cached: Value =
        serde_json::from_str(&cached_text).map_err(|e| CompareError(e.to_string()))?;
    let cached_obj = cached.as_object();
    let cached_job = cached_obj
        .and_then(|obj| obj.get("job"))
        .and_then(Value::as_object);
    let cached_before = cached_obj
        .and_then(|obj| obj.get("before"))
        .and_then(Value::as_object);
    let cached_after = cached_obj
        .and_then(|obj| obj.get("after"))
        .and_then(Value::as_object);
    let expected_before = identities
        .get("before")
        .and_then(|block| block.get("audit_sha256"))
        .and_then(Value::as_str);
    let expected_after = identities
        .get("after")
        .and_then(|block| block.get("audit_sha256"))
        .and_then(Value::as_str);
    let mismatch = cached_obj.is_none()
        || cached_obj.map(|obj| obj.get("schema_version").and_then(Value::as_str))
            != Some(Some(COMPARE_V2))
        || cached_job.is_none()
        || cached_job.map(|job| job.get("id").and_then(Value::as_str)) != Some(Some(identity))
        || cached_before.is_none()
        || cached_before.map(|block| block.get("audit_sha256").and_then(Value::as_str))
            != expected_before.map(Some)
        || cached_after.is_none()
        || cached_after.map(|block| block.get("audit_sha256").and_then(Value::as_str))
            != expected_after.map(Some)
        || cached_obj.map(|obj| obj.get("force").and_then(Value::as_bool)) != Some(Some(force))
        || cached_obj.map(|obj| {
            obj.get("compression")
                .map(|value| value.as_str().unwrap_or("none"))
                .unwrap_or("none")
        }) != Some(compression);
    if mismatch {
        return Err(CompareError(format!(
            "comparison output already exists for different inputs: {}",
            destination.display()
        )));
    }
    let Some(cached_obj) = cached_obj else {
        return Err(CompareError(format!(
            "comparison output already exists for different inputs: {}",
            destination.display()
        )));
    };
    if let Some(files) = cached_obj.get("files").and_then(Value::as_object) {
        for name in files.keys() {
            for stream_row in iter_compare_rows(&cached_manifest, name) {
                stream_row?;
            }
        }
    }
    discard_completed_checkpoint(work, identity);
    let reservation = reservation_path(destination, identity);
    let _ = (|| -> Result<(), CompareError> {
        super::shared::safe_member(&reservation, false, true, 1)?;
        if reservation.exists() {
            let size = std::fs::metadata(&reservation)
                .map_err(|e| CompareError(crate::compare::base::os_error_text(&e, &reservation)))?
                .len();
            if size == 0 {
                std::fs::remove_file(&reservation).map_err(|e| {
                    CompareError(crate::compare::base::os_error_text(&e, &reservation))
                })?;
            }
        }
        Ok(())
    })();
    notify(
        progress,
        identity,
        "finished",
        "cached",
        Some(1),
        Some(1),
        None,
    );
    let mut result = cached_obj.clone();
    result.insert(
        "manifest".into(),
        json!(destination.join("compare.json").display().to_string()),
    );
    result.insert("out_dir".into(), json!(destination.display().to_string()));
    result.insert("cache_hit".into(), json!(true));
    Ok(result)
}

/// Work gates, phases and exports for a destination that does not exist yet.
#[allow(clippy::too_many_arguments)]
fn compare_to_files_fresh(
    before: &AuditSource<'_>,
    after: &AuditSource<'_>,
    destination: &Path,
    work: &Path,
    mut identities: Obj,
    declaration: Option<Declaration>,
    identity: String,
    force: bool,
    compression: &str,
    resume: bool,
    progress: Option<&dyn Fn(&JobProgressUpdate)>,
    cancel_event: Option<&dyn Fn() -> bool>,
) -> Result<Obj, CompareError> {
    if work.exists() && !resume {
        return Err(CompareError(format!(
            "comparison checkpoint exists; pass resume=True to continue: {}",
            work.display()
        )));
    }
    if work.is_symlink() || (work.exists() && !work.is_dir()) {
        return Err(CompareError(
            "comparison checkpoint path must be a regular directory".into(),
        ));
    }
    if resume && !work.exists() {
        return Err(CompareError(format!(
            "no comparison checkpoint exists to resume: {}",
            work.display()
        )));
    }
    if !work.exists() {
        std::fs::create_dir(work)
            .map_err(|e| CompareError(crate::compare::base::os_error_text(&e, work)))?;
        let output = work.join("output");
        std::fs::create_dir(&output)
            .map_err(|e| CompareError(crate::compare::base::os_error_text(&e, &output)))?;
        let correspondence_sha256 = match &declaration {
            Some(decl) => json!(super::shared::hex(&sha2::Sha256::digest(
                py_json(&json!({"origin_map": decl.origin_map, "pairs": decl.pairs})).as_bytes(),
            ))),
            None => Value::Null,
        };
        let initial_job: Obj = serde_json::from_value(json!({
            "schema_version": "compare.job.v1",
            "job_id": identity,
            "identities": identities,
            "force": force,
            "compression": compression,
            "correspondence_sha256": correspondence_sha256,
            "files": {},
            "state": "running",
            "stage": "indexing",
        }))
        .map_err(|e| CompareError(e.to_string()))?;
        let mut data = py_json(&Value::Object(initial_job));
        data.push('\n');
        spiderhead_core::filesystem::atomic_write_bytes(
            &work.join("job.json"),
            data.as_bytes(),
            None,
            None,
            true,
        )
        .map_err(|e| CompareError(e.to_string()))?;
    }
    super::shared::safe_member(work, true, false, 1)?;
    let work_info = std::fs::symlink_metadata(work)
        .map_err(|e| CompareError(crate::compare::base::os_error_text(&e, work)))?;
    let job_path = work.join("job.json");
    super::shared::safe_member(&job_path, false, false, 1)?;
    // Read failures and malformed JSON are folded into one preserved-checkpoint
    // message; a structurally valid but foreign document fails the check below.
    let parsed: Value = (|| -> Result<Value, CompareError> {
        let text = std::fs::read_to_string(&job_path).map_err(|_| {
            CompareError("comparison checkpoint is unreadable; it was preserved".into())
        })?;
        serde_json::from_str(&text).map_err(|_| {
            CompareError("comparison checkpoint is unreadable; it was preserved".into())
        })
    })()?;
    let empty = Obj::new();
    let job = parsed.as_object().unwrap_or(&empty).clone();
    let valid = parsed.is_object()
        && job.get("schema_version").and_then(Value::as_str) == Some("compare.job.v1")
        && job.get("job_id").and_then(Value::as_str) == Some(identity.as_str());
    if !valid {
        return Err(CompareError(
            "comparison checkpoint belongs to different inputs or options".into(),
        ));
    }
    let lock_path = work.join(".writer.lock");
    let _ = super::shared::safe_member(&lock_path, false, true, 1);
    let lock =
        super::shared::LockFile::open(&lock_path, true).map_err(|error| CompareError(error.0))?;
    if lock.try_lock_exclusive().is_err() {
        return Err(CompareError(
            "comparison job already has an active writer".into(),
        ));
    }
    let mut recorder = Recorder::new(job, &job_path, &identity, progress, cancel_event);
    let body = run_phases(
        before,
        after,
        destination,
        work,
        &mut identities,
        &declaration,
        &identity,
        force,
        compression,
        &mut recorder,
        cancel_event,
    );
    let outcome = match body {
        Ok(value) => Ok(value),
        Err(error) => {
            // A cancel is a BaseException in the oracle: it skips the failed
            // checkpoint and the notification instead of being recorded.
            check_cancel(cancel_event)?;
            let stage = recorder.stage();
            let completed = recorder
                .job_value("completed")
                .and_then(|value| value.as_i64());
            let total = recorder.job_value("total").and_then(|value| value.as_i64());
            let reason = truncate_chars(&error.0, 500);
            let _ = recorder.checkpoint("failed", &stage, &[("reason", json!(reason.clone()))]);
            recorder.emit("failed", &truncate_chars(&stage, 64), completed, total);
            Err(error)
        }
    };
    drop(lock);
    if outcome.is_ok() {
        let _ = (|| -> Result<(), CompareError> {
            let current = std::fs::symlink_metadata(work)
                .map_err(|e| CompareError(crate::compare::base::os_error_text(&e, work)))?;
            if work_info.dev() == current.dev() && work_info.ino() == current.ino() {
                let _ = std::fs::remove_dir_all(work);
            }
            Ok(())
        })();
    }
    outcome
}

/// Everything under the writer lock: phases, exports, manifest and publish.
#[allow(clippy::too_many_arguments)]
fn run_phases(
    before: &AuditSource<'_>,
    after: &AuditSource<'_>,
    destination: &Path,
    work: &Path,
    identities: &mut Obj,
    declaration: &Option<Declaration>,
    identity: &str,
    force: bool,
    compression: &str,
    recorder: &mut Recorder<'_>,
    cancel_event: Option<&dyn Fn() -> bool>,
) -> Result<Obj, CompareError> {
    super::shared::safe_member(&work.join("output"), true, false, 1)?;
    super::shared::safe_member(&work.join("index.sqlite"), false, true, 1)?;
    for suffix in ["-journal", "-wal", "-shm"] {
        super::shared::safe_member(&work.join(format!("index.sqlite{suffix}")), false, true, 1)?;
    }
    {
        let output = work.join("output");
        let mut members: Vec<PathBuf> = std::fs::read_dir(&output)
            .map_err(|e| CompareError(crate::compare::base::os_error_text(&e, &output)))?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .collect();
        members.sort();
        for member in members {
            super::shared::safe_member(&member, false, false, 2)?;
        }
    }
    let root = work.join("output");
    let db_path = work.join("index.sqlite");
    let before_defs = segment_definitions(before);
    let after_defs = segment_definitions(after);
    let compatible_segments =
        super::shared::py_json(&json!(before_defs)) == super::shared::py_json(&json!(after_defs));
    let before_segments = if before_defs.is_empty() {
        Vec::new()
    } else {
        resolve_order(&before_defs).map_err(|e| CompareError(e.to_string()))?
    };
    let after_segments = if after_defs.is_empty() {
        Vec::new()
    } else {
        resolve_order(&after_defs).map_err(|e| CompareError(e.to_string()))?
    };
    let mut counts: BTreeMap<(i64, &str), Option<i64>> = BTreeMap::new();
    for (side, source) in [(0i64, before), (1, after)] {
        for name in ["pages", "issues"] {
            counts.insert((side, name), collection_count(source, name));
        }
    }
    let total_index = if counts.values().any(Option::is_none) {
        None
    } else {
        Some(counts.values().filter_map(|value| *value).sum())
    };
    let con = Connection::open(&db_path).map_err(|e| CompareError(e.to_string()))?;
    con.execute_batch("PRAGMA temp_store=FILE; PRAGMA cache_size=-8192; PRAGMA synchronous=FULL;")
        .map_err(|e| CompareError(e.to_string()))?;
    let existing: Option<String> = con
        .query_row(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='job_state'",
            [],
            |row| row.get(0),
        )
        .ok();
    if existing.is_none() {
        con.execute_batch(
            "CREATE TABLE job_state(key TEXT PRIMARY KEY,value TEXT NOT NULL);
             CREATE TABLE pages(side INTEGER,url TEXT,comparison_url TEXT,ordinal INTEGER,document TEXT,segment TEXT NOT NULL,PRIMARY KEY(side,url));
             CREATE TABLE issues(side INTEGER,check_key TEXT,url TEXT,comparison_url TEXT,ordinal INTEGER,document TEXT,PRIMARY KEY(side,check_key,url));
             CREATE TABLE pairs(before_url TEXT PRIMARY KEY,after_url TEXT,state TEXT,document TEXT);
             CREATE TABLE delta(bucket TEXT,check_key TEXT,url TEXT,issue_rowid INTEGER,segment TEXT NOT NULL,PRIMARY KEY(issue_rowid));",
        )
        .map_err(|e| CompareError(e.to_string()))?;
        set_state(&con, "job_id", &json!(identity))?;
        set_state(&con, "phase", &json!("indexing"))?;
        super::checkpoint::commit_txn(&con)?;
    } else if state_value(&con, "job_id")? != Some(json!(identity)) {
        return Err(CompareError(
            "comparison checkpoint index belongs to different inputs".into(),
        ));
    }
    // One durable transaction spans the whole phase runner; each phase
    // checkpoint pairs COMMIT+BEGIN through commit_txn like the oracle's
    // implicit-transaction connection.
    if con.is_autocommit() {
        con.execute_batch("BEGIN")
            .map_err(|e| CompareError(e.to_string()))?;
    }
    run_job_phases(
        &con,
        before,
        after,
        destination,
        &root,
        identities,
        declaration,
        identity,
        force,
        compression,
        recorder,
        cancel_event,
        before_defs,
        compatible_segments,
        before_segments,
        after_segments,
        counts,
        total_index,
    )
}

/// Finalize a completed package: counts, conservation, segment summary and the
/// hard-linked manifest publication.
#[allow(clippy::too_many_arguments)]
fn finalize(
    con: &Connection,
    before: &AuditSource<'_>,
    after: &AuditSource<'_>,
    destination: &Path,
    root: &Path,
    identities: &Obj,
    declaration: &Option<Declaration>,
    identity: &str,
    force: bool,
    compression: &str,
    recorder: &mut Recorder<'_>,
    cancel_event: Option<&dyn Fn() -> bool>,
    before_defs: &[Value],
    compatible_segments: bool,
    files: &Obj,
) -> Result<Obj, CompareError> {
    let mut result = Obj::new();
    result.insert("schema_version".into(), json!(COMPARE_V2));
    for key in ["before", "after"] {
        result.insert(
            key.into(),
            json!(identities.get(key).cloned().unwrap_or(Value::Null)),
        );
    }
    result.insert("force".into(), json!(force));
    result.insert("compression".into(), json!(compression));
    let mut warnings: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for warning in preflight(before, after)
        .into_iter()
        .chain(comparison_warnings(header_obj(before), header_obj(after)))
    {
        if seen.insert(warning.clone()) {
            warnings.push(warning);
        }
    }
    result.insert("warnings".into(), json!(warnings));
    result.insert(
        "compatibility".into(),
        json!(comparison_compatibility(
            header_obj(before),
            header_obj(after)
        )),
    );
    let mut summary = Obj::new();
    for bucket in BUCKETS {
        let rows = files
            .get(bucket)
            .and_then(|entry| entry.get("rows"))
            .and_then(Value::as_i64)
            .unwrap_or_default();
        summary.insert(bucket.into(), json!(rows));
    }
    result.insert("summary".into(), Value::Object(summary.clone()));
    // Conservation is proven before any segment accounting is built.
    let mut conservation = Obj::new();
    let before_issues = con
        .query_row("SELECT count(*) FROM issues WHERE side=0", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|e| CompareError(e.to_string()))?;
    let after_issues = con
        .query_row("SELECT count(*) FROM issues WHERE side=1", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|e| CompareError(e.to_string()))?;
    let left = summary
        .get("left")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let disappeared = summary
        .get("disappeared")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let entered = summary
        .get("entered")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let appeared = summary
        .get("appeared")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let unchanged = summary
        .get("unchanged")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    conservation.insert("before_issues".into(), json!(before_issues));
    conservation.insert("after_issues".into(), json!(after_issues));
    conservation.insert(
        "before_accounted".into(),
        json!(left + disappeared + unchanged),
    );
    conservation.insert(
        "after_accounted".into(),
        json!(entered + appeared + unchanged),
    );
    if before_issues != left + disappeared + unchanged
        || after_issues != entered + appeared + unchanged
    {
        return Err(CompareError("comparison conservation failed".into()));
    }
    conservation.insert("state".into(), json!("complete"));
    result.insert("conservation".into(), Value::Object(conservation));
    result.insert(
        "segment_breakdown".into(),
        json!(segment_breakdown(
            con,
            before_defs,
            compatible_segments,
            &summary
        )?),
    );
    result.insert("policies".into(), json!({
        "identity": "exact check and target URL; declared correspondence only",
        "duplicates": "reject duplicate page URLs, finding keys, and correspondence collisions",
        "suppression": "all retained findings, including suppressed rows; no view filtering",
        "unchanged": "same finding key; both evidence rows retained, not an evidence-equality assertion",
        "partial": "partial or incompatible observations remain warned deltas, not verified fixes",
        "resume": "source identity and comparison options must match the persistent checkpoint",
    }));
    result.insert("files".into(), json!(files));
    result.insert(
        "job".into(),
        json!({"id": identity, "state": "finished", "resumable": false, "cache_hit": false}),
    );
    let gaps = measurement_gaps(before, after);
    if !gaps.is_empty() {
        result.insert("measurement_gaps".into(), json!(gaps));
    }
    if let Some(decl) = declaration {
        result.insert(
            "correspondence".into(),
            json!({
                "schema_version": "url-correspondence.v1",
                "origin_map": decl.origin_map,
            }),
        );
    }
    let manifest_path = root.join("compare.json");
    let staged = root.join(".compare.json.staged");
    for (path, message) in [
        (
            &manifest_path,
            "checkpoint manifest path is unsafe to replace",
        ),
        (&staged, "checkpoint staging path is unsafe to replace"),
    ] {
        if path.exists() {
            let info = std::fs::symlink_metadata(path)
                .map_err(|e| CompareError(crate::compare::base::os_error_text(&e, path)))?;
            if path.is_symlink() || !info.is_file() || info.nlink() != 1 {
                return Err(CompareError(message.into()));
            }
            std::fs::remove_file(path)
                .map_err(|e| CompareError(crate::compare::base::os_error_text(&e, path)))?;
        }
    }
    let mut payload = py_json(&Value::Object(result.clone()));
    payload.push('\n');
    {
        use std::io::Write;
        let mut stream = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)
            .map_err(|e| CompareError(crate::compare::base::os_error_text(&e, &staged)))?;
        stream
            .write_all(payload.as_bytes())
            .map_err(|e| CompareError(e.to_string()))?;
        stream.flush().map_err(|e| CompareError(e.to_string()))?;
        stream.sync_all().map_err(|e| CompareError(e.to_string()))?;
    }
    std::fs::hard_link(&staged, &manifest_path)
        .map_err(|e| CompareError(crate::compare::base::os_error_text(&e, &manifest_path)))?;
    std::fs::remove_file(&staged)
        .map_err(|e| CompareError(crate::compare::base::os_error_text(&e, &staged)))?;
    fsync_directory(root)?;
    recorder.report("running", "publishing", Some(0), Some(1), None)?;
    check_cancel(cancel_event)?;
    publish(root, destination, identity, cancel_event)?;
    recorder.checkpoint(
        "finished",
        "complete",
        &[
            ("files", json!(files)),
            ("completed", json!(1)),
            ("total", json!(1)),
        ],
    )?;
    recorder.emit("finished", "complete", Some(1), Some(1));
    let mut published = result;
    published.insert(
        "manifest".into(),
        json!(format!("{}/compare.json", destination.display())),
    );
    published.insert("out_dir".into(), json!(destination.display().to_string()));
    published.insert("cache_hit".into(), json!(false));
    Ok(published)
}

/// The segment_breakdown block: whole-site totals plus one entry per segment.
fn segment_breakdown(
    con: &Connection,
    before_defs: &[Value],
    compatible_segments: bool,
    summary: &Obj,
) -> Result<Obj, CompareError> {
    let before_pages = con
        .query_row("SELECT count(*) FROM pages WHERE side=0", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|e| CompareError(e.to_string()))?;
    let after_pages = con
        .query_row("SELECT count(*) FROM pages WHERE side=1", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|e| CompareError(e.to_string()))?;
    let before_issues = con
        .query_row("SELECT count(*) FROM issues WHERE side=0", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|e| CompareError(e.to_string()))?;
    let after_issues = con
        .query_row("SELECT count(*) FROM issues WHERE side=1", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|e| CompareError(e.to_string()))?;
    let mut block = Obj::new();
    let named = compatible_segments && !before_defs.is_empty();
    block.insert(
        "state".into(),
        json!(if named { "compared" } else { "unavailable" }),
    );
    if !named {
        block.insert(
            "reason".into(),
            json!("both runs must retain matching named segment definitions"),
        );
    }
    block.insert(
        "whole_site".into(),
        json!({
            "before": {"pages": before_pages, "issues": before_issues},
            "after": {"pages": after_pages, "issues": after_issues},
            "changes": summary,
        }),
    );
    block.insert(
        "segments".into(),
        json!(if named {
            segment_summary(con, before_defs, compatible_segments)?
        } else {
            Obj::new()
        }),
    );
    Ok(block)
}

fn empty_segment_entry() -> Value {
    json!({
        "before": {"pages": 0, "issues": 0},
        "after": {"pages": 0, "issues": 0},
        "changes": {
            "entered": 0, "left": 0, "appeared": 0, "disappeared": 0, "unchanged": 0,
        },
    })
}

fn segment_summary(
    con: &Connection,
    before_defs: &[Value],
    compatible_segments: bool,
) -> Result<Obj, CompareError> {
    let mut summary: BTreeMap<String, Value> = BTreeMap::new();
    fn entry_for<'a>(summary: &'a mut BTreeMap<String, Value>, name: &str) -> &'a mut Value {
        summary
            .entry(name.to_owned())
            .or_insert_with(empty_segment_entry)
    }
    entry_for(&mut summary, "unsegmented");
    if compatible_segments {
        for definition in before_defs {
            let name = definition
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !name.is_empty() {
                entry_for(&mut summary, name);
            }
        }
    }
    let mut statement = con
        .prepare("SELECT segment,count(*) FROM pages WHERE side=? GROUP BY segment")
        .map_err(|e| CompareError(e.to_string()))?;
    for side in [0i64, 1] {
        let label = if side == 0 { "before" } else { "after" };
        let mut rows = statement
            .query([side])
            .map_err(|e| CompareError(e.to_string()))?;
        while let Some(row) = rows.next().map_err(|e| CompareError(e.to_string()))? {
            let segment: String = row.get(0).map_err(|e| CompareError(e.to_string()))?;
            let pages: i64 = row.get(1).map_err(|e| CompareError(e.to_string()))?;
            entry_for(&mut summary, &segment)[label]["pages"] = json!(pages);
        }
    }
    let mut statement = con
        .prepare(
            "SELECT COALESCE(p.segment,'unavailable'),count(*) FROM issues i
             LEFT JOIN pages p ON p.side=i.side AND p.url=i.url WHERE i.side=?
             GROUP BY COALESCE(p.segment,'unavailable')",
        )
        .map_err(|e| CompareError(e.to_string()))?;
    for side in [0i64, 1] {
        let label = if side == 0 { "before" } else { "after" };
        let mut rows = statement
            .query([side])
            .map_err(|e| CompareError(e.to_string()))?;
        while let Some(row) = rows.next().map_err(|e| CompareError(e.to_string()))? {
            let segment: String = row.get(0).map_err(|e| CompareError(e.to_string()))?;
            let issues: i64 = row.get(1).map_err(|e| CompareError(e.to_string()))?;
            let name = if segment.is_empty() {
                "unavailable"
            } else {
                &segment
            };
            entry_for(&mut summary, name)[label]["issues"] = json!(issues);
        }
    }
    let mut statement = con
        .prepare("SELECT segment,bucket,count(*) FROM delta GROUP BY segment,bucket")
        .map_err(|e| CompareError(e.to_string()))?;
    {
        let mut rows = statement
            .query([])
            .map_err(|e| CompareError(e.to_string()))?;
        while let Some(row) = rows.next().map_err(|e| CompareError(e.to_string()))? {
            let segment: String = row.get(0).map_err(|e| CompareError(e.to_string()))?;
            let bucket: String = row.get(1).map_err(|e| CompareError(e.to_string()))?;
            let amount: i64 = row.get(2).map_err(|e| CompareError(e.to_string()))?;
            entry_for(&mut summary, &segment)["changes"][&bucket] = json!(amount);
        }
    }
    let mut statement = con
        .prepare(
            "SELECT COALESCE(p.segment,'unavailable'),count(*) FROM issues b
             JOIN issues a ON a.side=1 AND a.check_key=b.check_key AND a.comparison_url=b.comparison_url
             LEFT JOIN pages p ON p.side=1 AND p.url=a.url WHERE b.side=0
             GROUP BY COALESCE(p.segment,'unavailable')",
        )
        .map_err(|e| CompareError(e.to_string()))?;
    let mut rows = statement
        .query([])
        .map_err(|e| CompareError(e.to_string()))?;
    while let Some(row) = rows.next().map_err(|e| CompareError(e.to_string()))? {
        let segment: String = row.get(0).map_err(|e| CompareError(e.to_string()))?;
        let amount: i64 = row.get(1).map_err(|e| CompareError(e.to_string()))?;
        let name = if segment.is_empty() {
            "unavailable"
        } else {
            &segment
        };
        entry_for(&mut summary, name)["changes"]["unchanged"] = json!(amount);
    }
    let mut out = Obj::new();
    for (name, mut values) in summary {
        if compatible_segments && !before_defs.is_empty() {
            if let Some(entry) = values.as_object_mut() {
                entry.insert(
                    "state".into(),
                    json!(if name == "unavailable" {
                        "unavailable"
                    } else {
                        "compared"
                    }),
                );
            }
        }
        out.insert(name, values);
    }
    Ok(out)
}

/// Flatten one SQL row mapper result into the stream item type.
fn flatten_sql(item: rusqlite::Result<Value>) -> Result<Value, CompareError> {
    match item {
        Ok(value) => Ok(value),
        Err(rusqlite::Error::ToSqlConversionFailure(boxed)) => {
            match boxed.downcast::<CompareError>() {
                Ok(error) => Err(*error),
                Err(other) => Err(CompareError(other.to_string())),
            }
        }
        Err(error) => Err(CompareError(error.to_string())),
    }
}

/// Carry a domain error through a row closure, whose error type must be the
/// connection's; [`flatten_sql`] restores the original message.
fn wrap_err(error: CompareError) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(error))
}

/// Run the resumable phase machine over the checkpoint index: cursors are
/// validated against the stored rows, indexing / binding / classification run
/// to completion, then every export stream is (re)written and the package is
/// finalized. The caller owns one open transaction.
#[allow(clippy::too_many_arguments)]
fn run_job_phases(
    con: &Connection,
    before: &AuditSource<'_>,
    after: &AuditSource<'_>,
    destination: &Path,
    root: &Path,
    identities: &mut Obj,
    declaration: &Option<Declaration>,
    identity: &str,
    force: bool,
    compression: &str,
    recorder: &mut Recorder<'_>,
    cancel_event: Option<&dyn Fn() -> bool>,
    before_defs: Vec<Value>,
    compatible_segments: bool,
    before_segments: Vec<Segment>,
    after_segments: Vec<Segment>,
    counts: BTreeMap<(i64, &'static str), Option<i64>>,
    total_index: Option<i64>,
) -> Result<Obj, CompareError> {
    let count_of =
        |side: i64, name: &str| -> Option<i64> { counts.get(&(side, name)).copied().flatten() };
    let mut phase = state_value_or(con, "phase", json!("indexing"))?
        .as_str()
        .unwrap_or_default()
        .to_owned();
    match phase.as_str() {
        "indexing" | "binding" | "classify_before" | "classify_after" | "export" => {}
        _ => {
            return Err(CompareError(
                "comparison checkpoint has an invalid phase".into(),
            ))
        }
    }
    // Resume validation: stored cursors must match what the index actually
    // contains, and later phases must not skip unfinished work.
    for side in [0i64, 1] {
        for name in ["pages", "issues"] {
            let total = count_of(side, name);
            let stored = state_value_or(con, &format!("indexed:{side}:{name}"), json!(0))?;
            let offset = stored.as_i64();
            let invalid = match offset {
                None => true,
                Some(value) => value < 0 || total.is_some_and(|bound| value > bound),
            };
            if invalid {
                return Err(CompareError(
                    "comparison checkpoint has an invalid source cursor".into(),
                ));
            }
            let offset = offset.unwrap_or_default();
            let (actual, maximum): (i64, i64) = con
                .query_row(
                    &format!("SELECT count(*),COALESCE(max(ordinal),-1) FROM {name} WHERE side=?"),
                    rusqlite::params![side],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|e| CompareError(e.to_string()))?;
            if actual != offset || maximum >= offset {
                return Err(CompareError(
                    "comparison checkpoint cursor does not match indexed rows".into(),
                ));
            }
        }
        let total_issues = count_of(side, "issues");
        let cursor = state_value_or(con, &format!("classified:{side}"), json!(0))?.as_i64();
        if let Some(value) = cursor {
            if value < 0 || total_issues.is_some_and(|bound| value > bound) {
                return Err(CompareError(
                    "comparison checkpoint has an invalid classification cursor".into(),
                ));
            }
        }
        let effective = cursor.unwrap_or_default();
        let actual: i64 = con
            .query_row(
                "SELECT count(*) FROM issues WHERE side=? AND ordinal<?",
                rusqlite::params![side, effective],
                |row| row.get(0),
            )
            .map_err(|e| CompareError(e.to_string()))?;
        let bound = match total_issues {
            Some(total) if total != 0 => effective.min(total),
            _ => effective,
        };
        if cursor.is_none() || actual != bound {
            return Err(CompareError(
                "comparison checkpoint has an invalid classification cursor".into(),
            ));
        }
    }
    if phase != "indexing" {
        for side in [0i64, 1] {
            for name in ["pages", "issues"] {
                let stored =
                    state_value_or(con, &format!("indexed:{side}:{name}"), json!(0))?.as_i64();
                if stored != count_of(side, name) {
                    return Err(CompareError(
                        "comparison checkpoint phase skips unfinished source indexing".into(),
                    ));
                }
            }
        }
    }
    if matches!(
        phase.as_str(),
        "classify_before" | "classify_after" | "export"
    ) {
        let stored = state_value_or(con, "classified:0", json!(0))?.as_i64();
        if stored != count_of(0, "issues") {
            return Err(CompareError(
                "comparison checkpoint skips unfinished before-side classification".into(),
            ));
        }
    }
    if matches!(phase.as_str(), "classify_after" | "export") {
        let stored = state_value_or(con, "classified:1", json!(0))?.as_i64();
        if stored != count_of(1, "issues") {
            return Err(CompareError(
                "comparison checkpoint skips unfinished after-side classification".into(),
            ));
        }
    }
    if phase == "export" {
        let before_count = count_of(0, "issues").unwrap_or_default();
        let after_count = count_of(1, "issues").unwrap_or_default();
        let delta_count: i64 = con
            .query_row("SELECT count(*) FROM delta", [], |row| row.get(0))
            .map_err(|e| CompareError(e.to_string()))?;
        let matched: i64 = con
            .query_row(
                "SELECT count(*) FROM issues b JOIN issues a ON a.side=1 AND a.check_key=b.check_key AND a.comparison_url=b.comparison_url WHERE b.side=0",
                [],
                |row| row.get(0),
            )
            .map_err(|e| CompareError(e.to_string()))?;
        if before_count + after_count != delta_count + 2 * matched {
            return Err(CompareError(
                "comparison checkpoint classification does not conserve findings".into(),
            ));
        }
    }
    if phase == "indexing" {
        let mut completed: i64 = 0;
        let order = [
            (0i64, before, "pages", &before_segments),
            (0, before, "issues", &before_segments),
            (1, after, "pages", &after_segments),
            (1, after, "issues", &after_segments),
        ];
        for (side, source, name, ordered_segments) in order {
            let done = index_collection(
                con,
                source,
                side,
                name,
                ordered_segments,
                &format!("indexed:{side}:{name}"),
                recorder,
                identity,
                completed,
                total_index,
                cancel_event,
            )?;
            completed += done;
        }
        set_state(con, "phase", &json!("binding"))?;
        commit_txn(con)?;
        phase = "binding".into();
    }
    if phase == "binding" {
        recorder.report("running", "binding", Some(0), Some(1), None)?;
        // The oracle opens an explicit transaction here; the caller's open one
        // already provides the same atomicity.
        if let Some(decl) = declaration {
            con.execute("DELETE FROM pairs", [])
                .map_err(|e| CompareError(e.to_string()))?;
            con.execute("UPDATE pages SET comparison_url=url", [])
                .map_err(|e| CompareError(e.to_string()))?;
            con.execute("UPDATE issues SET comparison_url=url", [])
                .map_err(|e| CompareError(e.to_string()))?;
            let page_total = count_of(0, "pages");
            bind(con, decl, recorder, identity, page_total, cancel_event)?;
        }
        con.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS issue_identity ON issues(side,check_key,comparison_url)",
            [],
        )
        .map_err(|e| CompareError(e.to_string()))?;
        con.execute(
            "CREATE INDEX IF NOT EXISTS page_identity ON pages(side,comparison_url)",
            [],
        )
        .map_err(|e| CompareError(e.to_string()))?;
        con.execute(
            "CREATE INDEX IF NOT EXISTS issue_ordinal ON issues(side,ordinal)",
            [],
        )
        .map_err(|e| CompareError(e.to_string()))?;
        set_state(con, "phase", &json!("classify_before"))?;
        commit_txn(con)?;
    }
    for (side, label) in [(0i64, "before"), (1i64, "after")] {
        let stage = format!("classify_{label}");
        if state_value(con, "phase")?.as_ref().and_then(Value::as_str) != Some(stage.as_str()) {
            continue;
        }
        let total = count_of(side, "issues");
        let other = if side == 0 { after } else { before };
        let partial_other = truthy(run_of(header_obj(other)).get("crawl_partial"));
        let (present_bucket, absent_bucket) = if side == 0 {
            ("left", "disappeared")
        } else {
            ("entered", "appeared")
        };
        let mut offset = state_value_or(con, &format!("classified:{side}"), json!(0))?
            .as_i64()
            .unwrap_or_default();
        loop {
            check_cancel(cancel_event)?;
            let ordinals: Vec<i64> = {
                let mut statement = con
                    .prepare(
                        "SELECT ordinal FROM issues WHERE side=? AND ordinal>=? ORDER BY ordinal LIMIT ?",
                    )
                    .map_err(|e| CompareError(e.to_string()))?;
                let mut rows = statement
                    .query(rusqlite::params![side, offset, INDEX_BATCH])
                    .map_err(|e| CompareError(e.to_string()))?;
                let mut out = Vec::new();
                while let Some(db_row) = rows.next().map_err(|e| CompareError(e.to_string()))? {
                    out.push(db_row.get(0).map_err(|e| CompareError(e.to_string()))?);
                }
                out
            };
            if ordinals.is_empty() {
                break;
            }
            let next_offset = ordinals[ordinals.len() - 1] + 1;
            let segment_fallback = if !compatible_segments || !before_defs.is_empty() {
                "unavailable"
            } else {
                "unsegmented"
            };
            con.execute(
                "INSERT INTO delta(bucket,check_key,url,issue_rowid,segment) \
                 SELECT CASE WHEN i.url='' OR ?1 OR op.url IS NOT NULL THEN ?2 ELSE ?3 END, \
                 i.check_key,i.url,i.rowid, \
                 CASE WHEN ?4 THEN 'unavailable' ELSE COALESCE(p.segment,?5) END FROM issues i \
                 LEFT JOIN pages p ON p.side=i.side AND p.url=i.url \
                 LEFT JOIN pages op ON op.side=?6 AND op.comparison_url=i.comparison_url \
                 WHERE i.side=?7 AND i.ordinal>=?8 AND i.ordinal<?9 AND NOT EXISTS (\
                 SELECT 1 FROM issues j WHERE j.side=?10 AND j.check_key=i.check_key \
                 AND j.comparison_url=i.comparison_url)",
                rusqlite::params![
                    partial_other,
                    present_bucket,
                    absent_bucket,
                    !compatible_segments,
                    segment_fallback,
                    1 - side,
                    side,
                    offset,
                    next_offset,
                    1 - side,
                ],
            )
            .map_err(|e| CompareError(e.to_string()))?;
            offset = next_offset;
            set_state(con, &format!("classified:{side}"), &json!(offset))?;
            if total.is_some_and(|bound| offset >= bound) {
                set_state(
                    con,
                    "phase",
                    &json!(if side == 0 {
                        "classify_after"
                    } else {
                        "export"
                    }),
                )?;
            }
            commit_txn(con)?;
            recorder.report("running", &stage, Some(offset), total, None)?;
        }
        if state_value(con, "phase")?.as_ref().and_then(Value::as_str) == Some(stage.as_str()) {
            set_state(
                con,
                "phase",
                &json!(if side == 0 {
                    "classify_after"
                } else {
                    "export"
                }),
            )?;
            commit_txn(con)?;
        }
    }
    con.execute(
        "CREATE INDEX IF NOT EXISTS delta_order ON delta(bucket,check_key,url)",
        [],
    )
    .map_err(|e| CompareError(e.to_string()))?;
    con.execute(
        "CREATE INDEX IF NOT EXISTS delta_segment ON delta(segment,bucket,check_key,url)",
        [],
    )
    .map_err(|e| CompareError(e.to_string()))?;
    commit_txn(con)?;

    let mut files: Obj = recorder
        .job_value("files")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();

    for &bucket in BUCKETS.iter().take(BUCKETS.len() - 1) {
        let total: i64 = con
            .query_row(
                "SELECT COUNT(*) FROM delta WHERE bucket=?",
                rusqlite::params![bucket],
                |row| row.get(0),
            )
            .map_err(|e| CompareError(e.to_string()))?;
        let mut statement = con
            .prepare(
                "SELECT i.side,i.ordinal,i.document FROM delta d JOIN issues i ON i.rowid=d.issue_rowid WHERE d.bucket=? ORDER BY d.check_key,d.url",
            )
            .map_err(|e| CompareError(e.to_string()))?;
        let mut rows = statement
            .query_and_then(rusqlite::params![bucket], |db_row| {
                let side: i64 = db_row.get(0)?;
                let ordinal: i64 = db_row.get(1)?;
                let document: Option<String> = db_row.get(2)?;
                let source = if side == 0 { before } else { after };
                row(source, "issues", ordinal, document.as_deref()).map_err(wrap_err)
            })
            .map_err(|e| CompareError(e.to_string()))?
            .map(flatten_sql);
        export_stream(
            recorder,
            root,
            &mut files,
            bucket,
            compression,
            Some(total),
            &mut rows,
            cancel_event,
        )?;
    }
    let unchanged_total: i64 = con
        .query_row(
            "SELECT COUNT(*) FROM issues b JOIN issues a ON a.side=1 AND a.check_key=b.check_key AND a.comparison_url=b.comparison_url WHERE b.side=0",
            [],
            |row| row.get(0),
        )
        .map_err(|e| CompareError(e.to_string()))?;
    {
        let mut statement = con
            .prepare(
                "SELECT b.ordinal,b.document,a.ordinal,a.document FROM issues b JOIN issues a ON a.side=1 AND a.check_key=b.check_key AND a.comparison_url=b.comparison_url WHERE b.side=0 ORDER BY b.check_key,b.url",
            )
            .map_err(|e| CompareError(e.to_string()))?;
        let mut rows = statement
            .query_and_then([], |db_row| {
                let before_ordinal: i64 = db_row.get(0)?;
                let before_document: Option<String> = db_row.get(1)?;
                let after_ordinal: i64 = db_row.get(2)?;
                let after_document: Option<String> = db_row.get(3)?;
                let before_row = row(before, "issues", before_ordinal, before_document.as_deref())
                    .map_err(wrap_err)?;
                let after_row = row(after, "issues", after_ordinal, after_document.as_deref())
                    .map_err(wrap_err)?;
                Ok(json!({"before": before_row, "after": after_row}))
            })
            .map_err(|e| CompareError(e.to_string()))?
            .map(flatten_sql);
        export_stream(
            recorder,
            root,
            &mut files,
            "unchanged",
            compression,
            Some(unchanged_total),
            &mut rows,
            cancel_event,
        )?;
    }
    {
        let mut statement = con
            .prepare(
                "SELECT check_key,sum(bucket='entered'),sum(bucket='left'),sum(bucket='appeared'),sum(bucket='disappeared') FROM delta GROUP BY check_key ORDER BY check_key",
            )
            .map_err(|e| CompareError(e.to_string()))?;
        let mut rows = statement
            .query_and_then([], |db_row| {
                let check: String = db_row.get(0)?;
                let mut entry = Obj::new();
                entry.insert("check".into(), json!(check));
                for (index, key) in ["entered", "left", "appeared", "disappeared"]
                    .iter()
                    .enumerate()
                {
                    let amount: i64 = db_row.get(index + 1)?;
                    entry.insert((*key).into(), json!(amount));
                }
                Ok(Value::Object(entry))
            })
            .map_err(|e| CompareError(e.to_string()))?
            .map(flatten_sql);
        export_stream(
            recorder,
            root,
            &mut files,
            "by_check",
            compression,
            None,
            &mut rows,
            cancel_event,
        )?;
    }
    let delta_count: i64 = con
        .query_row("SELECT COUNT(*) FROM delta", [], |row| row.get(0))
        .map_err(|e| CompareError(e.to_string()))?;
    let segment_rows_total = delta_count + unchanged_total;
    {
        let unsegmented = before_defs.is_empty() && compatible_segments;
        let mut first = con
            .prepare(
                "SELECT d.bucket,d.segment,i.side,i.ordinal,i.document FROM delta d JOIN issues i ON i.rowid=d.issue_rowid ORDER BY d.segment,d.bucket,d.check_key,d.url",
            )
            .map_err(|e| CompareError(e.to_string()))?;
        let mut second = con
            .prepare(
                "SELECT p.segment,b.ordinal,b.document,a.ordinal,a.document FROM issues b JOIN issues a ON a.side=1 AND a.check_key=b.check_key AND a.comparison_url=b.comparison_url LEFT JOIN pages p ON p.side=1 AND p.url=a.url WHERE b.side=0 ORDER BY p.segment,b.check_key,b.url",
            )
            .map_err(|e| CompareError(e.to_string()))?;
        let first_rows = first
            .query_and_then([], |db_row| {
                let bucket: String = db_row.get(0)?;
                let segment: String = db_row.get(1)?;
                let side: i64 = db_row.get(2)?;
                let ordinal: i64 = db_row.get(3)?;
                let document: Option<String> = db_row.get(4)?;
                let source = if side == 0 { before } else { after };
                let issue =
                    row(source, "issues", ordinal, document.as_deref()).map_err(wrap_err)?;
                Ok(json!({"segment": segment, "change": bucket, "issue": issue}))
            })
            .map_err(|e| CompareError(e.to_string()))?
            .map(flatten_sql);
        let second_rows = second
            .query_and_then([], |db_row| {
                let segment: Option<String> = db_row.get(0)?;
                let before_ordinal: i64 = db_row.get(1)?;
                let before_document: Option<String> = db_row.get(2)?;
                let after_ordinal: i64 = db_row.get(3)?;
                let after_document: Option<String> = db_row.get(4)?;
                let mut name = match segment.as_deref() {
                    Some(text) if !text.is_empty() => text.to_owned(),
                    _ => {
                        if unsegmented {
                            "unsegmented".to_owned()
                        } else {
                            "unavailable".to_owned()
                        }
                    }
                };
                if !compatible_segments {
                    name = "unavailable".to_owned();
                }
                let before_row = row(before, "issues", before_ordinal, before_document.as_deref())
                    .map_err(wrap_err)?;
                let after_row = row(after, "issues", after_ordinal, after_document.as_deref())
                    .map_err(wrap_err)?;
                Ok(json!({
                    "segment": name,
                    "change": "unchanged",
                    "before": before_row,
                    "after": after_row,
                }))
            })
            .map_err(|e| CompareError(e.to_string()))?
            .map(flatten_sql);
        let mut chained: Box<dyn Iterator<Item = Result<Value, CompareError>> + '_> =
            Box::new(first_rows.chain(second_rows));
        export_stream(
            recorder,
            root,
            &mut files,
            "segment_changes",
            compression,
            Some(segment_rows_total),
            &mut chained,
            cancel_event,
        )?;
    }
    if declaration.is_some() {
        {
            let mut statement = con
                .prepare("SELECT document FROM pairs ORDER BY before_url")
                .map_err(|e| CompareError(e.to_string()))?;
            let mut rows = statement
                .query_and_then([], |db_row| {
                    let document: String = db_row.get(0)?;
                    serde_json::from_str(&document)
                        .map_err(|e| wrap_err(CompareError(e.to_string())))
                })
                .map_err(|e| CompareError(e.to_string()))?
                .map(flatten_sql);
            export_stream(
                recorder,
                root,
                &mut files,
                "correspondence",
                compression,
                None,
                &mut rows,
                cancel_event,
            )?;
        }
        let facts = facts_rows(con, before, after)?;
        let mut rows = facts.into_iter().map(|entry| Ok(Value::Object(entry)));
        export_stream(
            recorder,
            root,
            &mut files,
            "facts",
            compression,
            None,
            &mut rows,
            cancel_event,
        )?;
    }

    for (side, label) in [(0i64, "before"), (1i64, "after")] {
        let page_total: i64 = con
            .query_row(
                "SELECT count(*) FROM pages WHERE side=?",
                rusqlite::params![side],
                |row| row.get(0),
            )
            .map_err(|e| CompareError(e.to_string()))?;
        if let Some(entry) = identities.get_mut(label).and_then(Value::as_object_mut) {
            entry.insert("urls_crawled".into(), json!(page_total));
        }
    }
    finalize(
        con,
        before,
        after,
        destination,
        root,
        identities,
        declaration,
        identity,
        force,
        compression,
        recorder,
        cancel_event,
        &before_defs,
        compatible_segments,
        &files,
    )
}

/// Write one named export stream with skip-if-identical resume semantics and a
/// running checkpoint after each completed file.
#[allow(clippy::too_many_arguments)]
fn export_stream(
    recorder: &mut Recorder<'_>,
    root: &Path,
    files: &mut Obj,
    name: &str,
    compression: &str,
    total: Option<i64>,
    rows: &mut dyn Iterator<Item = Result<Value, CompareError>>,
    cancel_event: Option<&dyn Fn() -> bool>,
) -> Result<(), CompareError> {
    let path = root.join(format!(
        "{name}.ndjson{}",
        if compression == "gzip" { ".gz" } else { "" }
    ));
    if let Some(info) = files.get(name) {
        if path.is_file() && !path.is_symlink() {
            // An unreadable prior file is an error, like the oracle's digest
            // call, not a silent rewrite.
            let (digest, size) = file_sha256_with_size(&path)?;
            if Some(digest.as_str()) == info.get("sha256").and_then(Value::as_str)
                && info.get("bytes").and_then(Value::as_i64) == Some(size as i64)
            {
                return Ok(());
            }
        }
    }
    if path.exists() {
        let info = std::fs::symlink_metadata(&path)
            .map_err(|e| CompareError(crate::compare::base::os_error_text(&e, &path)))?;
        if path.is_symlink() || !info.is_file() || info.nlink() != 1 {
            return Err(CompareError(
                "uncheckpointed comparison output file is unsafe to replace".into(),
            ));
        }
        std::fs::remove_file(&path)
            .map_err(|e| CompareError(crate::compare::base::os_error_text(&e, &path)))?;
    }
    let stage = format!("export_{name}");
    recorder.report("running", &stage, Some(0), total, None)?;
    check_cancel(cancel_event)?;
    let mut latest: i64 = 0;
    let mut stream_error: Option<CompareError> = None;
    let exported = {
        let mut on_progress = |count: i64| {
            if stream_error.is_some() {
                return;
            }
            match check_cancel(cancel_event) {
                Err(error) => stream_error = Some(error),
                Ok(()) => {
                    latest = count;
                    if let Err(error) = recorder.report("running", &stage, Some(count), total, None)
                    {
                        stream_error = Some(error);
                    }
                }
            }
        };
        export_one(
            root,
            name,
            rows,
            compression,
            Some(&mut on_progress),
            cancel_event,
        )
    };
    if let Some(error) = stream_error {
        return Err(error);
    }
    let exported = exported?;
    files.insert(name.to_owned(), Value::Object(exported));
    recorder.checkpoint(
        "running",
        "export",
        &[
            ("files", json!(Value::Object(files.clone()))),
            ("completed_file", json!(name)),
            ("completed_rows", json!(latest)),
        ],
    )?;
    Ok(())
}

//! Durable, credential-free destination checkpoints stored beside the local package.
//! The destination error types shared by every module live here too.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

/// Format marker retained in every destination checkpoint file.
pub const DESTINATION_STATE_FORMAT: &str = "spiderhead.bi-destination-state.v1";

/// Hidden directory beside the BI package that holds progress checkpoints.
pub const DESTINATION_STATE_DIRECTORY: &str = ".seohead-destination-state";

/// A local BI package cannot safely feed an optional destination.
#[derive(Debug)]
pub struct BIDestinationError(pub String);

impl fmt::Display for BIDestinationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BIDestinationError {}

/// Google may have committed a request after the client lost its response.
#[derive(Debug)]
pub struct BIDestinationCommitUncertain(pub String);

impl fmt::Display for BIDestinationCommitUncertain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BIDestinationCommitUncertain {}

/// One destination-facing failure: a definitive error, or a commit whose outcome is unknown.
/// Callers can branch on the uncertain variant; both variants are errors.
#[derive(Debug)]
pub enum DestinationError {
    /// A definitive local verdict (`BIDestinationError`).
    Error(BIDestinationError),
    /// The remote outcome is unknown (`BIDestinationCommitUncertain`).
    Uncertain(BIDestinationCommitUncertain),
}

impl DestinationError {
    /// Commit outcome unknown: the caller must not assume the write failed.
    pub fn is_uncertain(&self) -> bool {
        matches!(self, DestinationError::Uncertain(_))
    }
}

impl fmt::Display for DestinationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DestinationError::Error(e) => e.fmt(f),
            DestinationError::Uncertain(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for DestinationError {}

impl From<BIDestinationError> for DestinationError {
    fn from(e: BIDestinationError) -> Self {
        DestinationError::Error(e)
    }
}

impl From<BIDestinationCommitUncertain> for DestinationError {
    fn from(e: BIDestinationCommitUncertain) -> Self {
        DestinationError::Uncertain(e)
    }
}

/// Build a definitive destination error.
pub fn bi_err(message: impl Into<String>) -> DestinationError {
    DestinationError::Error(BIDestinationError(message.into()))
}

/// Build a destination error whose remote outcome is unknown.
pub fn uncertain(message: impl Into<String>) -> DestinationError {
    DestinationError::Uncertain(BIDestinationCommitUncertain(message.into()))
}

/// Return a stable non-secret identity for one resumable destination attempt.
pub fn state_identity(
    destination: &str,
    target: &str,
    operation: &str,
    manifest_sha256: &str,
) -> String {
    use sha2::{Digest, Sha256};
    let encoded = serde_json::to_string(&json!({
        "destination": destination,
        "target": target,
        "operation": operation,
        "manifest_sha256": manifest_sha256,
    }))
    .unwrap_or_default();
    // Compact JSON with sorted keys, built by hand so the key order is deterministic.
    let mut pairs = [
        format!(
            "\"destination\":{}",
            serde_json::to_string(destination).unwrap_or_default()
        ),
        format!(
            "\"manifest_sha256\":{}",
            serde_json::to_string(manifest_sha256).unwrap_or_default()
        ),
        format!(
            "\"operation\":{}",
            serde_json::to_string(operation).unwrap_or_default()
        ),
        format!(
            "\"target\":{}",
            serde_json::to_string(target).unwrap_or_default()
        ),
    ];
    pairs.sort();
    let compact = format!("{{{}}}", pairs.join(","));
    let _ = encoded; // kept for clarity; the compact sorted form is canonical
    let mut hasher = Sha256::new();
    hasher.update(compact.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Keep durable progress beside the local package without altering its manifest.
pub fn state_path(
    root: &Path,
    destination: &str,
    target: &str,
    operation: &str,
    manifest_sha256: &str,
) -> PathBuf {
    root.join(DESTINATION_STATE_DIRECTORY).join(format!(
        "{}.json",
        state_identity(destination, target, operation, manifest_sha256)
    ))
}

/// Atomically retain a restartable, credential-free publication checkpoint.
pub fn save_state(path: &Path, state: &Value) -> Result<(), BIDestinationError> {
    let encoded = crate::bi::writer::dump_manifest(state).map_err(|_| {
        BIDestinationError("destination transaction cannot be persisted safely".into())
    })?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    // Owner-only directory; the parent must already exist.
    match fs::create_dir(parent) {
        Ok(()) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
            }
        }
        Err(exc) if exc.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => {
            return Err(BIDestinationError(
                "destination progress state cannot be persisted".into(),
            ));
        }
    }
    let temporary = parent.join(format!(
        ".state-{}-{:x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));
    fs::write(&temporary, &encoded)
        .and_then(|_| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600));
            }
            fs::rename(&temporary, path)
        })
        .map_err(|_| {
            let _ = fs::remove_file(&temporary);
            BIDestinationError("destination progress state cannot be persisted".into())
        })
}

/// Load only the checkpoint belonging to this exact package and target.
pub fn load_state(
    path: &Path,
    expected: &Map<String, Value>,
) -> Result<Option<Value>, BIDestinationError> {
    if !path.exists() {
        return Ok(None);
    }
    if path.is_symlink() || !path.is_file() {
        return Err(BIDestinationError(
            "destination progress state is not a regular file".into(),
        ));
    }
    let state: Value = serde_json::from_str(
        &fs::read_to_string(path)
            .map_err(|_| BIDestinationError("destination progress state is unreadable".into()))?,
    )
    .map_err(|_| BIDestinationError("destination progress state is unreadable".into()))?;
    let Some(obj) = state.as_object() else {
        return Err(BIDestinationError(
            "destination progress state has an unsupported format".into(),
        ));
    };
    if obj.get("format").and_then(Value::as_str) != Some(DESTINATION_STATE_FORMAT) {
        return Err(BIDestinationError(
            "destination progress state has an unsupported format".into(),
        ));
    }
    if expected
        .iter()
        .any(|(key, value)| obj.get(key) != Some(value))
    {
        return Err(BIDestinationError(
            "destination progress state belongs to another package or target".into(),
        ));
    }
    match obj.get("transaction") {
        Some(Value::Object(_))
        | Some(Value::String(_))
        | Some(Value::Number(_))
        | Some(Value::Array(_))
        | Some(Value::Null) => {}
        _ => {
            return Err(BIDestinationError(
                "destination progress state has an invalid transaction".into(),
            ))
        }
    }
    if !matches!(obj.get("datasets"), Some(Value::Object(_))) {
        return Err(BIDestinationError(
            "destination progress state has no dataset accounting".into(),
        ));
    }
    Ok(Some(state))
}

/// Fresh checkpoint document for one destination attempt.
pub fn new_state(
    destination: &str,
    target: &str,
    operation: &str,
    manifest_sha256: &str,
    datasets: &Map<String, Value>,
    transaction: &Value,
) -> Value {
    let mut entries = Map::new();
    for (name, info) in datasets {
        entries.insert(
            name.clone(),
            json!({
                "input_rows": info.get("rows").cloned().unwrap_or(Value::Null),
                "written_rows": 0,
                "skipped_rows": 0,
                "failed_rows": 0,
                "next_chunk": 0,
            }),
        );
    }
    json!({
        "format": DESTINATION_STATE_FORMAT,
        "destination": destination,
        "target": target,
        "operation": operation,
        "manifest_sha256": manifest_sha256,
        "status": "staging",
        "transaction": transaction,
        "pending": Value::Null,
        "datasets": entries,
    })
}

/// Return complete per-dataset counts without exposing internal cursor state.
pub fn accounting(state: &Value) -> Result<Map<String, Value>, BIDestinationError> {
    let invalid =
        || BIDestinationError("destination progress state has invalid row accounting".into());
    let mut result = Map::new();
    let datasets = state
        .get("datasets")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for (name, value) in datasets {
        let Some(obj) = value.as_object() else {
            return Err(BIDestinationError(
                "destination progress state has an invalid dataset entry".into(),
            ));
        };
        let mut counts = Map::new();
        for key in ["input_rows", "written_rows", "skipped_rows", "failed_rows"] {
            let number = obj.get(key);
            let valid =
                matches!(number, Some(Value::Number(n)) if n.as_i64().is_some_and(|v| v >= 0));
            if !valid {
                return Err(invalid());
            }
            counts.insert(key.to_string(), number.cloned().unwrap_or(Value::Null));
        }
        result.insert(name, Value::Object(counts));
    }
    Ok(result)
}

/// Expose restart-safe accounting for committed, failed and uncertain states.
pub fn result_from_state(state: &Value, reason: Option<&str>) -> Result<Value, BIDestinationError> {
    let counts = accounting(state)?;
    let mut rows = Map::new();
    let mut input = Map::new();
    let mut skipped = Map::new();
    let mut failed = Map::new();
    let mut pending = Map::new();
    for (name, values) in &counts {
        let get = |k: &str| values.get(k).and_then(Value::as_i64).unwrap_or_default();
        rows.insert(name.clone(), json!(get("written_rows")));
        input.insert(name.clone(), json!(get("input_rows")));
        skipped.insert(name.clone(), json!(get("skipped_rows")));
        failed.insert(name.clone(), json!(get("failed_rows")));
        pending.insert(
            name.clone(),
            json!(
                get("input_rows") - get("written_rows") - get("skipped_rows") - get("failed_rows")
            ),
        );
    }
    let mut result = json!({
        "format": "spiderhead.bi-destination-apply.v1",
        "target": state.get("target").cloned().unwrap_or(Value::Null),
        "operation": state.get("operation").cloned().unwrap_or(Value::Null),
        "state": state.get("status").cloned().unwrap_or(Value::Null),
        "manifest_sha256": state.get("manifest_sha256").cloned().unwrap_or(Value::Null),
        "rows": rows,
        "input_rows": input,
        "skipped_rows": skipped,
        "failed_rows": failed,
        "pending_rows": pending,
        "external_companion_refs": state
            .get("external_companion_refs")
            .cloned()
            .unwrap_or_else(|| json!([])),
    });
    if let Some(reason) = reason {
        result
            .as_object_mut()
            .map(|o| o.insert("reason".into(), json!(reason)));
    }
    Ok(result)
}

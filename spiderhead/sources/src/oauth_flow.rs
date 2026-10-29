//! Pending browser-consent flow record for GSC grants.
//!
//! The record holds lifecycle metadata only: no grant material, no client secret, no authorization code.
//! The raw `state` is returned to the caller once and stored as a SHA-256 digest, so the file alone
//! cannot complete a callback.

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::credentials::is_private_mode;
use crate::{pyjson, pytime};

/// `FLOW_TTL` — a waiting flow dies ten minutes after creation.
pub const FLOW_TTL_SECONDS: i64 = 600;
/// `FLOW_LOCK_TIMEOUT_SECONDS` — how long a contender polls for the lock file.
pub const FLOW_LOCK_TIMEOUT_SECONDS: f64 = 35.0;
/// `FLOW_LOCK_POLL_SECONDS` — the retry cadence while a lock exists.
pub const FLOW_LOCK_POLL_SECONDS: f64 = 0.05;

const BASE_KEYS: [&str; 5] = [
    "flow_id",
    "state_sha256",
    "status",
    "created_at",
    "expires_at",
];
const EXTRA_KEYS: [&str; 2] = ["connected_at", "token_expires_at"];
const STATES: [&str; 6] = [
    "waiting",
    "connected",
    "expired",
    "cancelled",
    "revoked",
    "invalid",
];
/// Errors this module raises: invalid input for every refusal, and a type error for a naive/aware timestamp comparison.
#[derive(Debug, thiserror::Error)]
pub enum FlowError {
    /// Invalid input or a refused state.
    #[error("{0}")]
    Invalid(String),
    /// A naive and an aware timestamp were compared.
    #[error("{0}")]
    TypeErr(String),
    /// Filesystem failure.
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

impl FlowError {
    /// Stable error class name reported to callers.
    pub fn python_class(&self) -> &'static str {
        match self {
            FlowError::Invalid(_) => "ValueError",
            FlowError::TypeErr(_) => "TypeError",
            FlowError::Io(_) => "OSError",
        }
    }
}

fn fail<T>(message: &str) -> Result<T, FlowError> {
    Err(FlowError::Invalid(message.to_owned()))
}

/// `hash_state` — the SHA-256 hex digest of a raw callback state.
pub fn hash_state(state: &str) -> String {
    let digest = Sha256::digest(state.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Random token source; injectable so tests can pin the tokens a flow mints
/// (state first at 24 bytes, then `flow_id` at 16).
pub trait Tokens {
    /// Random URL-safe token of the given byte length.
    fn urlsafe(&mut self, nbytes: usize) -> Result<String, FlowError>;
}

/// The OS-backed random token source.
pub struct SystemTokens;

impl Tokens for SystemTokens {
    fn urlsafe(&mut self, nbytes: usize) -> Result<String, FlowError> {
        let mut bytes = vec![0u8; nbytes];
        getrandom::fill(&mut bytes)
            .map_err(|e| FlowError::Io(std::io::Error::other(e.to_string())))?;
        Ok(render_urlsafe(&bytes))
    }
}

/// `n` random bytes rendered as unpadded base64url.
pub fn token_urlsafe(nbytes: usize) -> Result<String, FlowError> {
    SystemTokens.urlsafe(nbytes)
}

fn render_urlsafe(bytes: &[u8]) -> String {
    let nbytes = bytes.len();
    const ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(nbytes.div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().fold(0u32, |acc, b| (acc << 8) | u32::from(*b));
        for i in 0..=chunk.len() {
            let shift = chunk.len() as i32 * 8 - 6 * (i as i32 + 1);
            let six = if shift >= 0 {
                (n >> shift) & 63
            } else {
                (n << (-shift) as u32) & 63
            };
            out.push(ALPHA[six as usize] as char);
        }
    }
    out
}

/// Constant-time equality for digest strings.
fn compare_digest(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

fn is_symlink(path: &Path) -> bool {
    path.symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Refuse symlinks on the record or its directory.
fn check_path(path: &Path) -> Result<(), FlowError> {
    let parent_link = path.parent().map(is_symlink).unwrap_or(false);
    if parent_link || is_symlink(path) {
        return fail("OAuth storage must not use symlinks");
    }
    Ok(())
}

/// `flow_lock` — a guard serializing flow transitions across processes through a lock file.
///
/// The lock is `path.with_name(name + ".lock")` created `O_EXCL`; contenders poll every
/// [`FLOW_LOCK_POLL_SECONDS`] up to [`FLOW_LOCK_TIMEOUT_SECONDS`], then `oauth_flow_busy`.
/// Dropping the guard removes the lock file.
pub struct FlowLock {
    lock: PathBuf,
}

impl FlowLock {
    /// Acquire the lock for `path`'s flow record.
    pub fn acquire(path: &Path) -> Result<Self, FlowError> {
        check_path(path)?;
        if let Some(parent) = path.parent() {
            // Only a directory this call creates receives mode 0700.
            let missing = !parent.exists();
            fs::create_dir_all(parent).map_err(io_fail)?;
            if missing {
                set_private_dir(parent);
            }
        }
        let lock = path.with_file_name(format!(
            "{}.lock",
            path.file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default()
        ));
        check_path(&lock)?;
        let deadline = Instant::now() + Duration::from_secs_f64(FLOW_LOCK_TIMEOUT_SECONDS);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = loop {
            match options.open(&lock) {
                Ok(f) => break f,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if Instant::now() >= deadline {
                        return fail("oauth_flow_busy");
                    }
                    std::thread::sleep(Duration::from_secs_f64(FLOW_LOCK_POLL_SECONDS));
                }
                Err(e) => return Err(FlowError::Io(e)),
            }
        };
        let mut file = file;
        // Write failures propagate, and the lock is still released.
        if let Err(e) = file.write_all(std::process::id().to_string().as_bytes()) {
            let _ = fs::remove_file(&lock);
            return Err(FlowError::Io(e));
        }
        Ok(FlowLock { lock })
    }
}

#[cfg(unix)]
fn set_private_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn set_private_dir(_path: &Path) {}

impl Drop for FlowLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.lock);
    }
}

fn io_fail(e: std::io::Error) -> FlowError {
    FlowError::Io(e)
}

fn write_record(path: &Path, record: &Map<String, Value>) -> Result<(), FlowError> {
    spiderhead_core::filesystem::atomic_write_bytes(
        path,
        pyjson::dumps_opts(&Value::Object(record.clone()), true, true).as_bytes(),
        Some(".oauth-flow-"),
        Some(0o600),
        true,
    )
    .map_err(io_fail)
}

/// `_create_flow_unlocked` — write a new waiting record while the caller holds the lock.
/// Returns `(record, raw_state)`; the raw state never reaches disk.
pub fn create_flow_unlocked(
    path: &Path,
    now: i64,
) -> Result<(Map<String, Value>, String), FlowError> {
    create_flow_unlocked_with(path, now, &mut SystemTokens)
}

/// [`create_flow_unlocked`] with an injected token source; tests pin the minted state and flow id.
pub fn create_flow_unlocked_with(
    path: &Path,
    now: i64,
    tokens: &mut dyn Tokens,
) -> Result<(Map<String, Value>, String), FlowError> {
    check_path(path)?;
    if let Some(parent) = path.parent() {
        let missing = !parent.exists();
        fs::create_dir_all(parent).map_err(io_fail)?;
        if missing {
            set_private_dir(parent);
        }
    }
    let state = tokens.urlsafe(24)?;
    let record = json!({
        "flow_id": tokens.urlsafe(16)?,
        "state_sha256": hash_state(&state),
        "status": "waiting",
        "created_at": pytime::isoformat(now),
        "expires_at": pytime::isoformat(now + FLOW_TTL_SECONDS * 1_000_000),
        "connected_at": Value::Null,
        "token_expires_at": Value::Null,
    });
    let record = record.as_object().cloned().unwrap_or_default();
    write_record(path, &record)?;
    Ok((record, state))
}

/// `create_flow` — like [`create_flow_unlocked`] but takes the flow lock itself.
pub fn create_flow(path: &Path, now: i64) -> Result<(Map<String, Value>, String), FlowError> {
    create_flow_with(path, now, &mut SystemTokens)
}

/// [`create_flow`] with an injected token source.
pub fn create_flow_with(
    path: &Path,
    now: i64,
    tokens: &mut dyn Tokens,
) -> Result<(Map<String, Value>, String), FlowError> {
    let _guard = FlowLock::acquire(path)?;
    create_flow_unlocked_with(path, now, tokens)
}

fn parse_iso(text: &str) -> Result<pytime::Parsed, FlowError> {
    pytime::fromisoformat(text)
        .ok_or_else(|| FlowError::Invalid("OAuth flow record is invalid".to_owned()))
}

/// `load_flow` — read and validate a flow record; `Ok(None)` when it does not exist.
pub fn load_flow(path: &Path) -> Result<Option<Map<String, Value>>, FlowError> {
    check_path(path)?;
    if !path.exists() {
        return Ok(None);
    }
    let info = path.symlink_metadata().map_err(io_fail)?;
    let mode = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            info.mode()
        }
        #[cfg(not(unix))]
        {
            0o600
        }
    };
    if !info.is_file() || !is_private_mode(mode) || info.len() > 4096 {
        return fail("OAuth flow record must be a private bounded regular file");
    }
    let text = fs::read_to_string(path)
        .map_err(|_| FlowError::Invalid("OAuth flow record is unreadable".to_owned()))?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|_| FlowError::Invalid("OAuth flow record is unreadable".to_owned()))?;
    let map = match &value {
        Value::Object(m) => m,
        _ => return fail("OAuth flow record is invalid"),
    };
    let keys: std::collections::BTreeSet<&str> = map.keys().map(String::as_str).collect();
    let base: std::collections::BTreeSet<&str> = BASE_KEYS.into_iter().collect();
    let full: std::collections::BTreeSet<&str> =
        base.iter().chain(EXTRA_KEYS.iter()).copied().collect();
    let status_ok = map
        .get("status")
        .and_then(Value::as_str)
        .map(|s| STATES.contains(&s))
        .unwrap_or(false);
    let flow_id_ok = map
        .get("flow_id")
        .and_then(Value::as_str)
        .map(|s| (1..=64).contains(&s.chars().count()))
        .unwrap_or(false);
    let digest_ok = map
        .get("state_sha256")
        .and_then(Value::as_str)
        .map(|s| s.chars().count() == 64)
        .unwrap_or(false);
    if keys != base && keys != full {
        return fail("OAuth flow record is invalid");
    }
    if !status_ok || !flow_id_ok || !digest_ok {
        return fail("OAuth flow record is invalid");
    }
    parse_iso(
        map.get("created_at")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )?;
    parse_iso(
        map.get("expires_at")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )?;
    for name in EXTRA_KEYS {
        if let Some(text) = map.get(name).and_then(Value::as_str) {
            parse_iso(text)?;
        } else if map.get(name).is_some_and(|v| !v.is_null()) {
            return fail("OAuth flow record is invalid");
        }
    }
    let mut normalized = Map::new();
    normalized.insert("connected_at".to_owned(), Value::Null);
    normalized.insert("token_expires_at".to_owned(), Value::Null);
    normalized.extend(map.clone());
    Ok(Some(normalized))
}

/// `effective_status` — a waiting flow past `expires_at` reads as `expired`.
///
/// `now` is epoch microseconds. A naive stored `expires_at` is an error: it cannot be compared with `now`.
pub fn effective_status(flow: &Map<String, Value>, now: i64) -> Result<String, FlowError> {
    let status = flow
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if status == "waiting" {
        let expires = flow
            .get("expires_at")
            .and_then(Value::as_str)
            .and_then(pytime::fromisoformat);
        match expires {
            Some(pytime::Parsed {
                micros,
                aware: true,
            }) if now >= micros => return Ok("expired".to_owned()),
            Some(pytime::Parsed { aware: true, .. }) => {}
            Some(pytime::Parsed { aware: false, .. }) => {
                return Err(FlowError::TypeErr(
                    "can't compare offset-naive and offset-aware datetimes".to_owned(),
                ))
            }
            None => return fail("OAuth flow record is invalid"),
        }
    }
    Ok(status)
}

/// `cancel_flow` — mark a waiting flow cancelled; repeated cancels are no-ops returning the
/// effective status (an already-expired flow reports `expired` without a write).
pub fn cancel_flow(path: &Path, now: i64) -> Result<Option<Map<String, Value>>, FlowError> {
    let _guard = FlowLock::acquire(path)?;
    let Some(mut flow) = load_flow(path)? else {
        return Ok(None);
    };
    let mut status = effective_status(&flow, now)?;
    if status == "waiting" {
        flow.insert("status".to_owned(), json!("cancelled"));
        status = "cancelled".to_owned();
        write_record(path, &flow)?;
    }
    flow.insert("status".to_owned(), json!(status));
    Ok(Some(flow))
}

/// `verify_callback_state` — one-time correlation check; cancelled or expired flows fail.
pub fn verify_callback_state(
    path: &Path,
    state: Option<&str>,
    now: i64,
) -> Result<Map<String, Value>, FlowError> {
    let Some(flow) = load_flow(path)? else {
        return fail("no_active_flow");
    };
    if effective_status(&flow, now)? != "waiting" {
        return fail("oauth_flow_not_waiting");
    }
    let valid = match state {
        Some(s) if (1..=512).contains(&s.chars().count()) => compare_digest(
            flow.get("state_sha256")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            &hash_state(s),
        ),
        _ => false,
    };
    if !valid {
        return fail("oauth_state_mismatch");
    }
    Ok(flow)
}

/// `complete_flow` — mark connected only after a verified token exchange.
pub fn complete_flow(
    path: &Path,
    flow: &Map<String, Value>,
    now: i64,
    expires_in: i64,
) -> Result<Map<String, Value>, FlowError> {
    if !(1..=86400).contains(&expires_in) {
        return fail("OAuth token expiry is outside the supported range");
    }
    let Some(mut current) = load_flow(path)? else {
        return fail("oauth_flow_not_waiting");
    };
    let same_id = current.get("flow_id") == flow.get("flow_id");
    if !same_id || effective_status(&current, now)? != "waiting" {
        return fail("oauth_flow_not_waiting");
    }
    current.insert("status".to_owned(), json!("connected"));
    current.insert("connected_at".to_owned(), json!(pytime::isoformat(now)));
    current.insert(
        "token_expires_at".to_owned(),
        json!(pytime::isoformat(now + expires_in * 1_000_000)),
    );
    write_record(path, &current)?;
    Ok(current)
}

/// `mark_revoked` — mark stored flow metadata revoked after disconnect or remote revoke.
pub fn mark_revoked(path: &Path) -> Result<Option<Map<String, Value>>, FlowError> {
    mark_state(path, "revoked")
}

/// `mark_invalid` — record that the provider rejected the stored grant during refresh.
pub fn mark_invalid(path: &Path) -> Result<Option<Map<String, Value>>, FlowError> {
    mark_state(path, "invalid")
}

fn mark_state(path: &Path, status: &str) -> Result<Option<Map<String, Value>>, FlowError> {
    let Some(mut flow) = load_flow(path)? else {
        return Ok(None);
    };
    flow.insert("status".to_owned(), json!(status));
    write_record(path, &flow)?;
    Ok(Some(flow))
}

//! Read external-provider secrets from `~/.config`, with environment overrides.
//!
//! The canonical source is a user-readable file under `~/.config`.
//! Environment variables take precedence. A secret
//! value is never printed or included in an exception — messages identify only the missing
//! path or variable.

use serde_json::{json, Value};
use spiderhead_core::host::Host;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// A credential is missing; the message names sources but never exposes a value.
/// Other variants cover invalid input, existing or missing targets, and I/O errors.
#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    /// No usable source exists.
    #[error("{0}")]
    Missing(String),
    /// Rejected input or an unsafe storage layout.
    #[error("{0}")]
    Invalid(String),
    /// The target exists and must not be overwritten.
    #[error("{0}")]
    Exists(String),
    /// Replace was requested for a credential that does not exist.
    #[error("{0}")]
    MissingFile(String),
    /// Filesystem error.
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

impl CredentialError {
    /// Error class name reported to callers.
    pub fn python_class(&self) -> &'static str {
        match self {
            CredentialError::Missing(_) => "MissingCredential",
            CredentialError::Invalid(_) => "ValueError",
            CredentialError::Exists(_) => "FileExistsError",
            CredentialError::MissingFile(_) => "FileNotFoundError",
            CredentialError::Io(_) => "OSError",
        }
    }
}

/// `~/.config`, with `~` expanded to the home directory.
pub fn default_config_root(host: &dyn Host) -> PathBuf {
    if let Some(home) = host.var("HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(home).join(".config");
    }
    if let Some(home) = std::env::home_dir() {
        return home.join(".config");
    }
    PathBuf::from("~/.config")
}

/// True when a secret file is not readable by group or others (POSIX bits; on Windows the
/// profile ACL carries privacy and the check always passes).
pub fn is_private_mode(mode: u32) -> bool {
    if cfg!(windows) {
        return true;
    }
    mode & 0o077 == 0
}

#[cfg(unix)]
fn mode_of(path: &Path) -> std::io::Result<u32> {
    use std::os::unix::fs::MetadataExt;
    Ok(path.metadata()?.mode())
}

#[cfg(not(unix))]
fn mode_of(path: &Path) -> std::io::Result<u32> {
    let _ = path;
    Ok(0o666)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    let _ = (path, mode);
    Ok(())
}

/// Read a secret from `$env_var`, or else from `<root>/<path>`.
pub fn read(
    host: &dyn Host,
    root: &Path,
    path: &str,
    env_var: &str,
    hint: &str,
) -> Result<String, CredentialError> {
    if let Some(from_env) = host.var(env_var).filter(|v| !v.trim().is_empty()) {
        return Ok(from_env.trim().to_owned());
    }
    let file_path = root.join(path);
    if !file_path.exists() {
        return Err(CredentialError::Missing(format!(
            "credential not found: store it in {} or set ${}{}",
            file_path.display(),
            env_var,
            if hint.is_empty() {
                String::new()
            } else {
                format!(". {hint}")
            }
        )));
    }
    let value = fs::read_to_string(&file_path)
        .map_err(CredentialError::Io)?
        .trim()
        .to_owned();
    if value.is_empty() {
        return Err(CredentialError::Missing(format!(
            "{} is empty; store a value there or set ${}",
            file_path.display(),
            env_var
        )));
    }
    Ok(value)
}

/// Return `<root>/<path>` after refusing symlinks on the file and its directory.
fn private_target(root: &Path, path: &str) -> Result<PathBuf, CredentialError> {
    let target = root.join(path);
    let parent = target.parent().unwrap_or(root);
    if parent
        .symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
        || target
            .symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
    {
        return Err(CredentialError::Invalid(format!(
            "credential storage must not use symlinks: {path}"
        )));
    }
    Ok(target)
}

/// Write `data` to a unique temp file in `directory` and fsync it. Returns the staged path;
/// the caller publishes or removes it.
pub(crate) fn stage_bytes(
    directory: &Path,
    prefix: &str,
    data: &[u8],
    mode: u32,
) -> std::io::Result<PathBuf> {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    for counter in 0u32..1000 {
        let candidate = directory.join(format!("{prefix}{pid}-{nanos}-{counter}"));
        let staged = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => {
                let mut file = file;
                let result = file
                    .write_all(data)
                    .and_then(|()| file.sync_all())
                    .and_then(|()| set_mode(&candidate, mode));
                drop(file);
                match result {
                    Ok(()) => Ok(candidate.clone()),
                    Err(e) => {
                        let _ = fs::remove_file(&candidate);
                        Err(e)
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => Err(e),
        };
        return staged;
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "no free staging name",
    ))
}

/// Store `value` at `<root>/<path>` with mode 0600, atomically.
///
/// `replace=false` fails if the file exists; `replace=true` fails if it does not. A failed
/// write leaves the previous file untouched. Errors name only the relative path.
pub fn write_secret(
    root: &Path,
    path: &str,
    value: &str,
    replace: bool,
) -> Result<(), CredentialError> {
    if value.is_empty() {
        return Err(CredentialError::Invalid(format!(
            "empty credential value rejected for {path}"
        )));
    }
    let target = private_target(root, path)?;
    let parent = target.parent().unwrap_or(root);
    fs::create_dir_all(parent)?;
    set_mode(parent, 0o700)?;
    if replace && !target.is_file() {
        return Err(CredentialError::MissingFile(format!(
            "no existing credential to replace at {path}"
        )));
    }
    if !replace && target.exists() {
        return Err(CredentialError::Exists(format!(
            "credential already exists at {path}; use replace"
        )));
    }
    let staged = stage_bytes(parent, ".credential-", value.as_bytes(), 0o600)?;
    let publish = if replace {
        fs::rename(&staged, &target)
    } else {
        // hard_link refuses an existing target, so a concurrent writer cannot be overwritten.
        fs::hard_link(&staged, &target)
    };
    let _ = fs::remove_file(&staged);
    publish?;
    Ok(())
}

/// Delete `<root>/<path>`; return whether a file was removed.
pub fn revoke_secret(root: &Path, path: &str) -> Result<bool, CredentialError> {
    let target = private_target(root, path)?;
    match fs::remove_file(&target) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Whether a credential exists, for diagnostics and graceful provider skips (`available`).
pub fn available(host: &dyn Host, root: &Path, path: &str, env_var: &str) -> bool {
    read(host, root, path, env_var, "").is_ok()
}

fn invalid_source(path: &str, accepted: &[String], reason: &str) -> Value {
    json!({
        "state": "invalid",
        "source_reference": format!("config:{path}"),
        "accepted_source_references": accepted,
        "reason": reason,
    })
}

/// Describe a credential source without its value or an absolute path.
pub fn source_status(
    host: &dyn Host,
    root: &Path,
    path: &str,
    env_var: &str,
    file_env_var: Option<&str>,
) -> Value {
    let mut accepted = vec![format!("env:{env_var}"), format!("config:{path}")];
    let custom_file = file_env_var
        .and_then(|v| host.var(v))
        .map(|v| v.trim().to_owned())
        .unwrap_or_default();
    if let Some(var) = file_env_var {
        accepted.push(format!("env-path:{var}"));
    }
    if let Some(from_env) = host.var(env_var).filter(|v| !v.trim().is_empty()) {
        let _ = from_env;
        return json!({
            "state": "configured_unverified",
            "source_reference": format!("env:{env_var}"),
            "accepted_source_references": accepted,
        });
    }

    if !custom_file.is_empty() {
        let candidate = PathBuf::from(shellexpand_home(host, &custom_file));
        let source_reference = format!("env-path:{}", file_env_var.unwrap_or_default());
        let invalid = |reason: &str| {
            json!({
                "state": "invalid",
                "source_reference": source_reference,
                "accepted_source_references": accepted,
                "reason": reason,
            })
        };
        if candidate
            .symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
            || candidate
                .parent()
                .and_then(|p| p.symlink_metadata().ok())
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false)
        {
            return invalid("configured credential file uses a symlink");
        }
        let info = match candidate.symlink_metadata() {
            Ok(m) => m,
            Err(_) => {
                return invalid("configured credential file is missing or unreadable");
            }
        };
        if !info.is_file() || !is_private_mode(mode_of(&candidate).unwrap_or(0o777)) {
            return invalid("configured credential file is not a private regular file");
        }
        if info.len() > 1024 * 1024 {
            return invalid("configured credential file exceeds 1 MiB");
        }
        let configured = fs::read_to_string(&candidate)
            .ok()
            .map(|t| t.trim().to_owned())
            .unwrap_or_default();
        if configured.is_empty() {
            return invalid("configured credential file is unreadable or empty");
        }
        return json!({
            "state": "configured_unverified",
            "source_reference": source_reference,
            "accepted_source_references": accepted,
        });
    }

    let candidate = root.join(path);
    let is_link = |p: &Path| {
        p.symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
    };
    if is_link(&candidate) || candidate.parent().map(is_link).unwrap_or(false) {
        return invalid_source(path, &accepted, "configured credential path uses a symlink");
    }
    let info = match candidate.symlink_metadata() {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return json!({
                "state": "missing",
                "source_reference": Value::Null,
                "accepted_source_references": accepted,
            });
        }
        Err(_) => {
            return invalid_source(path, &accepted, "configured credential file is unreadable");
        }
    };
    if !info.is_file() || !is_private_mode(mode_of(&candidate).unwrap_or(0o777)) {
        return invalid_source(
            path,
            &accepted,
            "configured credential path is not a private regular file",
        );
    }
    if info.len() > 1024 * 1024 {
        return invalid_source(path, &accepted, "configured credential file exceeds 1 MiB");
    }
    match fs::read_to_string(&candidate) {
        Ok(text) if !text.trim().is_empty() => json!({
            "state": "configured_unverified",
            "source_reference": format!("config:{path}"),
            "accepted_source_references": accepted,
        }),
        Ok(_) => invalid_source(path, &accepted, "configured credential file is empty"),
        Err(_) => invalid_source(
            path,
            &accepted,
            "configured credential file is unreadable or invalid text",
        ),
    }
}

fn shellexpand_home(host: &dyn Host, value: &str) -> String {
    if let Some(rest) = value.strip_prefix('~') {
        if rest.is_empty() || rest.starts_with('/') {
            let home = host
                .var("HOME")
                .filter(|v| !v.is_empty())
                .or_else(|| std::env::home_dir().map(|p| p.display().to_string()))
                .unwrap_or_else(|| "~".to_owned());
            return format!("{home}{rest}");
        }
    }
    value.to_owned()
}

mod providers;

pub use providers::*;

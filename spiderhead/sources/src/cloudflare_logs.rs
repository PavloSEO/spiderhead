//! Bounded local Logpush imports and explicitly started Instant Logs captures.
//! `import_logpush_file` converts a caller-supplied JSON Lines export into the shared log
//! store under the project `logs/` directory.
//! `instant_logs_capture` runs one confirmed, finite WebSocket session with hard caps on
//! hard caps on seconds, events, and bytes.
//!
//! Every filesystem and provider boundary is a seam: `open_project`,
//! `import_logs`, the request transport, the socket factory, cancel, and clock.

use spiderhead_core::filesystem::{file_identity, link_identity, NoFollow, StatCompat};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use spiderhead_core::host::Host;

use crate::cloudflare::{CfError, CfTransport, LiveSend, API};
use crate::cloudflare_traffic::{
    resolve_project_zone, to_apache, BindingsLookup, LogImport, ProjectOpener,
};
use crate::http::{Sleeper, Transport};
use crate::pystr;

mod instant;
pub use instant::*;

/// `MAX_INPUT_BYTES` — one Logpush file may not exceed 16 MiB.
pub const MAX_INPUT_BYTES: u64 = 16 * 1024 * 1024;
/// `MAX_INPUT_ROWS` — the default and maximum `max_events` for a file import.
pub const MAX_INPUT_ROWS: i64 = 10_000;
/// `MAX_LINE_BYTES` — one JSON Lines row may not exceed 64 KiB.
pub const MAX_LINE_BYTES: usize = 65_536;
/// `MAX_CAPTURE_BYTES` — Instant Logs byte ceiling for one session.
pub const MAX_CAPTURE_BYTES: i64 = 8 * 1024 * 1024;
/// `MAX_CAPTURE_EVENTS` — Instant Logs event ceiling for one session.
pub const MAX_CAPTURE_EVENTS: i64 = 1_000;
/// `MAX_CAPTURE_SECONDS` — Instant Logs duration ceiling for one session.
pub const MAX_CAPTURE_SECONDS: i64 = 60;
/// `INSTANT_FIELDS` — the Logpush edge field list asked for at session creation.
pub const INSTANT_FIELDS: [&str; 7] = [
    "ClientIP",
    "ClientRequestURI",
    "EdgeStartTimestamp",
    "EdgeResponseStatus",
    "ClientRequestMethod",
    "ClientRequestUserAgent",
    "EdgeResponseBytes",
];

/// Injected dependencies: project bindings, project root, logstore import and the
/// environment used to read the token.
pub struct LogsEnv<'a> {
    /// Env-var/filesystem host for `cloudflare.read_token`.
    pub host: &'a dyn Host,
    /// `~` expansion for the default token path.
    pub home: &'a dyn Fn(&str) -> PathBuf,
    /// Binding list for a project.
    pub bindings: Option<&'a BindingsLookup<'a>>,
    /// Project root lookup; returns the project path.
    pub open_project: Option<&'a ProjectOpener<'a>>,
    /// Logstore step that imports a temporary file into a project.
    pub import_logs: Option<&'a LogImport<'a>>,
}

/// I/O errors, reported as `[Errno N] text: 'path'`.
fn io_error(path: &Path, error: &std::io::Error) -> CfError {
    let text = path.to_string_lossy();
    match error.kind() {
        std::io::ErrorKind::NotFound => CfError::Python(
            "FileNotFoundError",
            format!(
                "[Errno 2] No such file or directory: {}",
                pystr::repr_str(&text)
            ),
        ),
        std::io::ErrorKind::PermissionDenied => CfError::Python(
            "PermissionError",
            format!("[Errno 13] Permission denied: {}", pystr::repr_str(&text)),
        ),
        _ => CfError::Os(format!("{error}: {text:?}")),
    }
}

/// Stat failures map to their own error kind; anything else becomes an I/O error.
fn lstat(path: &Path) -> Result<std::fs::Metadata, CfError> {
    path.symlink_metadata().map_err(|e| io_error(path, &e))
}

/// Only strings are accepted as paths.
fn to_path(value: &Value) -> Result<PathBuf, CfError> {
    match value.as_str() {
        Some(text) => Ok(PathBuf::from(text)),
        None => Err(CfError::Python(
            "TypeError",
            format!(
                "argument should be a str or an os.PathLike object where __fspath__ returns a str, not '{}'",
                pystr::type_name(value)
            ),
        )),
    }
}

/// Integer argument within `lo..=hi`; other types or values raise the fixed invalid-value error.
fn bounded_int(
    value: Option<&Value>,
    default: i64,
    lo: i64,
    hi: i64,
    message: &str,
) -> Result<i64, CfError> {
    let v = value.cloned().unwrap_or_else(|| json!(default));
    if !pystr::is_py_int(&v) || !(lo..=hi).contains(&v.as_i64().unwrap_or(0)) {
        return Err(CfError::Value(message.to_owned()));
    }
    Ok(v.as_i64().unwrap_or(0))
}

/// Shared tail of both entry points: writes the converted lines to a private temp file
/// inside `<project>/logs`, imports it through the logstore seam and builds the envelope.
///
/// The caller's `sentinel` flag decides the extra blank line that marks a capped
/// prefix (`complete=False`, or `logpull`'s `len(converted) == count`).
#[allow(clippy::too_many_arguments)]
fn write_project_import(
    project_s: &str,
    lines: &[String],
    complete: bool,
    open_project: Option<&ProjectOpener>,
    import_logs: Option<&LogImport>,
    max_lines: i64,
    sentinel: bool,
    prefix: &str,
) -> Result<Value, CfError> {
    if lines.is_empty() {
        return Ok(json!({"ok": true, "state": "empty", "imported": 0, "complete": complete}));
    }
    let Some(open_project) = open_project else {
        return Err(CfError::Cloudflare(
            "project binding seam is unavailable".to_owned(),
        ));
    };
    let opened = open_project(project_s)?;
    let root = match crate::cloudflare::py_subscript(&opened, "path")? {
        Value::String(path) => PathBuf::from(path),
        other => {
            return Err(CfError::Python(
                "TypeError",
                format!(
                    "argument should be a str or an os.PathLike object where __fspath__ returns a str, not '{}'",
                    pystr::type_name(other)
                ),
            ))
        }
    };
    let directory = root.join("logs");
    if directory.is_symlink() || (directory.exists() && !directory.is_dir()) {
        return Err(CfError::Value(
            "project logs directory is unsafe".to_owned(),
        ));
    }
    use spiderhead_core::filesystem::PrivateMode;
    std::fs::DirBuilder::new()
        .recursive(false)
        .private_mode(0o700)
        .create(&directory)
        .map_err(|e| io_error(&directory, &e))?;
    let temporary = directory.join(format!("{prefix}{}.log", std::process::id()));
    let mut text = lines.join("\n");
    text.push('\n');
    if sentinel {
        text.push('\n');
    }
    // The temp file is created owner-only (0600).
    let written = {
        use spiderhead_core::filesystem::PrivateMode;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .private_mode(0o600)
            .open(&temporary)
            .and_then(|mut f| std::io::Write::write_all(&mut f, text.as_bytes()))
    };
    let result = match written {
        Ok(()) => {
            let Some(import_logs) = import_logs else {
                let _ = std::fs::remove_file(&temporary);
                return Err(CfError::Cloudflare(
                    "logstore import seam is unavailable".to_owned(),
                ));
            };
            let path_s = temporary.to_string_lossy().to_string();
            import_logs(&path_s, project_s, max_lines)
        }
        Err(e) => Err(io_error(&temporary, &e)),
    };
    let _ = std::fs::remove_file(&temporary);
    let result = result?;
    if !pystr::truthy(crate::cloudflare::obj_get(&result, "ok")?) {
        return Ok(json!({
            "ok": false,
            "state": "failed",
            "reason": truthy_or(
                crate::cloudflare::obj_get(&result, "error")?.cloned(),
                json!("local log import failed"),
            ),
        }));
    }
    Ok(json!({
        "ok": true,
        "state": if !complete || !pystr::truthy(crate::cloudflare::obj_get(&result, "complete")?) { "partial" } else { "complete" },
        "imported": match crate::cloudflare::obj_get(&result, "lines")? {
            Some(l) if l.is_object() => l.get("parsed").and_then(Value::as_i64).unwrap_or(0),
            Some(l) => return Err(crate::cloudflare::attr_get_error(l)),
            None => 0,
        },
        "complete": complete && pystr::truthy(crate::cloudflare::obj_get(&result, "complete")?),
        "log_import": result,
    }))
}

/// Use the default when the value is falsy.
fn truthy_or(value: Option<Value>, default: Value) -> Value {
    match value {
        Some(v) if pystr::truthy(Some(&v)) => v,
        _ => default,
    }
}

/// Convert a caller-supplied Logpush JSON Lines file into the local logstore.
pub fn import_logpush_file(
    env: &LogsEnv<'_>,
    input_path: &Value,
    project: &Value,
    max_events: Option<&Value>,
) -> Result<Value, CfError> {
    if !pystr::truthy(Some(project)) {
        return Err(CfError::Value(
            "project is required for local Logpush retention".to_owned(),
        ));
    }
    let max = bounded_int(
        max_events,
        MAX_INPUT_ROWS,
        1,
        MAX_INPUT_ROWS,
        &format!("max_events must be between 1 and {MAX_INPUT_ROWS}"),
    )?;
    let project_s = pystr::repr_value(project);
    let path = to_path(input_path)?;
    let before = lstat(&path)?;
    let before_id = link_identity(&path).map_err(|e| io_error(&path, &e))?;
    if !before.file_type().is_file() || before_id.2 != 1 || before.size() > MAX_INPUT_BYTES {
        return Err(CfError::Value(
            "Logpush input must be an unaliased regular file no larger than 16 MiB".to_owned(),
        ));
    }
    // Open without following links, then check that the descriptor is still the file that was
    // stat'd (same inode and device).
    let file = std::fs::OpenOptions::new()
        .read(true)
        .no_follow()
        .open(&path)
        .map_err(|e| io_error(&path, &e))?;
    let opened = file.metadata().map_err(|e| io_error(&path, &e))?;
    let opened_id = file_identity(&file).map_err(|e| io_error(&path, &e))?;
    if !opened.file_type().is_file()
        || opened_id.2 != 1
        || opened.size() > MAX_INPUT_BYTES
        || (opened_id.0, opened_id.1) != (before_id.0, before_id.1)
    {
        return Err(CfError::Value(
            "Logpush input changed while it was being opened".to_owned(),
        ));
    }
    use std::io::Read;
    let mut raw = Vec::new();
    file.take(MAX_INPUT_BYTES + 1)
        .read_to_end(&mut raw)
        .map_err(|e| io_error(&path, &e))?;
    if raw.len() as u64 > MAX_INPUT_BYTES {
        return Err(CfError::Value(
            "Logpush input exceeds the 16 MiB byte limit".to_owned(),
        ));
    }
    let text = String::from_utf8(raw)
        .map_err(|_| CfError::Value("Logpush input must be UTF-8 JSON Lines".to_owned()))?;
    let lines = pystr::splitlines(&text)
        .into_iter()
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>();
    if lines.len() as i64 > max {
        return Err(CfError::Value(
            "Logpush input exceeds max_events".to_owned(),
        ));
    }
    let mut parsed = Vec::with_capacity(lines.len());
    for line in lines {
        if line.len() > MAX_LINE_BYTES {
            return Ok(json!({
                "ok": false,
                "state": "failed",
                "reason": "Logpush JSON line exceeds the 64 KiB limit",
            }));
        }
        let row: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                return Ok(json!({
                    "ok": false,
                    "state": "failed",
                    "reason": crate::pyjson::decode_error(&line)
                        .unwrap_or_else(|| "Expecting value: line 1 column 1 (char 0)".to_owned()),
                }))
            }
        };
        if !row.is_object() {
            return Ok(json!({
                "ok": false,
                "state": "failed",
                "reason": "Logpush line must contain a JSON object",
            }));
        }
        match to_apache(&row) {
            Ok(line) => parsed.push(line),
            Err(e) => return Ok(json!({"ok": false, "state": "failed", "reason": e.to_string()})),
        }
    }
    write_project_import(
        &project_s,
        &parsed,
        true,
        env.open_project,
        env.import_logs,
        parsed.len() as i64,
        false,
        ".cloudflare-import-",
    )
}

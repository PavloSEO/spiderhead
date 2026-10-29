//! Bounded, restricted browser sidecars referenced from native scan context.
//!
//!
//! Sidecar bodies are content-addressed: the file name is the SHA-256 of the exact JSON bytes,
//! so the body is serialized byte-for-byte with compact separators
//! (ASCII-escaped, insertion order); `payload_json` sorts keys.

use crate::error::{StoreError, StoreResult};
use indexmap::IndexMap;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use spiderhead_core::filesystem::{set_permissions_mode, PrivateMode};
use std::ffi::OsString;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const KIND: &str = "browser_artifacts";

pub const VERSION: &str = "browser_artifacts.v1";

pub const MAX_SCREENSHOT_BYTES: i64 = 10485760;

pub const MAX_SCREENSHOT_WIDTH: i64 = 4096;

pub const MAX_SCREENSHOT_HEIGHT: i64 = 30000;

pub const MAX_SCREENSHOT_PIXELS: i64 = 80000000;

pub const MAX_CONSOLE_ERRORS: i64 = 100;

pub const MAX_CONSOLE_CHARS: i64 = 1000;

pub const MAX_CONSOLE_MESSAGES: i64 = 100;

pub const MAX_CONSOLE_LOCATION_CHARS: i64 = 300;

/// Case-insensitive Unicode regex source.
pub const SECRET: &str = r"(?i)(?:authorization|token|secret|password|cookie)\s*[:=]\s*[^\s,;]+";

/// Regex source with Unicode matching.
pub const URL: &str = r#"https?://[^\s'\"]+"#;

static SECRET_RE: OnceLock<Option<fancy_regex::Regex>> = OnceLock::new();
static URL_RE: OnceLock<Option<fancy_regex::Regex>> = OnceLock::new();
static NULL: Value = Value::Null;

/// Build a `StoreError::Invalid`.
fn invalid(message: &str) -> StoreError {
    StoreError::Invalid(message.to_string())
}

/// Text of an error as it appears in the retention reason.
fn error_text(error: &StoreError) -> String {
    match error {
        StoreError::Invalid(message) => message.clone(),
        other => other.to_string(),
    }
}

/// Parent directory of `scan_path`, with `.` for a bare file name.
fn parent_of(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// `Path.is_symlink()`: the entry itself is a symbolic link.
fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
}

/// `Path.is_dir()`: follows symbolic links, false when missing.
fn is_dir(path: &Path) -> bool {
    fs::metadata(path)
        .map(|meta| meta.is_dir())
        .unwrap_or(false)
}

/// `Path.is_file()`: follows symbolic links, false when missing.
fn is_file(path: &Path) -> bool {
    fs::metadata(path)
        .map(|meta| meta.is_file())
        .unwrap_or(false)
}

/// Resolve a path that may not exist: canonicalize the longest existing ancestor and
/// append the rest unchanged.
fn resolve_lenient(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut existing = absolute.clone();
    let mut tail: Vec<OsString> = Vec::new();
    loop {
        match fs::canonicalize(&existing) {
            Ok(real) => {
                let mut out = real;
                for part in tail.iter().rev() {
                    out.push(part);
                }
                return out;
            }
            Err(_) => match existing.file_name() {
                Some(name) => {
                    tail.push(name.to_os_string());
                    if !existing.pop() {
                        return absolute;
                    }
                }
                None => return absolute,
            },
        }
    }
}

/// Truthiness of a JSON value: null, false, zero and empty values are false.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().map(|n| n != 0.0).unwrap_or(true),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// Text form of a JSON value; containers use JSON text.
fn py_str(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        other => py_dump(other, false),
    }
}

/// Text of a value, or empty when it is falsy.
fn py_text(value: &Value) -> String {
    if truthy(value) {
        py_str(value)
    } else {
        String::new()
    }
}

/// First `count` characters (code points, not bytes).
fn take_chars(text: &str, count: i64) -> String {
    text.chars().take(count.max(0) as usize).collect()
}

/// JSON string literal with non-ASCII escaped.
fn py_quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(ch),
            _ => {
                let mut units = [0u16; 2];
                for unit in ch.encode_utf16(&mut units).iter() {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
    out
}

/// Compact JSON text of a value; keys are sorted when `sort_keys` is set.
fn py_dump(value: &Value, sort_keys: bool) -> String {
    let mut out = String::new();
    write_json(value, sort_keys, &mut out);
    out
}

/// Compact JSON text with sorted keys.
pub(crate) fn py_dump_sorted(value: &Value) -> String {
    py_dump(value, true)
}

fn write_json(value: &Value, sort_keys: bool, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Number(number) => out.push_str(&number.to_string()),
        Value::String(text) => out.push_str(&py_quote(text)),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_json(item, sort_keys, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            if sort_keys {
                entries.sort_by(|left, right| left.0.cmp(right.0));
            }
            out.push('{');
            for (index, (key, item)) in entries.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&py_quote(key));
                out.push(':');
                write_json(item, sort_keys, out);
            }
            out.push('}');
        }
    }
}

/// Lowercase hex SHA-256 of the given bytes.
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// True when the text is a 64-character lowercase hex digest.
fn is_hex64(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// True when the value is a 64-character lowercase hex digest.
fn valid_sha(value: Option<&Value>) -> bool {
    value.and_then(Value::as_str).map(is_hex64).unwrap_or(false)
}

/// True when `value` is a JSON object whose keys are exactly `keys`.
fn exact_keys(value: &Value, keys: &[&str]) -> bool {
    value
        .as_object()
        .map(|map| map.len() == keys.len() && keys.iter().all(|key| map.contains_key(*key)))
        .unwrap_or(false)
}

/// Integer value within `low..=high`; booleans do not count.
fn int_in(value: Option<&Value>, low: i64, high: i64) -> bool {
    value
        .and_then(Value::as_i64)
        .map(|n| (low..=high).contains(&n))
        .unwrap_or(false)
}

fn secret_re() -> Option<&'static fancy_regex::Regex> {
    SECRET_RE
        .get_or_init(|| fancy_regex::Regex::new(SECRET).ok())
        .as_ref()
}

fn url_re() -> Option<&'static fancy_regex::Regex> {
    URL_RE
        .get_or_init(|| fancy_regex::Regex::new(URL).ok())
        .as_ref()
}

/// Bound, then redact secrets and URLs in already-stringified text.
///
/// Fails closed: when a pattern cannot be compiled, the whole text is replaced.
fn redact_text(text: &str) -> String {
    let clipped = take_chars(text, MAX_CONSOLE_CHARS);
    match (secret_re(), url_re()) {
        (Some(secret), Some(url)) => {
            let scrubbed = secret.replace_all(&clipped, "[redacted]");
            url.replace_all(&scrubbed, "[url]").into_owned()
        }
        _ => "[redacted]".to_string(),
    }
}

/// Directory of the browser-staging area for a scan file.
pub fn staging_dir(scan_path: &Path) -> PathBuf {
    let name = scan_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    scan_path.with_file_name(format!("{name}.browser-staging"))
}

/// Root of the restricted browser-artifact sidecar of a scan file.
pub fn root(scan_path: &Path) -> PathBuf {
    let name = scan_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    scan_path.with_file_name(format!("{name}.browser-artifacts"))
}

/// Create `path` (and the sidecar root) as private directories, refusing symbolic links.
pub fn restricted_directory(scan_path: &Path, path: &Path) -> Result<(), StoreError> {
    let scan_parent = parent_of(scan_path);
    if is_symlink(&scan_parent) || !is_dir(&scan_parent) {
        return Err(invalid("scan parent is unsafe for browser artifacts"));
    }
    let sidecar_root = root(scan_path);
    if is_symlink(&sidecar_root) {
        return Err(invalid("browser artifact root must not be a symlink"));
    }
    make_private_dir(&sidecar_root)?;
    if is_symlink(&sidecar_root) || !is_dir(&sidecar_root) {
        return Err(invalid("browser artifact root is unsafe"));
    }
    chmod(&sidecar_root, 0o700)?;
    if path.strip_prefix(&sidecar_root).is_err() {
        return Err(invalid(
            "browser artifact directory is outside the scan sidecar",
        ));
    }
    if is_symlink(path) {
        return Err(invalid("browser artifact child must not be a symlink"));
    }
    make_private_dir(path)?;
    if is_symlink(path) || !is_dir(path) {
        return Err(invalid("browser artifact child is unsafe"));
    }
    chmod(path, 0o700)?;
    Ok(())
}

/// `mkdir(mode=0o700, exist_ok=True)`.
fn make_private_dir(path: &Path) -> Result<(), StoreError> {
    match DirBuilder::new().private_mode(0o700).create(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && is_dir(path) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn chmod(path: &Path, mode: u32) -> Result<(), StoreError> {
    set_permissions_mode(path, mode)?;
    Ok(())
}

/// Redact a JSON value's text.
pub fn redact(value: &Value) -> String {
    redact_text(&py_text(value))
}

/// Location fields of one console message, bounded, without a full URL.
fn location_pairs(value: &Value) -> Vec<(String, Value)> {
    let Some(map) = value.as_object() else {
        return Vec::new();
    };
    let mut pairs = Vec::new();
    if let Some(url) = map.get("url").and_then(Value::as_str) {
        let redacted = redact_text(url);
        pairs.push((
            "url".to_string(),
            Value::String(take_chars(&redacted, MAX_CONSOLE_LOCATION_CHARS)),
        ));
    }
    for key in ["line", "column"] {
        if let Some(number) = map.get(key).and_then(Value::as_i64) {
            pairs.push((key.to_string(), Value::from(number)));
        }
    }
    pairs
}

/// Bound one console message's location without reintroducing a full URL.
pub fn console_location(value: &Value) -> IndexMap<String, Value> {
    location_pairs(value).into_iter().collect()
}

/// One retained console message in insertion order (type, text, location).
struct RawRecord {
    kind: String,
    text: String,
    location: Vec<(String, Value)>,
}

/// Bounded, redacted console messages in order; at most `MAX_CONSOLE_MESSAGES`.
fn raw_records(messages: &Value) -> Vec<RawRecord> {
    let mut records = Vec::new();
    let Some(items) = messages.as_array() else {
        return records;
    };
    for item in items {
        let Some(map) = item.as_object() else {
            break;
        };
        if records.len() as i64 >= MAX_CONSOLE_MESSAGES {
            break;
        }
        let kind_value = map.get("type").filter(|value| truthy(value));
        let kind = match kind_value {
            Some(value) => take_chars(&py_str(value), 32),
            None => take_chars("unknown", 32),
        };
        records.push(RawRecord {
            kind,
            text: redact(map.get("text").unwrap_or(&NULL)),
            location: location_pairs(map.get("location").unwrap_or(&NULL)),
        });
    }
    records
}

/// Ordered JSON text of one raw record.
fn record_json(record: &RawRecord) -> String {
    let location = record
        .location
        .iter()
        .map(|(key, value)| format!("{}:{}", py_quote(key), py_dump(value, false)))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"type\":{},\"text\":{},\"location\":{{{location}}}}}",
        py_quote(&record.kind),
        py_quote(&record.text)
    )
}

/// Bound and redact captured console messages for retention.
pub fn console_records(messages: &Value) -> Vec<IndexMap<String, Value>> {
    raw_records(messages)
        .into_iter()
        .map(|record| {
            let location = record
                .location
                .into_iter()
                .collect::<serde_json::Map<_, _>>();
            IndexMap::from([
                ("type".to_string(), Value::String(record.kind)),
                ("text".to_string(), Value::String(record.text)),
                ("location".to_string(), Value::Object(location)),
            ])
        })
        .collect()
}

/// Width and height of a PNG, refusing anything beyond the retention bounds.
pub fn png_dimensions(path: &Path) -> Result<(i64, i64), StoreError> {
    let mut header = Vec::with_capacity(24);
    File::open(path)?.take(24).read_to_end(&mut header)?;
    if header.get(..8) != Some(b"\x89PNG\r\n\x1a\n".as_slice())
        || header.get(12..16) != Some(b"IHDR".as_slice())
    {
        return Err(invalid("browser screenshot is not a PNG"));
    }
    if header.len() < 24 {
        return Err(invalid("browser screenshot PNG header is truncated"));
    }
    let width = u32::from_be_bytes([header[16], header[17], header[18], header[19]]) as u64;
    let height = u32::from_be_bytes([header[20], header[21], header[22], header[23]]) as u64;
    if width == 0
        || height == 0
        || width > MAX_SCREENSHOT_WIDTH as u64
        || height > MAX_SCREENSHOT_HEIGHT as u64
        || width * height > MAX_SCREENSHOT_PIXELS as u64
    {
        return Err(invalid(
            "browser screenshot dimensions exceed the retention bound",
        ));
    }
    Ok((width as i64, height as i64))
}

/// Retention state dict: `{"state", "reason", "ref"}`.
fn state_dict(state: &str, reason: &str, reference: Value) -> IndexMap<String, Value> {
    IndexMap::from([
        ("state".to_string(), Value::String(state.to_string())),
        ("reason".to_string(), Value::String(reason.to_string())),
        ("ref".to_string(), reference),
    ])
}

/// Move a staged screenshot into the content-addressed store.
pub fn move_screenshot(
    scan_path: &Path,
    source: Option<&str>,
) -> Result<IndexMap<String, Value>, StoreError> {
    let Some(source) = source.filter(|text| !text.is_empty()) else {
        return Ok(state_dict(
            "unavailable",
            "renderer produced no screenshot",
            Value::Null,
        ));
    };
    let staging = staging_dir(scan_path);
    if is_symlink(&staging) || is_symlink(&parent_of(&staging)) {
        return Ok(state_dict(
            "unavailable",
            "browser screenshot staging directory is unsafe",
            Value::Null,
        ));
    }
    let staged = resolve_lenient(&staging);
    match move_screenshot_inner(scan_path, Path::new(source), &staged) {
        Ok(stored) => Ok(stored),
        Err(error) => Ok(state_dict(
            "unavailable",
            &redact_text(&error_text(&error)),
            Value::Null,
        )),
    }
}

fn move_screenshot_inner(
    scan_path: &Path,
    source: &Path,
    staged: &Path,
) -> Result<IndexMap<String, Value>, StoreError> {
    let resolved = fs::canonicalize(source)?;
    if is_symlink(source)
        || !resolved.ancestors().skip(1).any(|parent| parent == staged)
        || !is_file(&resolved)
    {
        return Err(invalid(
            "browser screenshot is outside the restricted staging directory",
        ));
    }
    let size = fs::metadata(&resolved)?.len();
    if !(1..=MAX_SCREENSHOT_BYTES as u64).contains(&size) {
        return Err(invalid(
            "browser screenshot exceeds the byte retention bound",
        ));
    }
    let (width, height) = png_dimensions(&resolved)?;
    let bytes = fs::read(&resolved)?;
    let digest = sha256_hex(&bytes);
    let target = root(scan_path)
        .join("screenshots")
        .join(format!("{digest}.png"));
    restricted_directory(scan_path, &parent_of(&target))?;
    if target.exists() {
        if is_symlink(&target) || sha256_hex(&fs::read(&target)?) != digest {
            return Err(invalid(
                "browser screenshot destination conflicts with different bytes",
            ));
        }
        fs::remove_file(&resolved)?;
    } else {
        fs::rename(&resolved, &target)?;
        chmod(&target, 0o600)?;
    }
    Ok(state_dict(
        "stored",
        "",
        json!({"sha256": digest, "bytes": size, "width": width, "height": height}),
    ))
}

/// Write one console body exactly once, content-addressed, with owner-only permissions.
fn store_console_body(scan_path: &Path, target: &Path, body: &str) -> Result<(), StoreError> {
    restricted_directory(scan_path, &parent_of(target))?;
    if target.exists() {
        if is_symlink(target) || fs::read_to_string(target)? != body {
            return Err(invalid(
                "browser console destination conflicts with different content",
            ));
        }
        return Ok(());
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .private_mode(0o600)
        .open(target)?;
    file.write_all(body.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

/// Persist the bounded console errors and messages as a content-addressed sidecar.
///
pub fn save_console(
    scan_path: &Path,
    errors: &Value,
    omitted: &Value,
    messages: Option<&Value>,
    messages_omitted: &Value,
) -> Result<IndexMap<String, Value>, StoreError> {
    let values: Vec<String> = match errors.as_array() {
        Some(items) => items
            .iter()
            .map(redact)
            .take(MAX_CONSOLE_ERRORS as usize)
            .collect(),
        None => Vec::new(),
    };
    let error_list = values
        .iter()
        .map(|value| py_quote(value))
        .collect::<Vec<_>>()
        .join(",");
    let mut body = format!("{{\"schema_version\":\"browser_console.v1\",\"errors\":[{error_list}]");
    // Additive section (#1475): absent key means the capture predates messages.
    if let Some(messages) = messages {
        let records = raw_records(messages)
            .iter()
            .map(record_json)
            .collect::<Vec<_>>()
            .join(",");
        let truncated = messages_omitted.as_i64().map(|n| n > 0).unwrap_or(false);
        body.push_str(&format!(
            ",\"messages\":[{records}],\"messages_truncated\":{truncated}"
        ));
    }
    body.push('}');
    let omitted_count = omitted.as_i64().filter(|n| *n >= 0).unwrap_or(0);
    let digest = sha256_hex(body.as_bytes());
    let target = root(scan_path)
        .join("console")
        .join(format!("{digest}.json"));
    match store_console_body(scan_path, &target, &body) {
        Ok(()) => {
            let reason = if omitted_count > 0 {
                "console error count exceeded the retention bound"
            } else {
                ""
            };
            Ok(state_dict(
                if omitted_count > 0 {
                    "partial"
                } else {
                    "stored"
                },
                reason,
                json!({"sha256": digest, "captured": values.len(), "omitted": omitted_count}),
            ))
        }
        Err(error) => Ok(state_dict(
            "unavailable",
            &redact_text(&error_text(&error)),
            Value::Null,
        )),
    }
}

/// Persist opt-in browser sidecars and return one portable native context item.
pub fn save(
    scan_path: &Path,
    page_url_id: i64,
    document_id: i64,
    rendered: &Value,
    screenshots: bool,
    console_errors: bool,
) -> Result<IndexMap<String, Value>, StoreError> {
    if page_url_id < 1 || document_id < 1 || !rendered.is_object() {
        return Err(invalid("browser artifact input is invalid"));
    }
    let policy = rendered
        .get("renderer")
        .and_then(|renderer| renderer.get("policy"));
    let flag = |key: &str| {
        policy
            .and_then(|policy| policy.get(key))
            .map(truthy)
            .unwrap_or(false)
    };
    let retention_blocked = flag("credentials_used") || flag("cache_control_no_store");
    let blocked_value = json!({"ok": false});
    let rendered: &Value = if retention_blocked {
        if let Some(staged) = rendered.get("screenshot_path").and_then(Value::as_str) {
            let candidate = Path::new(staged);
            if !is_symlink(candidate) && is_file(candidate) {
                let staged_root = resolve_lenient(&staging_dir(scan_path));
                let inside = fs::canonicalize(candidate)
                    .map(|real| real.ancestors().skip(1).any(|parent| parent == staged_root))
                    .unwrap_or(false);
                if inside {
                    fs::remove_file(candidate)?;
                }
            }
        }
        &blocked_value
    } else {
        rendered
    };
    let screenshot = if screenshots {
        move_screenshot(
            scan_path,
            rendered.get("screenshot_path").and_then(Value::as_str),
        )?
    } else {
        state_dict("disabled", "screenshot retention disabled", Value::Null)
    };
    let console = if console_errors {
        if rendered.get("ok") != Some(&Value::Bool(false)) {
            save_console(
                scan_path,
                rendered.get("console_errors").unwrap_or(&NULL),
                rendered.get("console_errors_omitted").unwrap_or(&NULL),
                rendered
                    .get("console_messages")
                    .filter(|value| !value.is_null()),
                rendered.get("console_messages_omitted").unwrap_or(&NULL),
            )?
        } else {
            state_dict(
                "unavailable",
                "render did not complete; console evidence is unavailable",
                Value::Null,
            )
        }
    } else {
        state_dict("disabled", "console retention disabled", Value::Null)
    };
    let screenshot_state = screenshot
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("");
    let console_state = console.get("state").and_then(Value::as_str).unwrap_or("");
    let states = [screenshot_state, console_state];
    let completeness = if states
        .iter()
        .all(|state| matches!(*state, "stored" | "disabled"))
    {
        "complete"
    } else if states
        .iter()
        .any(|state| matches!(*state, "stored" | "partial"))
    {
        "partial"
    } else {
        "unavailable"
    };
    let reason = joined_reasons(&screenshot, &console);
    let payload = json!({
        "schema_version": VERSION,
        "page_url_id": page_url_id,
        "document_id": document_id,
        "screenshot": Value::Object(screenshot.clone().into_iter().collect()),
        "console": Value::Object(console.clone().into_iter().collect()),
    });
    Ok(IndexMap::from([
        ("kind".to_string(), Value::String(KIND.to_string())),
        (
            "item_key".to_string(),
            Value::String(format!("page:{page_url_id}:document:{document_id}")),
        ),
        (
            "payload_version".to_string(),
            Value::String("scan_context.v1".to_string()),
        ),
        (
            "payload_json".to_string(),
            Value::String(py_dump(&payload, true)),
        ),
        (
            "completeness".to_string(),
            Value::String(completeness.to_string()),
        ),
        ("reason".to_string(), Value::String(reason)),
    ]))
}

/// The non-empty retention reasons of screenshot and console, joined into one text.
fn joined_reasons(
    screenshot: &IndexMap<String, Value>,
    console: &IndexMap<String, Value>,
) -> String {
    [screenshot, console]
        .iter()
        .filter_map(|part| part.get("reason").and_then(Value::as_str))
        .filter(|reason| !reason.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

/// Validate a stored `screenshot` reference: integers within bounds, sha, no reason.
fn screenshot_ref_valid(reference: &Value, reason: &str) -> bool {
    exact_keys(reference, &["sha256", "bytes", "width", "height"])
        && valid_sha(reference.get("sha256"))
        && int_in(reference.get("bytes"), 1, MAX_SCREENSHOT_BYTES)
        && int_in(reference.get("width"), 1, MAX_SCREENSHOT_WIDTH)
        && int_in(reference.get("height"), 1, MAX_SCREENSHOT_HEIGHT)
        && reference
            .get("width")
            .and_then(Value::as_i64)
            .zip(reference.get("height").and_then(Value::as_i64))
            .map(|(w, h)| w * h <= MAX_SCREENSHOT_PIXELS)
            .unwrap_or(false)
        && reason.is_empty()
}

/// Validate a stored `console` reference against its state and reason.
fn console_ref_valid(reference: &Value, state: &str, reason: &str) -> bool {
    if !exact_keys(reference, &["sha256", "captured", "omitted"])
        || !valid_sha(reference.get("sha256"))
    {
        return false;
    }
    let captured = reference.get("captured").and_then(Value::as_i64);
    let omitted = reference.get("omitted").and_then(Value::as_i64);
    match (captured, omitted) {
        (Some(captured), Some(omitted)) if captured >= 0 && omitted >= 0 => {
            if captured > MAX_CONSOLE_ERRORS {
                return false;
            }
            match state {
                "stored" => omitted == 0 && reason.is_empty(),
                "partial" => omitted >= 1 && !reason.is_empty(),
                _ => false,
            }
        }
        _ => false,
    }
}

/// Validate one persisted browser-artifact context against its page and document.
pub fn validate_context(
    con: &Connection,
    item: &IndexMap<String, Value>,
    payload: &Value,
) -> Result<(), StoreError> {
    let bad = || invalid("browser artifact context is invalid");
    if !exact_keys(
        payload,
        &[
            "schema_version",
            "page_url_id",
            "document_id",
            "screenshot",
            "console",
        ],
    ) || payload.get("schema_version").and_then(Value::as_str) != Some(VERSION)
    {
        return Err(bad());
    }
    let page = payload
        .get("page_url_id")
        .and_then(Value::as_i64)
        .filter(|n| *n >= 1);
    let document = payload
        .get("document_id")
        .and_then(Value::as_i64)
        .filter(|n| *n >= 1);
    let (Some(page), Some(document)) = (page, document) else {
        return Err(bad());
    };
    let expected_key = format!("page:{page}:document:{document}");
    if item.get("item_key").and_then(Value::as_str) != Some(expected_key.as_str()) {
        return Err(bad());
    }
    let bound = con
        .query_row(
            "SELECT 1 FROM documents WHERE document_id=?1 AND url_id=?2",
            params![document, page],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !bound {
        return Err(bad());
    }
    let screenshot = payload.get("screenshot").unwrap_or(&NULL);
    let console = payload.get("console").unwrap_or(&NULL);
    for part in [screenshot, console] {
        let state_ok = matches!(
            part.get("state").and_then(Value::as_str),
            Some("disabled" | "stored" | "partial" | "unavailable")
        );
        if !exact_keys(part, &["state", "reason", "ref"])
            || !state_ok
            || part.get("reason").and_then(Value::as_str).is_none()
        {
            return Err(invalid(
                "browser artifact context has an invalid retention state",
            ));
        }
    }
    let s_state = screenshot
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("");
    let s_reason = screenshot
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("");
    let s_ref = screenshot.get("ref").unwrap_or(&NULL);
    if s_state == "stored" {
        if !screenshot_ref_valid(s_ref, s_reason) {
            return Err(invalid("browser screenshot reference is invalid"));
        }
    } else if !s_ref.is_null() || s_reason.is_empty() {
        return Err(invalid(
            "unavailable browser screenshot must not carry a reference",
        ));
    }
    let c_state = console.get("state").and_then(Value::as_str).unwrap_or("");
    let c_reason = console.get("reason").and_then(Value::as_str).unwrap_or("");
    let c_ref = console.get("ref").unwrap_or(&NULL);
    if c_state == "stored" || c_state == "partial" {
        if !console_ref_valid(c_ref, c_state, c_reason) {
            return Err(invalid("browser console reference is invalid"));
        }
    } else if !c_ref.is_null() || c_reason.is_empty() {
        return Err(invalid(
            "unavailable browser console must not carry a reference",
        ));
    }
    let states = [s_state, c_state];
    let expected = if states
        .iter()
        .all(|state| matches!(*state, "stored" | "disabled"))
    {
        "complete"
    } else if states
        .iter()
        .any(|state| matches!(*state, "stored" | "partial"))
    {
        "partial"
    } else {
        "unavailable"
    };
    let expected_reason = [s_reason, c_reason]
        .iter()
        .filter(|reason| !reason.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("; ");
    let completeness = item.get("completeness").and_then(Value::as_str);
    let reason = item.get("reason").and_then(Value::as_str);
    if completeness != Some(expected) || reason != Some(expected_reason.as_str()) {
        return Err(invalid(
            "browser artifact context completeness disagrees with its evidence",
        ));
    }
    Ok(())
}

/// Run a query and collect the first text column of each row.
fn query_strings(
    con: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> StoreResult<Vec<String>> {
    let mut statement = con.prepare(sql)?;
    let rows = statement.query_map(params, |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Read and parse one content-addressed sidecar; missing or symlinked files are errors.
fn read_sidecar(path: &Path) -> Result<Value, StoreError> {
    if is_symlink(path) || !is_file(path) {
        return Err(invalid("sidecar is missing or a symbolic link"));
    }
    let text = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}

/// The retention-state dict of a page when no evidence can be read.
fn unavailable_console(reason: &str) -> IndexMap<String, Value> {
    IndexMap::from([
        (
            "state".to_string(),
            Value::String("unavailable".to_string()),
        ),
        ("reason".to_string(), Value::String(reason.to_string())),
        ("document_id".to_string(), Value::Null),
        ("messages".to_string(), Value::Array(Vec::new())),
        ("messages_truncated".to_string(), Value::Bool(false)),
    ])
}

/// One page's retained render console, read offline from its sidecar (#1475).
///
/// A page can carry more than one retained render (a probe and an accepted
/// full render); the highest document id is the render whose DOM the scan
/// kept selected. Missing, malformed or tampered evidence is unavailable by
/// name -- never an empty console presented as a clean one.
pub fn retained_console(
    con: &Connection,
    scan_path: &Path,
    url_id: i64,
) -> Result<IndexMap<String, Value>, StoreError> {
    let rows = query_strings(
        con,
        "SELECT payload_json FROM context_items WHERE kind=?1 AND item_key>=?2 AND item_key<?3",
        params![
            KIND,
            format!("page:{url_id}:"),
            format!("page:{}:", url_id + 1)
        ],
    )?;
    if rows.is_empty() {
        return Ok(unavailable_console(
            "no rendered browser artifact was retained for this URL",
        ));
    }
    let mut best: Option<(i64, Value)> = None;
    for raw in &rows {
        let Ok(payload) = serde_json::from_str::<Value>(raw) else {
            continue;
        };
        let (Some(document), Some(console)) = (payload.get("document_id"), payload.get("console"))
        else {
            continue;
        };
        let Some(document) = document.as_i64() else {
            continue;
        };
        if !console.is_object() || best.as_ref().is_some_and(|(kept, _)| document <= *kept) {
            continue;
        }
        best = Some((document, console.clone()));
    }
    let Some((document_id, console)) = best else {
        return Ok(unavailable_console(
            "retained browser artifact record could not be read",
        ));
    };
    let state_raw = console.get("state").and_then(Value::as_str).unwrap_or("");
    let state = match state_raw {
        "stored" | "partial" | "disabled" | "unavailable" => state_raw,
        _ => "unavailable",
    };
    let mut result = IndexMap::from([
        ("state".to_string(), Value::String(state.to_string())),
        (
            "reason".to_string(),
            Value::String(py_text(console.get("reason").unwrap_or(&NULL))),
        ),
        ("document_id".to_string(), Value::from(document_id)),
        ("messages".to_string(), Value::Array(Vec::new())),
        ("messages_truncated".to_string(), Value::Bool(false)),
    ]);
    if state != "stored" && state != "partial" {
        return Ok(result);
    }
    let digest = console
        .get("ref")
        .and_then(|reference| reference.get("sha256"))
        .and_then(Value::as_str);
    let Some(digest) = digest.filter(|digest| is_hex64(digest)) else {
        result.insert(
            "state".to_string(),
            Value::String("unavailable".to_string()),
        );
        result.insert(
            "reason".to_string(),
            Value::String("retained console reference is invalid".to_string()),
        );
        return Ok(result);
    };
    let sidecar = root(scan_path)
        .join("console")
        .join(format!("{digest}.json"));
    let body = match read_sidecar(&sidecar) {
        Ok(body) if body.is_object() => body,
        _ => {
            result.insert(
                "state".to_string(),
                Value::String("unavailable".to_string()),
            );
            result.insert(
                "reason".to_string(),
                Value::String("retained console sidecar could not be read".to_string()),
            );
            return Ok(result);
        }
    };
    let messages = match body.get("messages") {
        None | Some(Value::Null) => return Ok(result),
        Some(messages) => messages,
    };
    let items = match messages.as_array() {
        Some(items) if items.iter().all(Value::is_object) => items,
        _ => {
            result.insert(
                "state".to_string(),
                Value::String("unavailable".to_string()),
            );
            result.insert(
                "reason".to_string(),
                Value::String("retained console sidecar could not be read".to_string()),
            );
            return Ok(result);
        }
    };
    let truncated = body.get("messages_truncated").map(truthy).unwrap_or(false);
    let bounded = console_records(&Value::Array(items.clone()))
        .into_iter()
        .map(|record| Value::Object(record.into_iter().collect()))
        .collect::<Vec<_>>();
    result.insert("messages".to_string(), Value::Array(bounded));
    result.insert(
        "messages_truncated".to_string(),
        Value::Bool(truncated || items.len() as i64 > MAX_CONSOLE_MESSAGES),
    );
    Ok(result)
}

/// Outcome of reading one retained console row for the page summary.
enum ConsoleRow {
    /// Not a stored or partial console: not counted at all.
    Skipped,
    /// Present but unreadable: counted as unreadable, never as clean.
    Unreadable,
    /// Readable; carries the page entry when the console has errors or omissions.
    Readable(Option<Value>),
}

fn console_row(con: &Connection, scan_path: &Path, raw: &str) -> ConsoleRow {
    let Ok(payload) = serde_json::from_str::<Value>(raw) else {
        return ConsoleRow::Unreadable;
    };
    let Some(console) = payload.get("console").filter(|value| value.is_object()) else {
        return ConsoleRow::Unreadable;
    };
    let Some(state) = console.get("state").and_then(Value::as_str) else {
        return ConsoleRow::Unreadable;
    };
    let reference = console.get("ref").unwrap_or(&NULL);
    if !matches!(state, "stored" | "partial") || !reference.is_object() {
        return ConsoleRow::Skipped;
    }
    let Some(digest) = reference.get("sha256").and_then(Value::as_str) else {
        return ConsoleRow::Unreadable;
    };
    if !is_hex64(digest) {
        return ConsoleRow::Unreadable;
    }
    let sidecar = root(scan_path)
        .join("console")
        .join(format!("{digest}.json"));
    let errors: Option<Vec<String>> = read_sidecar(&sidecar).ok().and_then(|body| {
        body.get("errors")
            .and_then(Value::as_array)
            .filter(|items| items.iter().all(Value::is_string))
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect()
            })
    });
    let Some(errors) = errors else {
        return ConsoleRow::Unreadable;
    };
    let Some(document) = payload.get("document_id").and_then(Value::as_i64) else {
        return ConsoleRow::Unreadable;
    };
    let url: Option<String> = con
        .query_row(
            "SELECT urls.url FROM documents JOIN urls ON urls.url_id=documents.url_id \
             WHERE documents.document_id=?1",
            params![document],
            |row| row.get(0),
        )
        .optional()
        .ok()
        .flatten();
    let Some(url) = url else {
        return ConsoleRow::Unreadable;
    };
    let omitted = reference
        .get("omitted")
        .and_then(Value::as_i64)
        .filter(|n| *n >= 0)
        .unwrap_or(0);
    if errors.is_empty() && omitted == 0 {
        return ConsoleRow::Readable(None);
    }
    let shown: Vec<Value> = errors
        .iter()
        .take(5)
        .map(|error| Value::String(error.clone()))
        .collect();
    ConsoleRow::Readable(Some(json!({
        "target_url": url,
        "error_count": errors.len() as i64 + omitted,
        "errors": shown,
    })))
}

/// Pages whose retained render console recorded at least one error.
///
/// Reads the context items and their content-addressed console sidecars. Evidence
/// that is present but unreadable is counted, never treated as a clean page.
pub fn console_error_pages(
    con: &Connection,
    scan_path: &Path,
) -> Result<IndexMap<String, Value>, StoreError> {
    let rows = query_strings(
        con,
        "SELECT payload_json FROM context_items WHERE kind=?1 ORDER BY item_key",
        params![KIND],
    )?;
    let mut pages = Vec::new();
    let mut captured: i64 = 0;
    let mut unreadable: i64 = 0;
    for raw in &rows {
        match console_row(con, scan_path, raw) {
            ConsoleRow::Skipped => {}
            ConsoleRow::Unreadable => unreadable += 1,
            ConsoleRow::Readable(page) => {
                captured += 1;
                if let Some(page) = page {
                    pages.push(page);
                }
            }
        }
    }
    Ok(IndexMap::from([
        ("pages".to_string(), Value::Array(pages)),
        ("captured".to_string(), Value::from(captured)),
        ("unreadable".to_string(), Value::from(unreadable)),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_masks_secrets_and_urls() {
        // behaviour of browser_artifacts._redact
        assert_eq!(redact(&json!("token=abc123 done")), "[redacted] done");
        assert_eq!(
            redact(&json!("see https://x.test/a?b=1 now")),
            "see [url] now"
        );
        assert_eq!(redact(&Value::Null), "");
    }

    #[test]
    fn json_quoting_matches_reference_ascii_escapes() {
        // behaviour of json.dumps(ensure_ascii=True) used for the sidecar digest
        assert_eq!(py_quote("a\"b\n"), "\"a\\\"b\\n\"");
        assert_eq!(py_quote("é"), "\"\\u00e9\"");
        assert_eq!(py_quote("\u{7f}"), "\"\\u007f\"");
    }

    #[test]
    fn console_location_keeps_only_bounded_fields() {
        // behaviour of browser_artifacts._console_location
        let location =
            console_location(&json!({"url": "https://x.test/p", "line": 3, "column": "x"}));
        assert_eq!(location.get("line"), Some(&json!(3)));
        assert!(!location.contains_key("column"));
        assert!(location
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .contains("[url]"));
    }

    #[test]
    fn console_records_are_bounded() {
        // behaviour of browser_artifacts._console_records
        let messages = Value::Array(vec![json!({"type": "error", "text": "boom"}); 150]);
        assert_eq!(
            console_records(&messages).len() as i64,
            MAX_CONSOLE_MESSAGES
        );
        let first = &console_records(&json!([{"text": "x"}]))[0];
        assert_eq!(first.get("type"), Some(&json!("unknown")));
    }
}
